# Haul

A download manager for file hosts. Paste links, or drop a `.dlc` or `.txt` file, and Haul resolves each host's page into the actual file, merges mirrors of the same file across hosts, picks the host you prefer or the fastest one, and downloads with resume, a shared speed limit and an optional proxy. When a host wants a person (a captcha, a timer, a click), the download waits for you instead of failing, and the desktop app's built-in browser hands the download back to Haul once you have dealt with the page.

It comes as a Windows desktop app (a Tauri 2 shell around the engine) and as `haul`, a headless server with the same web UI for any browser.

## Status

Early. Working today:

- Queue with mirrors: the same file name on several hosts becomes one row; links that hide the name are looked up first (name and size), so they merge before anything downloads.
- Preferred hosts: a file with mirrors downloads from the hosts you star, in that order, and otherwise from the fastest; speeds come from earlier downloads and are remembered for a week.
- Files already downloaded are recognised before anything is fetched: the same name and size in the download folder, and the same checksum where the host publishes one (pixeldrain, gofile, and servers that send digest headers). The row finishes as **Already downloaded**; a different file by that name is saved as `name (1).ext`.
- Per-host slots, cooldowns when a host answers 429 or 503, and failover to the next mirror. Dropped connections and unreachable hosts are retried with growing pauses before a mirror counts as failed, a host's failures weigh less as they age, and failed downloads are tried again on their own after a while.
- Resume through HTTP ranges; partial files survive restarts and pauses, and move along when you change the download folder.
- One global speed limit across every transfer, parallel downloads and per-host slots, all changeable while downloads run.
- Every setting can be changed in the app or in `config.toml`; each is written to the other, and edits to the file apply within a second.
- Clipboard watching (desktop app): links you copy anywhere are added when they point at a supported host or a file.
- Optional per-host subfolders of the download folder, and an option to move a finished download's file to the Recycle Bin when you remove it from the list.
- Proxy (SOCKS5 or HTTP, with credentials): every host goes through it unless you allow a direct route for that host. A 403 or 451 through the proxy flags the host instead of silently leaving the tunnel.
- Hosts: pixeldrain (files and lists), gofile, buzzheavier, 1fichier, rapidgator and nitroflare (premium accounts), mega.nz through `megatools`, and any direct link. [docs/hosts.md](docs/hosts.md) says how each one is handled and what has been verified against the real host.
- Import of JDownloader `.dlc` containers (through JDownloader's key service) and of any text file with links in it.
- Desktop app: native window with light and dark themes that follow Windows, tray icon (closing the window keeps downloading), notifications, single instance, `.dlc` file association, and the in-app browser for hosts that need a person.

## Install

Windows: download the installer from the [releases](https://github.com/garysassano/haul/releases) (`Haul_<version>_x64-setup.exe`). It asks whether to install for you alone, in `%LOCALAPPDATA%\Haul` without administrator rights, or for everyone on the computer, in Program Files; on an administrator account Windows asks for permission as it starts. For a silent install, pass `/S` with `/CurrentUser` or `/AllUsers`. The `.msi` always installs for everyone. The installer is not code-signed yet, so SmartScreen asks once: **More info**, then **Run anyway**. Windows 10 and 11 ship the WebView2 runtime the app needs.

From source, with Rust stable:

```sh
# Headless server with the web UI
cargo install --locked --git https://github.com/garysassano/haul haul-web

# Desktop app (on Linux it needs WebKitGTK 4.1, see .github/workflows/ci.yml)
git clone https://github.com/garysassano/haul && cd haul
cargo build --release -p haul-desktop        # target/release/haul-desktop(.exe)
cd crates/haul-desktop && npx @tauri-apps/cli@2 build   # installers in target/release/bundle
```

## Use

- **Add links**: paste anywhere in the window (Ctrl+V), use **Add links**, drop links or `.dlc` / `.txt` files on the window, or pass them on the command line: `Haul.exe <link or file>…`. A second launch hands its links to the running app. Each way fills the Add dialog, so nothing downloads until you click **Add**; **Settings › Add dropped links and files right away** skips the dialog for dropped and opened ones.
- **Needs you**: a host that wants a captcha or a browser step parks its download under **Needs you**. In the desktop app, **Open here** opens the page in Haul's own browser window: solve it and press the host's download button, and Haul takes the download over (with the page's cookies, through your proxy if the host uses it). Anywhere else, **Open page** and **Paste link** do the same by hand.
- **Hosts**: transfer speed per host, the star that makes it preferred, slots (files downloaded from it at once), the folder its files go to (a subfolder of the download folder), and the proxy route. A host that looks blocked through the proxy offers **Allow direct**, which needs a second click because it reveals your real IP address to that host.
- **Removing**: the ✕ on a row takes the download off the list and leaves its file alone, unless **Settings › Move removed files to the Recycle Bin** is on; then a finished download's file goes to the Recycle Bin. **Clear completed** only tidies the list.
- **Headless**: `haul --port 8765 --dir D:\Downloads`, then open <http://127.0.0.1:8765/>. `haul --help` lists the options.
- **From any page**: this bookmarklet opens Haul's Add dialog with the page you are on (adding still takes your click): `javascript:location.href='http://127.0.0.1:8765/#/add/'+encodeURIComponent(location.href)`

## Configure

Everything is under **Settings**, and everything there is stored in `config.toml` in the data folder: the app writes each change to the file, and changes you save in the file apply within a second. If the file stops parsing, the previous settings stay in effect and the app shows the error until it is fixed. **Settings › Settings file › Open** creates the file with every key explained:

```toml
download_dir = "D:/Downloads"          # default: Downloads\Haul
max_total = 8                          # files at the same time
rate_limit_mbps = 0                    # MB/s, 0 = no limit
theme = "system"                       # system, light or dark
watch_clipboard = false                # desktop app
recycle_on_remove = false              # removing a finished download also moves its file to the Recycle Bin
resume_attempts = 3                    # retries per mirror after network trouble
retry_failed_minutes = 15              # retry failed downloads (up to 3 times), 0 = never
proxy = "socks5://user:pass@host:1080" # every host goes through it unless allowed direct
preferred_hosts = ["rapidgator.net"]     # downloaded from first, in this order

# Accounts
1fichier_key = ""
pixeldrain_key = ""
rapidgator_user = ""
rapidgator_pass = ""
nitroflare_user = ""
nitroflare_key = ""
gofile_token = ""

megatools = "C:/Tools/megatools/megatools.exe" # default: found on PATH
user_agent = "..."                             # default: Haul's own

[limits]            # files at once per host (default 32)
"1fichier.com" = 1

[routes]            # hosts allowed off the proxy
"example.com" = "direct"

[folders]           # subfolders of the download folder per host
"pixeldrain.com" = "pixeldrain"
```

Environment variables: `HAUL_PROXY` replaces `proxy`; `HAUL_1FICHIER_KEY`, `HAUL_PIXELDRAIN_KEY`, `HAUL_RAPIDGATOR_USER`, `HAUL_RAPIDGATOR_PASS`, `HAUL_NITROFLARE_USER`, `HAUL_NITROFLARE_KEY` and `HAUL_GOFILE_TOKEN` fill the account keys the file leaves unset. Settings shows those as set by the environment and does not change them. `HAUL_DATA_DIR` moves the data folder and `HAUL_PORT` the local port (default 8765).

## Files

The data folder is `%LOCALAPPDATA%\io.github.garysassano.haul` on Windows, which the uninstaller deletes when you ask it to remove the app's data, and `~/.local/share/haul` on Linux. Versions up to 0.3 kept it in `%LOCALAPPDATA%\Haul`, next to the program; 0.4 moves it over the first time it starts. It holds `config.toml`, the queue (`queue.json`), what Haul measured per host (`hosts.json`), the desktop app's log (`haul.log`), the in-app browser's profiles (`webview\`) and `haul.lock`, which stops a second Haul from loading and rewriting the same queue. Partial downloads sit in `.partial` inside the download folder until they finish.

## Coming from the Python prototype

Copy `queue.json` and `routes.json` into the data folder: Haul reads the queue as it is and moves the route choices into `config.toml`. The old `grabber.json` keys map one to one onto `config.toml`, and the `GRABBER_*` variables are now `HAUL_*`. To keep partial downloads, move the old `downloads\.partial` folder into the new download folder.
