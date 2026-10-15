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
for f in Season.1.Episode.01.mkv:300M ubuntu-26.04-desktop-amd64.iso:180M holiday-photos.zip:40M \
         photos-backup.part1.rar:25M Season.1.Episode.02.mkv:400M; do truncate -s ${f#*:} $D/files/cdn/${f%%:*}; done
printf '<title>Download project-files.7z</title>captcha' > $D/files/getfile.html
(cd $D/files && python3 -m http.server 8799 --bind 127.0.0.1 >/dev/null 2>&1 &)
cargo build -p haul-web && (HAUL_UI_DIR=$PWD/crates/haul-web/ui target/debug/haul --port 8766 --data-dir $D/data --dir $D/dl >$D/haul.log 2>&1 &)
B=http://127.0.0.1:8766 F=http://127.0.0.1:8799
curl -s -H 'Content-Type: application/json' -d '{"max_total":4,"rate_limit_mbps":6}' $B/api/settings
curl -s -H 'Content-Type: application/json' -d "{\"text\":\"$F/cdn/Season.1.Episode.01.mkv http://localhost:8799/cdn/Season.1.Episode.01.mkv $F/cdn/ubuntu-26.04-desktop-amd64.iso $F/cdn/holiday-photos.zip $F/getfile.html $F/cdn/missing-file.zip http://localhost:8799/cdn/photos-backup.part1.rar http://localhost:8799/cdn/Season.1.Episode.02.mkv\"}" $B/api/add
sleep 2; id=$(curl -s $B/api/state | jq -r '.items[] | select(.name=="Season.1.Episode.02.mkv") | .id'); curl -s -X POST $B/api/item/$id/pause
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

Check both themes for contrast and that nothing is clipped; the narrow shot must keep labels on buttons inside InfoBars.

Also capture the empty list: a second server with its own empty data folder shows what a new user starts from, and it is easy to miss because every demo has downloads in it. Look at icons inside buttons in particular: a descendant rule such as `.empty .icon` reaches into any button placed there and blows its icon up to the size of the illustration.

Drag states cannot be triggered in a headless screenshot. Copy `crates/haul-web/ui` to a scratch folder, append one line to the copy's `app.js` that forces the state, and point another server's `HAUL_UI_DIR` at the copy:

```sh
cp -r crates/haul-web/ui $D/ui-drag
echo '$("drop").hidden = false;' >> $D/ui-drag/app.js                # the window-wide drop zone
```

When done, stop the server and the file server and delete `$D`: the downloads are real bytes, on a disk that has run full.

## The desktop app and the in-app browser

Build `haul-desktop`, start `scripts/fake_host.py`, and run the app on Xvfb as `wsl-gui-check` describes, with `HAUL_DATA_DIR` and `HAUL_PORT` pointing at scratch values. Then:

```sh
B=http://127.0.0.1:8767
curl -s -H 'Content-Type: application/json' -d '{"text":"http://127.0.0.1:8798/captcha"}' $B/api/add
sleep 2; id=$(curl -s $B/api/state | jq -r '.items[] | select(.status=="captcha") | .id')
curl -s -H 'Content-Type: application/json' -H "Origin: $B" -d '{"user_agent":"Mozilla/5.0 TestAgent"}' $B/api/item/$id/browse
sleep 1; python3 ~/.agents/skills/wsl-gui-check/scripts/xwindows.py :98      # the browse window is mapped, titled "<host>: solve the page…"
sleep 4; curl -s $B/api/state | jq '.items[] | {name, status, path}'
```

Expect the item `done` as `solved-file.bin`, `took over the download` in `$HAUL_DATA_DIR/haul.log`, and in the fake host's log a final `GET /files/solved-file.bin` carrying `cookie=session=s3cret` and the `TestAgent` user agent: that request is the engine's, with the page's cookie handed over. The webview's own request just before it is the download that was cancelled.

On Linux the app renders with WebKitGTK and Linux fonts. WebView2 on Windows behaves like Chrome, so judge the look from the Chrome screenshots.
