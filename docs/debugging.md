# Debug mode

Debug mode lets an agent (or a person) watch everything Haul does as it happens, look at what it holds inside, wait for something to happen, and look into a page Haul is passing out of sight. Use it to diagnose a problem or test a change, instead of reading `haul.log` afterwards or polling `/api/state`.

It is off unless asked for, because it shows page contents and links and can run a script in a page being passed.

## Turning it on

- In the app: **Settings**, **About**, **Live connection** (`live_debug = true` in `config.toml`). It goes on and off at once, no restart needed, so a diagnosing agent can follow the user's own Haul while they use it.
- For one run: `HAUL_DEBUG=1` in the environment (desktop app or server), or `haul --debug`. This holds whatever the setting says.

Haul then writes a fresh token to `debug-token` in its data folder, and takes it away when the connection goes off. Every `/api/debug/…` request must come from this machine (as every API request does) and send that token as `X-Haul-Debug`. While it is off, these routes answer 404.

```sh
TOKEN=$(cat "$DATA/debug-token")
curl -N -H "X-Haul-Debug: $TOKEN" "http://127.0.0.1:$PORT/api/debug/stream?replay=true"
```

## Signals

`GET /api/debug/stream` sends one JSON object per line, as things happen. It starts with a `hello` (`version`, `pid`, `desktop`, `data_dir`), then sends the last 2000 signals kept (with `?replay=true`), then live ones. Every signal has `t` (ISO 8601, UTC, to the millisecond) and `kind`:

| `kind`         | Fields                                                                                 | When                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| -------------- | -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `log`          | `level`, `target`, `message`, `fields`                                                 | Every event Haul logs, at every level (trace and debug included, whatever the detailed log is set to), with its fields (`item`, `url`, `host`…).                                                                                                                                                                                                                                                                                                                                                                                                        |
| `status`       | `row` (`item` or `wrapper`), `id`, `name`, `from`, `to`, `error`                       | A download or wrapper changes status; `to: "removed"` when it leaves the list. `from` is `null` for a new row.                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `pass`         | `event` (`start`, `page`, `press`, `refused`, `end`), `id`, `url`, …                   | The desktop app passing row `id`'s page out of sight: `start` with the browser's `profile` and `pid`; `page` with what the page's script `said` to Haul (links shown, a file handed over, settled…); `press` with the words of the download `button` it pressed on a file's page; `refused` with where the page was kept from going, off its site; `end` with the `outcome` (`listed`, `settled`, `files`, `gone`, `later`, `cleared` with the names of the cookies handed to Haul, `wants_person` with what the page showed, `closed`, or an `error`). |
| `page`         | `event` (`opened`, `loads`, `refused`, `sent back`, `popup`, `closed`), `id`, `url`, … | Row `id`'s page window in the desktop app (Open here, or a page passed in a window where there is no Edge): `opened` with whether it is `quiet` (out of sight) and how it `keep`s to its site (`Host`, `Wrapper`, `Free`); `loads` for each page it loads; `refused` with `why` for a navigation kept off it; `sent back` with where `to`; `popup` for a new window the page asked for, refused, and whether it loads `in_place`; `closed`.                                                                                                             |
| `notification` | `title`, `body`, `waiting`                                                             | The desktop app shows a system notification.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `lagged`       | `missed`                                                                               | The reader was too slow and missed that many signals.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| anything else  | the client's own                                                                       | `POST /api/debug/emit` with `{"kind": "note", …}`: a marker for a test's steps.                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |

`GET /api/debug/recent` answers the kept signals as one JSON array.

Every signal but `log` goes to `haul.log` too, debug mode or not (target `haul_core::signal`, the whole signal in its `signal` field): each row's status changes, `looking_up` for a file whose page is opened only to learn what it is, `removed` when it leaves the list, a run's rows at its start counted in one line, each pass's start, presses, refusals and end, each page window's opening, pages, refusals and closing (its page messages only with the detailed log), and each notification. Each run's first line is `Haul <version> starts`.

## The report

**Settings**, **About**, **Save report…** (or `GET /api/diagnostics`) writes one zip, laid out for whoever diagnoses it:

- `timeline.txt`: the last 4 MB of `haul.log` and of the log before it, oldest first, a line per event: `MM-DD HH:MM:SS.mmm L target [row name] message key=value…`, each run headed by `==== … Haul <version> starts (os, pid) ====`. Read it first.
- `report.json`, each fact once: `about` (version, system, data folder, whether the detailed log and the live connection are on, paused, speed, update), `settings` (no secrets: keys say only whether they are set), `counts`, `rows` (every download and wrapper in the queue's order: the engine's fields as `/api/debug/inspect` gives them, `view` with what the UI shows of it that those don't say, and its last events as `history` when the detailed log is off), `cards` (the Wrappers view's), `archives`, `hosts` (those this run dealt with or the settings name, each as the Hosts view shows it, `measured` with the engine's measures that it doesn't show, and its `lanes`, one per exit), `network` (proxies, pool, router, VPN services), `engine` (paused, moving, redialing, draining, pages behind checks) and `desktop` (its passes out of sight and windows). Under `view`, `measured` and `hosts`, a field left out is at its default (`null`, `false`, `0`, empty).
- `haul.log` and `haul.1.log`, whole; `pages/<site>-wants-person.png`, the last page of each site that wanted a person.

Links lose their `#…` part (mega.nz keys); a link a page handed back loses its query (tokens).

## Inside

`GET /api/debug/inspect` answers:

- `state`: what `/api/state` answers.
- `engine`: what the UI does not show.
  - `items`: each row's links (`mirrors`), the ones that failed, are gone or want a person, `page_wants_person`, `page_retrying` (its page is passed again on its own, telling nobody), `tentative`, `probed`, `running`, the exit it downloads through, the mirror its attempt goes to (`url`), what its speed says (`speed_verdict`), `slowed`, the mirrors that slowed it (`slow_on`), the host its partial file's bytes came from (`part_host`), retries, and the seconds until its next retry (`retry_in`) or try (`not_before_in`).
  - `hosts`: each host's limit, measured speed, what it gives when it isn't limiting the address (`normal`), whether a redial made for it didn't help (`redial_useless`), failures and current penalty.
  - `lanes`: each host from each exit, with the downloads it serves, the limit it showed (`cap`), the seconds left of its cooldown and settling, whether that cooldown is for slowing the address (`slowing`), and how many times in a row it did (`strikes`).
  - `guarded` (pages behind Cloudflare's check), `cleared` (those whose clearance a pass handed Haul), `pages_pass`, `paused`, `moving`, `reconnecting`, `draining` (why a redial waits for downloads to finish).
- `desktop` (desktop app only): `passes_out_of_sight`, each with its page, profile, browser `pid`, start, limit and the page's last message, and whether it is stopping or abandoned; the in-app browser windows open; the Edge or Chrome Haul found.

## Waiting

`POST /api/debug/wait` with `{"match": {…}, "timeout": 60, "recent": false}` answers with the first signal holding every field of `match`, or 408 after `timeout` seconds (600 at most). Strings match as substrings, and objects match the fields they name. With `"recent": true`, the kept signals count too.

```json
{ "match": { "kind": "status", "name": "part1", "to": "done" }, "timeout": 300 }
{ "match": { "kind": "pass", "event": "end", "url": "/guard/" } }
{ "match": { "kind": "log", "message": "took over" }, "recent": true }
```

One wait replaces a polling loop. Start it (in the background) before the action it waits for, or use `"recent": true`.

## Looking into a page being passed

While the desktop app passes item `id`'s page out of sight:

- `GET /api/debug/pass/{id}` answers what the page shows: `title`, `url`, `frames`, `turnstile` answers, `buttons`, its first 50 `links`, and a `screenshot_png_base64`.
- `POST /api/debug/pass/{id}` with `{"eval": "document.title"}` runs the script in the page and answers its value (a promise is awaited).
- `POST /api/debug/pass/{id}` with `{"click": ".some-checkbox"}` clicks the middle of the first element the selector finds, with the mouse events a person's click makes (DevTools input, which the page cannot tell from a person's), and answers where it clicked. A click from a page script is marked as not a person's, and some checks refuse it.
- `POST /api/debug/pass/{id}` with `{"cookies": true}` answers the browser's cookies for the page (those its scripts cannot read included, such as Cloudflare's `cf_clearance`) and its `user_agent`.

Both answer 409 when no page of that item is being passed, and 501 on the server, which passes no pages.

## Recipes

### The desktop app on Windows from WSL

WSL cannot reach Windows' loopback, but Windows' own `curl.exe` can, and it runs from WSL with its output coming back:

```sh
WINCURL=/mnt/c/Windows/System32/curl.exe
TOKEN=$(tr -d '\r' < /mnt/c/Users/$WINUSER/AppData/Local/Temp/haul-test/data/debug-token)
B=http://127.0.0.1:8791
$WINCURL -sN -H "X-Haul-Debug: $TOKEN" "$B/api/debug/stream" > stream.jsonl &   # follow everything
$WINCURL -s -H "X-Haul-Debug: $TOKEN" -H 'Content-Type: application/json' \
  -d '{"match":{"kind":"pass","event":"end"},"timeout":120}' "$B/api/debug/wait"
```

Run a test copy of the app with its own data folder and port (`HAUL_DATA_DIR`, `HAUL_PORT`, `HAUL_DEBUG=1`); never the user's own.

### Pages passed out of sight, without a real site

A page that answers `403` with `cf-mitigated: challenge` is passed in Edge like one behind Cloudflare. A small server in WSL is reachable from Windows at both `127.0.0.1` and `localhost`, which count as two sites. Serve pages that list nothing, swap their links after a delay, or keep a Turnstile field empty forever. Add them, then wait for each `pass` `end` and check its `outcome`.

### A test, step by step

1. Start the stream into a file, with `?replay=true`.
2. `POST /api/debug/emit` a `note` naming the step.
3. Act through the normal API (`/api/add`, `/api/item/{id}/…`).
4. `POST /api/debug/wait` for the signal that proves the step (a `status`, a `pass` `end`, a `log` message).
5. When something looks wrong, read `/api/debug/inspect` and, during a pass, `/api/debug/pass/{id}`.
6. Read the stream file for the whole story.
