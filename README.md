# Haul

A download manager for file hosts. Paste links, or drop a `.dlc` or `.txt` file, and Haul resolves each host's page into the actual file, merges mirrors of the same file across hosts, picks the fastest one, and downloads with resume, a shared speed limit and an optional proxy. When a host wants a person (a captcha, a timer, a click), the download waits for you instead of failing, and the desktop app's built-in browser hands the download back to Haul once you have dealt with the page.

It comes as a Windows desktop app (a Tauri 2 shell around the engine) and as `haul`, a headless server with the same web UI for any browser.

## Status

Early. Working today:

- Queue with mirrors: the same file name on several hosts becomes one row; links that hide the name are looked up first (name and size), so they merge before anything downloads.
- Per-host slots, cooldowns when a host answers 429 or 503, failover to the next mirror, and up to three automatic resumes of a broken transfer.
- Resume through HTTP ranges; partial files survive restarts and pauses.
- One global speed limit across every transfer; parallel downloads and the limit change live and are saved to `config.toml`.
- Proxy (SOCKS5 or HTTP, with credentials): every host goes through it unless you allow a direct route for that host. A 403 or 451 through the proxy flags the host instead of silently leaving the tunnel.
- Hosts: pixeldrain (files and lists), gofile, buzzheavier, 1fichier, rapidgator and nitroflare (premium accounts), mega.nz through `megatools`, and any direct link. [docs/hosts.md](docs/hosts.md) says how each one is handled and what has been verified against the real host.
- Import of JDownloader `.dlc` containers (through JDownloader's key service) and of any text file with links in it.
- Desktop app: native window with light and dark themes that follow Windows, tray icon (closing the window keeps downloading), notifications, single instance, `.dlc` file association, and the in-app browser for hosts that need a person.

## Install

Windows: download the installer from the [releases](https://github.com/garysassano/haul/releases) (`Haul_<version>_x64-setup.exe`, no administrator rights needed). It is not code-signed yet, so SmartScreen asks once: **More info**, then **Run anyway**. Windows 10 and 11 ship the WebView2 runtime the app needs.

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

- **Add links**: paste anywhere in the window (Ctrl+V), use **Add links**, drop links or `.dlc` / `.txt` files on the window, or pass them on the command line: `Haul.exe <link or file>…`. A second launch hands its links to the running app.
- **Needs you**: a host that wants a captcha or a browser step parks its download under **Needs you**. In the desktop app, **Open here** opens the page in Haul's own browser window: solve it and press the host's download button, and Haul takes the download over (with the page's cookies, through your proxy if the host uses it). Anywhere else, **Open page** and **Paste link** do the same by hand.
- **Hosts**: transfer speed per host, slots (files downloaded from it at once), and the proxy route. A host that looks blocked through the proxy offers **Allow direct**, which needs a second click because it reveals your real IP address to that host.
- **Headless**: `haul --port 8765 --dir D:\Downloads`, then open <http://127.0.0.1:8765/>. `haul --help` lists the options.
- **From any page**: this bookmarklet opens Haul's Add dialog with the page you are on (adding still takes your click): `javascript:location.href='http://127.0.0.1:8765/#/add/'+encodeURIComponent(location.href)`

## Configure

Settings live in `config.toml` in the data folder (**Settings › Settings file › Open** creates it with every key explained). The app writes the theme, parallel downloads, the speed limit and host slots back to it; restart Haul after editing anything else.

```toml
download_dir = "D:/Downloads"          # default: Downloads\Haul
max_total = 8                          # files at the same time
rate_limit_mbps = 0                    # MB/s, 0 = no limit
theme = "system"                       # system, light or dark
proxy = "socks5://user:pass@host:1080" # every host goes through it unless allowed direct

# Accounts
1fichier_key = ""
pixeldrain_key = ""
rapidgator_user = ""
rapidgator_pass = ""
nitroflare_user = ""
nitroflare_key = ""
gofile_token = ""

megatools = "C:/Tools/megatools/megatools.exe" # default: found on PATH

[limits]            # files at once per host (default 32)
"1fichier.com" = 1
```

Environment variables: `HAUL_PROXY` replaces `proxy`; `HAUL_1FICHIER_KEY`, `HAUL_PIXELDRAIN_KEY`, `HAUL_RAPIDGATOR_USER`, `HAUL_RAPIDGATOR_PASS`, `HAUL_NITROFLARE_USER`, `HAUL_NITROFLARE_KEY` and `HAUL_GOFILE_TOKEN` fill the account keys the file leaves unset; `HAUL_DATA_DIR` moves the data folder and `HAUL_PORT` the local port (default 8765).

## Files

The data folder is `%LOCALAPPDATA%\haul` on Windows and `~/.local/share/haul` on Linux. It holds `config.toml`, the queue (`queue.json`), per-host route choices (`routes.json`), the desktop app's log (`haul.log`), the in-app browser's profiles (`webview\`) and `haul.lock`, which stops a second Haul from loading and rewriting the same queue. Partial downloads sit in `.partial` inside the download folder until they finish.

## Coming from the Python prototype

Copy `queue.json` and `routes.json` into the data folder: Haul reads both formats as they are. The old `grabber.json` keys map one to one onto `config.toml`, and the `GRABBER_*` variables are now `HAUL_*`. To keep partial downloads, move the old `downloads\.partial` folder into the new download folder.
