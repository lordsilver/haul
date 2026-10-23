# Haul

A download manager for file hosts. Paste links, or drop a `.dlc` or `.txt` file, and Haul resolves each host's page into the actual file, merges mirrors of the same file across hosts, picks the host you prefer or the fastest one, and downloads with resume, a shared speed limit and an optional proxy. When a host page requires browser interaction, the download waits under **Needs you**, and the desktop app's built-in browser hands it back to Haul once you have dealt with the page.

It comes as a Windows desktop app (a Tauri 2 shell around the engine) and as `haul`, a headless server with the same web UI for any browser.

## Status

Early. Working today:

- Mirrors: the same file on several hosts becomes one row, downloaded from the hosts you star, in the order you set (pixeldrain.com from the start), or else the fastest, with failover to the next mirror.
- Resume through HTTP ranges; partial files survive restarts and pauses.
- Files already in the download folder are recognised and not fetched again, optionally after comparing the host's checksum.
- Automatic retries of network trouble, of rate-limited hosts and of failed downloads.
- One global speed limit, parallel downloads and per-host slots, all changeable while downloads run.
- Proxy (SOCKS5 or HTTP, with credentials) that a host leaves only when you allow it.
- Settings editable in the app or in `config.toml`, each kept in step with the other.
- Archives in parts (RAR, 7-Zip, zip, a zip split into `.z01` pieces, and pieces cut by a file splitter) recognised in the forms JDownloader knows, held back while a part is missing, and extracted once their last part is downloaded, or from their row, optionally moving the parts to the Recycle Bin afterwards.
- Clipboard watching, per-host subfolders, and an optional move to the Recycle Bin when you remove a finished download.
- A diagnostics report to save and send when something goes wrong, with account keys and passwords left out.
- Hosts: pixeldrain, gofile, buzzheavier, vikingfile, 1fichier, rapidgator and nitroflare (premium accounts), mega.nz through `megatools`, and any direct link.
- Import of JDownloader `.dlc` containers (through JDownloader's key service) and of any text file with links in it.
- Wrapper pages: a fileguard.cc vault solves its gate on its own and each file (each part of a release) becomes one row with every host as a mirror, not started until you start it; a filecrypt.cc container answers its checkbox and decrypts its Click'n'Load links itself, waiting for you in **Wrappers** only behind a password or when filecrypt.cc turns the answer away. A mirrored.to link becomes one row with every host that took the file as a mirror. A web page that lists links (a release's post) adds them as if pasted.
- Desktop app: light and dark themes that follow Windows, tray icon (closing the window keeps downloading), notifications, single instance, `.dlc` file association, and the in-app browser for hosts that need a person.

## Install

Windows: download the installer from the [releases](https://github.com/lordsilver/haul/releases) (`haul-desktop-v<version>-x86_64-pc-windows-msvc-setup.exe`). It asks whether to install for you alone, in `%LOCALAPPDATA%\Haul` without administrator rights, or for everyone on the computer, in Program Files; on an administrator account Windows asks for permission as it starts. For a silent install, pass `/S` with `/CurrentUser` or `/AllUsers`. The `.msi` always installs for everyone. The installer is not code-signed yet, so SmartScreen asks once: **More info**, then **Run anyway**. Windows 10 and 11 ship the WebView2 runtime the app needs.

From source, with Rust stable:

```sh
# Headless server with the web UI
cargo install --locked --git https://github.com/lordsilver/haul haul-web

# Desktop app (on Linux it needs WebKitGTK 4.1, see .github/workflows/ci.yml)
git clone https://github.com/lordsilver/haul && cd haul
cargo build --release -p haul-desktop        # target/release/haul-desktop(.exe)
cd crates/haul-desktop && npx @tauri-apps/cli@2 build   # installers in target/release/bundle
```

Headless: `haul --port 8765 --dir D:\Downloads`, then open <http://127.0.0.1:8765/>. `haul --help` lists the options.

The web UI lives in `crates/haul-web/ui` (React, TypeScript, Tailwind and shadcn/ui, built with Vite). Git keeps the built page in `ui/dist`, so the commands above need no Node. To change it, with Node 26 and pnpm (`mise install` sets them up): `pnpm install`, then `pnpm dev` for a live page that talks to a Haul already running on port 8765, and `pnpm check && pnpm build` before committing the sources together with `dist`. `cargo test` regenerates the page's types from the Rust ones (`ui/src/lib/bindings`).

## Documentation

- [Using Haul](docs/usage.md): adding links, **Needs you**, hosts, selecting and removing.
- [Configuration](docs/configuration.md): `config.toml`, environment variables and the data folder.
- [How downloads behave](docs/downloading.md): mirrors, files already there, retries, cooldowns, the proxy.
- [Containers and wrappers](docs/containers.md): `.dlc` and text files, and the pages that wrap one file's links on many hosts.
- [Hosts](docs/hosts.md): how each host is resolved and what has been checked against the real host.
- [HTTP API](docs/api.md): the local API the UI, the bookmarklet and scripts use.
- [Decisions](docs/decisions.md): design decisions and why they were made.
