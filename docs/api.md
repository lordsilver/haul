# HTTP API

The engine serves its UI and this API on `http://127.0.0.1:8765` (`--port` or `HAUL_PORT`), on the loopback interface only. Browser integrations (a bookmarklet, a future extension) and scripts can use it.

Any web page you visit can send requests to loopback addresses, so the server guards itself:

- Every request must carry a `Host` of `127.0.0.1:<port>`, `localhost:<port>` or `[::1]:<port>`, which defeats DNS rebinding.
- A request that changes anything is refused when it carries an `Origin` other than the server's own. Browsers send `Origin` on every POST; scripts that send none are allowed.
- Pages are served with a Content Security Policy that forbids framing, so no other page can overlay the UI to steer clicks.

Bodies are JSON. Errors come back as `{"error": "…"}` with a 4xx or 5xx status.

## State

| Method and path   | Does                                                                                                                                                                                            |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /api/state`  | The whole state: `items`, `hosts`, `paused`, `speed` (bytes/s), `max_total`, `rate` (bytes/s cap, 0 = none), `proxy`, `theme`, `download_dir`, `data_dir`, `config_file`, `desktop`, `version`. |
| `GET /api/events` | Server-sent events: a `state` event with the same JSON right away, after every change (bursts merged) and at least once a second.                                                               |

An item has `id`, `name`, `package`, `status` (`checking`, `queued`, `resolving`, `downloading`, `waiting`, `paused`, `captcha`, `done`, `error`), `error`, `size` and `done` (bytes, 0 when unknown), `speed`, `percent` (from external downloaders), `host`, `mirrors` (host names), `wait` (seconds), `open_urls` (pages that need a person) and `path` (where a finished file went).

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

| Method and path                               | Body                                                  | Does                                                                                                           |
| --------------------------------------------- | ----------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `POST /api/host/{name}/limit/{n}`             |                                                       | Files downloaded from this host at once (1 to 32); saved to `config.toml`.                                     |
| `POST /api/host/{name}/route/{direct\|proxy}` |                                                       | The user's route choice for a host when a proxy is configured; saved to `routes.json`.                         |
| `POST /api/global/{action}`                   |                                                       | `pause` (also pauses running transfers), `resume`, `retry_failed`, `clear_done`.                               |
| `POST /api/settings`                          | `{"max_total": 8, "rate_mbps": 2.5, "theme": "dark"}` | Any subset; each is applied at once and saved to `config.toml`.                                                |
| `POST /api/open/{downloads\|data\|config}`    |                                                       | Opens the download folder, the data folder, or `config.toml` (created from a commented template when missing). |
