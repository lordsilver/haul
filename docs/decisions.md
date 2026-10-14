# Decisions

Design decisions and why they were made. Newest last.

## Rust rewrite of a Python prototype

The first version was a Python (aiohttp) script with a web UI. Its logic (mirror grouping, probing names before downloading, per-host slots and cooldowns, the shared speed limit, proxy routing with explicit direct approval, the manual hand-off for captchas) was tested and kept; Haul ports it to Rust and fixes what the prototype got wrong:

- The queue file sat next to the script, so a second copy loaded and rewrote the user's real queue. State now lives in a per-user data folder, an exclusive lock refuses a second instance on the same folder, and saves go through a temporary file and a rename.
- Pasting the same link twice created two rows; a folder that resolved to nothing vanished silently; a mirror merged into a folder row reused the folder's resolved link on every mirror; a 404 or a 429 from the resolver was reported as "needs a browser"; a broken connection failed the mirror for good; "Pause all" let running transfers carry on. Each is fixed and covered by a test.
- The local API accepted requests from any web page, including the one that switches a host off the proxy. See "Local API" below.

## Tauri 2, not the Tauri 3 alpha, and not Electron

When the project started (October 2026), Tauri 3 was at `3.0.0-alpha.4`; there was no beta. Its changes so far are plugin and Linux runtime refactors that this app does not need, while its plugins and documentation were alpha too. Tauri 2.12 is current and stable; the shell is small enough that moving to 3 when it ships is a short change.

On Windows, Tauri renders with WebView2, which is the same Chromium engine Electron bundles, so the page performs the same; Tauri leaves out the bundled Chromium and Node.js, which makes for a small installer, and its webview reports downloads, which the in-app browser needs. Reports of Electron outperforming Tauri mostly come from Linux, where Tauri uses WebKitGTK, or from apps that push a lot of data through Tauri's IPC; Haul's UI does not use the IPC at all.

## One UI, served by the engine

The web UI is plain files served by the engine's local HTTP server, and the desktop window simply loads that page. The same UI then works headless in any browser (or from another machine's browser through an SSH tunnel), the browser-facing API is the one the UI uses, and the desktop shell stays a few hundred lines. The shell adds only what a page cannot do itself: the tray, notifications, single-instance handling, file associations and the in-app browser, the latter reached through one API route.

The page is vanilla JavaScript modules with no build step, so `cargo build` produces everything. Rows and cards are updated in place rather than re-rendered, so state pushed every second never wipes what the user is typing.

## Windows 11 look

The UI follows Fluent design as Windows 11 apps do: navigation pane, command bar, cards in the style of the Settings app, InfoBars, WinUI 3's light and dark theme colors, Fluent System Icons (MIT), Segoe UI Variable. The theme follows Windows unless the user picks one; the choice is saved in `config.toml`, so the window frame can match it from the first paint.

Each thing that needs attention is shown once. A download that needs you is flagged by its own row, and its count rides on **Needs you** in the navigation pane (the desktop app also sends a notification); the InfoBars above the list are kept for state that affects every list: paused downloads, a settings file with an error, a lost connection. An InfoBar inside a row leaves its icon out, as WinUI's `IsIconVisible` allows, because the row's icon already says it.

## Local API

Loopback-only servers are still reachable from any page the user visits. The API therefore checks `Host` (against DNS rebinding) and refuses state-changing requests whose `Origin` is not its own, and the UI forbids framing. There are no tokens to manage, and scripts without an `Origin` keep working.

## One settings file, two editors

Every setting can be changed in the app, and every setting lives in `config.toml`: the app writes each change into the file with `toml_edit`, which keeps the user's comments and layout (a cleared value is commented out, not deleted, so its explanation stays), and it reloads the file within a second of an edit on disk. A file that does not parse leaves the previous settings in effect and the app shows the error until it is fixed. There is never a second store that silently overrides the file; per-host route choices moved there from `routes.json` for that reason.

Account secrets are part of the settings, but they never ride the state stream that every open view receives: the settings view fetches them separately when it opens. Values an environment variable sets are shown as such and cannot change in the app, since the file would not win anyway.

## Retries

Errors are sorted by what retrying can achieve. A dead link or a refusal fails its mirror at once, so the next mirror takes over without delay. Network trouble (a dropped connection, a timeout, an unreachable host) is retried on the same mirror with growing pauses, because the host usually comes back and the bytes already downloaded are kept. Failures lower a host's rank with a ten-minute half-life, so one bad minute does not exile a host for the rest of the session. A download whose mirrors all failed is tried again after a while, three times at most, since hosts that are down tend to come back and the cost of another attempt is small.

## Choosing among mirrors

A file with mirrors downloads from a host the user prefers when one has it, in the order the user starred them; otherwise from the host with the best measured speed, where recent failures count against a host and a host never measured gets a turn. A preferred host that is only busy is waited for, since a user who stars a host (often one they hold an account with) wants the file from it; one that is cooling down or has failed for the file is passed over, so a preference never stalls a download for good.

Speeds come from real downloads and are remembered in `hosts.json` for a week. Haul does not test mirrors by downloading a piece of each: file hosts count every download start toward free-mode limits and wait timers, so a test can cost the real download. Nor does it switch mirrors in the middle of a file: without a checksum there is no telling whether two mirrors hold the same bytes, and resuming one from the other's offset would corrupt the file silently.

## Removing downloads and host folders

Removing a download from the list leaves its file alone unless the user turns on `recycle_on_remove`; then the file goes to the Recycle Bin rather than being deleted, so a slip can be undone. Only the remove button of a finished download does that: **Clear completed** never touches files, because one click there would empty a folder of finished work into the bin. A file that cannot be moved (open in another program, say) keeps its row, so the user sees why and can try again. Files megatools downloads are left alone, since Haul only knows the folder it shares with other downloads.

A host's folder is decided when a file finishes, by the host that delivered it: a file merged from several mirrors goes where the mirror that won sends it, and a changed folder applies to downloads already running. Folder names are cleaned like file names and cannot climb out of the download folder.

## The data folder on Windows

The installer puts Haul in `%LOCALAPPDATA%\Haul` for the current user, and up to 0.3 the data folder was `%LOCALAPPDATA%\haul`: the same folder, since Windows ignores case. The queue and settings sat next to the program, and the uninstaller's option to delete the app's data, which removes `%LOCALAPPDATA%\<identifier>`, missed them. The data folder is now `%LOCALAPPDATA%\io.github.garysassano.haul`, where WebView2 already keeps the app's browser data. At start, Haul moves its own files from the old folder (never the program) unless the new folder has a queue or settings already, or an older Haul still holds the old one. On Linux nothing shares `~/.local/share/haul`, so it stays.

## People, not bypasses

Haul uses public APIs, guest tokens and the user's own accounts. It does not solve captchas, skip wait timers or rotate addresses to evade host limits. When a host needs a person, the download waits for one; the desktop app makes that step short by catching the download the user starts in its own browser window. That window goes through the same proxy as the downloads (by way of a local, credential-free SOCKS5 bridge, because browser engines cannot pass proxy credentials), so opening a page does not reveal the real address that the proxy hides.
