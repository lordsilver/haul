---
name: haul-ui-check
description: Run Haul with a realistic demo queue in a private data folder and look at it — the web UI in both themes through Windows Chrome, and the desktop app (including the in-app browser hand-off) on a private Xvfb display. Use after changing anything in crates/haul-web/ui or crates/haul-desktop, or when asked how Haul looks or whether the desktop app works.
---

# Checking Haul's UI and desktop app

The generic techniques (Windows Chrome screenshots, keeping GTK apps off the user's desktop, stopping processes safely) are in the `wsl-gui-check` skill; read it first. Everything here runs in a scratch folder: never point Haul at the user's real data folder or queue.

## A demo queue for the web UI

Serve sparse files that look like real downloads, start the headless server on its own port and data folder, and give it a mix of states: downloading (a speed cap keeps them running), a mirror pair, done, needs you (a page), failed (404) and paused.

```sh
D=$SCRATCH/demo && mkdir -p $D/files/cdn $D/data
for f in lecture-01.mkv:300M ubuntu-26.04-desktop-amd64.iso:180M holiday-photos.zip:40M \
         photos-backup.part1.rar:25M lecture-02.mkv:400M; do truncate -s ${f#*:} $D/files/cdn/${f%%:*}; done
printf '<title>Download project-files.7z</title>captcha' > $D/files/getfile.html
(cd $D/files && python3 -m http.server 8799 --bind 127.0.0.1 >/dev/null 2>&1 &)
(cd crates/haul-web/ui && pnpm build) && cargo build -p haul-web && (HAUL_UI_DIR=$PWD/crates/haul-web/ui/dist target/debug/haul --port 8766 --data-dir $D/data --dir $D/dl >$D/haul.log 2>&1 &)
B=http://127.0.0.1:8766 F=http://127.0.0.1:8799
curl -s -H 'Content-Type: application/json' -d '{"max_total":4,"rate_limit_mbps":6}' $B/api/settings
curl -s -H 'Content-Type: application/json' -d "{\"text\":\"$F/cdn/lecture-01.mkv http://localhost:8799/cdn/lecture-01.mkv $F/cdn/ubuntu-26.04-desktop-amd64.iso $F/cdn/holiday-photos.zip $F/getfile.html $F/cdn/missing-file.zip http://localhost:8799/cdn/photos-backup.part1.rar http://localhost:8799/cdn/lecture-02.mkv\"}" $B/api/add
sleep 2; id=$(curl -s $B/api/state | jq -r '.items[] | select(.name=="lecture-02.mkv") | .id'); curl -s -X POST $B/api/item/$id/pause
```

Then capture each view in each theme with the `shot` function from `wsl-gui-check`, switching the theme through the API so the result does not depend on the Windows setting:

```sh
for t in light dark; do
  curl -s -H 'Content-Type: application/json' -d "{\"theme\":\"$t\"}" $B/api/settings
  shot $t-all "$B/#/all"; shot $t-hosts "$B/#/hosts"; shot $t-settings "$B/#/settings"
done
shot narrow "$B/#/attention" 720 700                      # compact navigation pane
shot dialog "$B/#/add/https%3A%2F%2Fpixeldrain.com%2Fu%2Fx"  # the Add dialog, filled in
```

The `shot` function passes `--hide-scrollbars`, which hides a stray horizontal scrollbar as well: capture each page once more without it, as the desktop app's WebView2 shows every scrollbar. Check both themes for contrast and that nothing is clipped; the narrow shot must keep labels on buttons inside InfoBars.

Also capture the empty list: a second server with its own empty data folder shows what a new user starts from, and it is easy to miss because every demo has downloads in it. Look at icons inside buttons in particular: a descendant rule such as `.empty .icon` reaches into any button placed there and blows its icon up to the size of the illustration.

Headless Chrome captures a dialog, menu or toast partway through its opening animation, so it looks see-through and slightly shrunk. To judge one, and to force states a headless screenshot cannot trigger (a drag over the window), copy the built UI to a scratch folder, change the copy, and point another server's `HAUL_UI_DIR` at it:

```sh
cp -r crates/haul-web/ui/dist $D/ui-still
echo '*,*::before,*::after{animation:none!important;transition:none!important}' >> $D/ui-still/assets/index.css
```

The README shows `docs/images/haul-light.png` and `haul-dark.png`: the light and dark `all` shots at 1280×760. Replace them whenever a change alters how the list looks.

When done, stop the server and the file server and delete `$D`: the downloads are real bytes, on a disk that has run full.

## The desktop app and the in-app browser

Serve a stand-in host page from the scratch folder: a page titled like a captcha that lists no links, which sets a cookie and then goes to its file, as a host's download button would:

```sh
H=$SCRATCH/host && mkdir -p $H/files && truncate -s 2M $H/files/solved-file.bin
cat > $H/captcha.html <<'HTML'
<title>Verify you are human</title><p>Solve the captcha to download.</p>
<script>document.cookie = "session=s3cret; path=/"; setTimeout(() => { location.href = "/files/solved-file.bin"; }, 2000);</script>
HTML
(cd $H && python3 -m http.server 8798 --bind 127.0.0.1 >$H/host.log 2>&1 &)
```

Build `haul-desktop` and run the app on Xvfb as `wsl-gui-check` describes, with `HAUL_DATA_DIR` and `HAUL_PORT` pointing at scratch values. Then:

```sh
B=http://127.0.0.1:8767
curl -s -H 'Content-Type: application/json' -d '{"text":"http://127.0.0.1:8798/captcha.html"}' $B/api/add
sleep 2; id=$(curl -s $B/api/state | jq -r '.items[] | select(.status=="captcha") | .id')
curl -s -H 'Content-Type: application/json' -H "Origin: $B" -d '{"user_agent":"Mozilla/5.0 TestAgent"}' $B/api/item/$id/browse
sleep 1; python3 ~/.agents/skills/wsl-gui-check/scripts/xwindows.py :98      # the browse window is mapped, titled "<host>: solve the page…"
sleep 4; curl -s $B/api/state | jq '.items[] | {name, status, path}'
```

Expect the item `done` as `solved-file.bin`, `took over the download` in `$HAUL_DATA_DIR/haul.log`, and two `GET /files/solved-file.bin` in `$H/host.log`: the webview's own, the download that was cancelled, then the engine's. `python3 -m http.server` logs no headers, so this does not show the page's cookie or the `TestAgent` user agent going with the engine's request. Stop the file server and delete `$H` when done.

On Linux the app renders with WebKitGTK and Linux fonts. WebView2 on Windows behaves like Chrome, so judge the look from the Chrome screenshots.
