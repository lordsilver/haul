# HTTP API

The engine serves its UI and this API on `http://127.0.0.1:8765` (`--port` or `HAUL_PORT`), on the loopback interface only. Browser integrations (a bookmarklet, a future extension) and scripts can use it.

Any web page you visit can send requests to loopback addresses, so the server guards itself:

- Every request must carry a `Host` of `127.0.0.1:<port>`, `localhost:<port>` or `[::1]:<port>`, which defeats DNS rebinding.
- A request that changes anything is refused when it carries an `Origin` other than the server's own. Browsers send `Origin` on every POST; scripts that send none are allowed.
- Pages are served with a Content Security Policy that forbids framing, so no other page can overlay the UI to steer clicks.

Bodies are JSON. Errors come back as `{"error": "…"}` with a 4xx or 5xx status.

## State

| Method and path   | Does                                                                                                                                                                                                                                                                                                                                                               |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `GET /api/state`  | The whole state: `items`, `hosts`, `paused`, `speed` (bytes/s), `max_total`, `rate` (bytes/s cap, 0 = none), `proxy`, `theme`, `watch_clipboard`, `config_version` (moves when a setting changes), `config_error` (why the settings file is not in effect), `download_dir`, `data_dir`, `config_file`, `desktop`, `version`. Account secrets are never part of it. |
| `GET /api/events` | Server-sent events: a `state` event with the same JSON right away, after every change (bursts merged) and at least once a second.                                                                                                                                                                                                                                  |

An item has `id`, `name`, `package`, `status` (`checking`, `queued`, `resolving`, `downloading`, `waiting`, `paused`, `captcha`, `done`, `error`), `error`, `size` and `done` (bytes, 0 when unknown), `speed`, `percent` (from external downloaders), `host`, `mirrors` (host names), `wait` (seconds), `retry_in` (seconds until a failed item is tried again on its own, 0 when never), `open_urls` (pages that need a person) and `path` (where a finished file went).

## Adding

| Method and path       | Body                            | Does                                                                                                                           |
| --------------------- | ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| `POST /api/add`       | `{"text": "…", "package": "…"}` | Adds every http(s) link in `text`; `package` is an optional subfolder. Answers `{"added", "merged", "duplicates", "invalid"}`. |
| `POST /api/container` | multipart, one or more files    | Imports `.dlc` containers and text files with links. Same answer as `/api/add`.                                                |

The UI also opens its Add dialog, filled in, at `/#/add/<url-encoded links>`; nothing is added until the user confirms.

## Items

| Method and path                | Body                                        | Does                                                                                                                                                      |
| ------------------------------ | ------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `POST /api/item/{id}/{action}` |                                             | `pause`, `resume`, `retry` (all mirrors again), `retry_manual` (ask the hosts that wanted a person again), `remove` (also deletes its partial file).      |
| `POST /api/item/{id}/direct`   | `{"url": "…", "headers": {…}, "name": "…"}` | Downloads this item from a link the user supplies, with optional request headers (cookies, referer) and a name for rows that have none.                   |
| `POST /api/item/{id}/browse`   | `{"user_agent": "…"}`                       | Desktop app only (501 elsewhere): opens the item's page in the in-app browser, which hands the download back with the page's cookies and this user agent. |
| `POST /api/item/{id}/reveal`   |                                             | Shows the finished file in the file manager.                                                                                                              |

## Hosts and settings

| Method and path                               | Body | Does                                                                                                           |
| --------------------------------------------- | ---- | -------------------------------------------------------------------------------------------------------------- |
| `POST /api/host/{name}/limit/{n}`             |      | Files downloaded from this host at once (1 to 32); saved under `[limits]` in `config.toml`.                    |
| `POST /api/host/{name}/route/{direct\|proxy}` |      | The user's route choice for a host when a proxy is configured; saved under `[routes]` in `config.toml`.        |
| `POST /api/global/{action}`                   |      | `pause` (also pauses running transfers), `resume`, `retry_failed`, `clear_done`.                               |
| `POST /api/open/{downloads\|data\|config}`    |      | Opens the download folder, the data folder, or `config.toml` (created from a commented template when missing). |

## Settings

Settings use the names of `config.toml`; each change is checked, written to the file and applied at once.

| Method and path                        | Body                                                     | Does                                                                                                                                                                                                                                                                                                                  |
| -------------------------------------- | -------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /api/settings`                    |                                                          | Every setting, account secrets included, plus `from_env` (keys an environment variable sets, which cannot change here), `download_dir_fixed`, `config_file` and `error`.                                                                                                                                              |
| `POST /api/settings`                   | `{"max_total": 8, "theme": "dark", "1fichier_key": "…"}` | Any subset of `download_dir`, `max_total`, `rate_limit_mbps`, `theme`, `watch_clipboard`, `resume_attempts`, `retry_failed_minutes`, `proxy`, `user_agent`, `megatools` and the account keys. An empty text removes the key, which brings back its default. Answers 400 with the reason when a value is not accepted. |
| `POST /api/settings/download-dir/pick` |                                                          | Desktop app only (501 elsewhere): asks for a folder in the system's dialog and moves the downloads there, unfinished ones included. Answers `{"cancelled": true}` when the user cancels.                                                                                                                              |
