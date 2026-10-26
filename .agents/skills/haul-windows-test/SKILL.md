---
name: haul-windows-test
description: Test Haul's desktop app on this machine's Windows from WSL — get a branch's Windows build from CI, run a test copy in debug mode with its own data folder and port, follow every signal live through Windows' curl.exe, and drive page passes against local stand-in pages instead of real sites. Use after changing crates/haul-desktop (page passes, Edge, notifications, the in-app browser) or anything only Windows shows, and whenever a change needs checking in the real app rather than in tests.
---

# Testing the desktop app on Windows

Debug mode (`docs/debugging.md`) is the way in: a live stream of every signal, the insides, waits instead of polling, and a look into a page being passed. Read that page first.

Never touch the user's own Haul (`%LOCALAPPDATA%\Haul`, its data folder or its queue): the test copy lives in `%TEMP%\haul-test` with a data folder and port of its own, and is stopped by its command line.

## A Windows build of a branch

The local `cargo xwin` build fails in unrar's C++ code, so build on CI (about 15 minutes) and take `Haul.exe` out of the setup:

```sh
~/.local/bin/gh workflow run release.yml --ref "$BRANCH"
id=$(~/.local/bin/gh run list --workflow release.yml --branch "$BRANCH" --limit 1 --json databaseId --jq '.[0].databaseId')
# wait for it (a Monitor until-loop on `gh run view $id --json status`), then:
D=$SCRATCH/build && rm -rf "$D" && mkdir -p "$D/x"
~/.local/bin/gh run download "$id" --dir "$D"
7z e -y -o"$D/x" "$D"/windows-installers/*setup.exe Haul.exe
T=/mnt/c/Users/$WINUSER/AppData/Local/Temp/haul-test && mkdir -p "$T" && cp "$D/x/Haul.exe" "$T/" && rm -rf "$D"
```

## Start, follow, act, stop

```sh
W="C:\\Users\\$WINUSER\\AppData\\Local\\Temp\\haul-test"
powershell.exe -NoProfile -Command "\$env:HAUL_DATA_DIR='$W\\data'; \$env:HAUL_PORT='8791'; \$env:HAUL_DEBUG='1'; Start-Process -FilePath '$W\\Haul.exe'"
WINCURL=/mnt/c/Windows/System32/curl.exe B=http://127.0.0.1:8791
TOKEN=$(tr -d '\r' < "$T/data/debug-token")
dbg() { "$WINCURL" -s -H "X-Haul-Debug: $TOKEN" -H 'Content-Type: application/json' "$@"; }
dbg -N "$B/api/debug/stream?replay=true" > "$SCRATCH/stream.jsonl" &      # everything, as it happens
dbg -d '{"text":"http://127.0.0.1:8799/guard/swap"}' "$B/api/add"
dbg -d '{"match":{"kind":"pass","event":"end"},"timeout":120}' "$B/api/debug/wait"
dbg "$B/api/debug/inspect" | jq '.desktop, .engine.items'
powershell.exe -NoProfile -Command "Get-CimInstance Win32_Process | Where-Object { \$_.CommandLine -like '*haul-test*' } | ForEach-Object { Stop-Process -Id \$_.ProcessId -Force -ErrorAction SilentlyContinue }"
```

- WSL cannot reach Windows' loopback; Windows' own `curl.exe`, started from WSL, can, and its output comes back to WSL. PowerShell's `Invoke-RestMethod` cannot stream.
- Start a fresh data folder (`rm -rf "$T/data"`) for a first-run test; the Edge profiles under `data/webview/chromium/<site>` carry Cloudflare clearances between runs.
- A link already in the list is a duplicate, query string or not: give each try a fresh address (`/guard/stuck-2`).
- The Edge processes of the test copy carry `haul-test` in their command line; count them with `Get-CimInstance Win32_Process -Filter "Name='msedge.exe'"`.

## Pages passed out of sight, without real sites

`scripts/standin.py` serves pages that answer like Cloudflare's check (`403`, `cf-mitigated: challenge`), so the desktop app passes them in Edge:

```sh
python3 -I .agents/skills/haul-windows-test/scripts/standin.py "$SCRATCH/standin" 8799 2> "$SCRATCH/standin.log" &
```

| Page           | What it does                                                         | Expect (`pass` `end` `outcome`)                         |
| -------------- | -------------------------------------------------------------------- | ------------------------------------------------------- |
| `/guard/empty` | passes and lists nothing                                             | `settled`, then the row under Needs you (a paste fails) |
| `/guard/swap`  | its two links become two files on `localhost` after 4 s (same count) | `listed`, and `Ex.part1.rar`, `Ex.part2.rar` paused   |
| `/guard/stuck` | a Turnstile field that never fills                                   | `wants_person` after the pass's time                    |

Windows reaches the server at `127.0.0.1:8799` and `localhost:8799`, which Haul counts as two sites. Add `-<anything>` to a page's path for a fresh address. Stop the server with `pgrep -f '^python3 -I .*standin.py'` and `kill`, and delete its folder.

Pastes and real hosts are recognised by their site, so they cannot be stood in for; test those only on the user's own links, with their say-so.
