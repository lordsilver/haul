# Haul

A download manager for file hosts. Paste links, or drop a `.dlc` or `.txt` file, and Haul resolves each host's page into the actual file, merges mirrors of the same file across hosts, picks the host you prefer or the fastest one, and downloads with resume, a shared speed limit and an optional proxy. When a host page requires browser interaction, the download waits under **Needs you**, and the desktop app's built-in browser hands it back to Haul once you have dealt with the page.

It comes as a Windows desktop app (a Tauri 2 shell around the engine) and as `haul`, a headless server with the same web UI for any browser.

## Status

Early. Working today:

- Mirrors: the same file on several hosts becomes one row, downloaded from the hosts you star or else the fastest, with failover to the next mirror.
- Resume through HTTP ranges; partial files survive restarts and pauses.
- Files already in the download folder are recognised and not fetched again, optionally after comparing the host's checksum.
- Automatic retries of network trouble, of rate-limited hosts and of failed downloads.
- One global speed limit, parallel downloads and per-host slots, all changeable while downloads run.
- Proxy (SOCKS5 or HTTP, with credentials) that a host leaves only when you allow it.
- Settings editable in the app or in `config.toml`, each kept in step with the other.
- Clipboard watching, per-host subfolders, and an optional move to the Recycle Bin when you remove a finished download.
- Hosts: pixeldrain, gofile, buzzheavier, 1fichier, rapidgator and nitroflare (premium accounts), mega.nz through `megatools`, and any direct link.
- Import of JDownloader `.dlc` containers (through JDownloader's key service) and of any text file with links in it.
- Desktop app: light and dark themes that follow Windows, tray icon (closing the window keeps downloading), notifications, single instance, `.dlc` file association, and the in-app browser for hosts that need a person.

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

Headless: `haul --port 8765 --dir D:\Downloads`, then open <http://127.0.0.1:8765/>. `haul --help` lists the options.

## Documentation

- [Using Haul](docs/usage.md): adding links, **Needs you**, hosts, selecting and removing.
- [Configuration](docs/configuration.md): `config.toml`, environment variables and the data folder.
- [How downloads behave](docs/downloading.md): mirrors, files already there, retries, cooldowns, the proxy.
- [Hosts](docs/hosts.md): how each host is resolved and what has been checked against the real host.
- [HTTP API](docs/api.md): the local API the UI, the bookmarklet and scripts use.
- [Decisions](docs/decisions.md): design decisions and why they were made.
