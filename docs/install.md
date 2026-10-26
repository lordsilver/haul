# Installing

## Windows desktop app

Download the installer from the [latest release](https://github.com/lordsilver/haul/releases/latest) (`haul-desktop-v<version>-x86_64-pc-windows-msvc-setup.exe`). It asks whether to install for you alone, in `%LOCALAPPDATA%\Haul` without administrator rights, or for everyone on the computer, in Program Files; on an administrator account Windows asks for permission as it starts. For a silent install, pass `/S` with `/CurrentUser` or `/AllUsers`. The `.msi` always installs for everyone.

The installer is not code-signed yet, so SmartScreen asks once: **More info**, then **Run anyway**. Windows 10 and 11 ship the WebView2 runtime the app needs.

Haul tells you in its status bar when a new version is out and installs it with one click. Running the new version's installer yourself does the same: it installs over the old version and keeps your downloads and settings.

## Headless server

The release has `haul` for Windows (`haul-v<version>-x86_64-pc-windows-msvc.zip`) and Linux (`haul-v<version>-x86_64-unknown-linux-gnu.tar.gz`). Or build it with Rust 1.96 or later; the web UI is built in, so no Node is needed:

```sh
cargo install --locked --git https://github.com/lordsilver/haul haul-web
```

Start it with `haul --port 8765 --dir D:\Downloads`, then open <http://127.0.0.1:8765/>; `--open` opens it for you. `haul --help` lists the options, and [Configuration](configuration.md) covers `config.toml`, environment variables and the data folder.

The server listens on 127.0.0.1 only, and has no login: anything running on the same computer can use it. The [HTTP API](api.md) describes how it guards against web pages.

## Building from source

With Rust 1.96 or later:

```sh
git clone https://github.com/lordsilver/haul && cd haul
cargo build --release -p haul-web             # target/release/haul(.exe)
cargo build --release -p haul-desktop         # target/release/haul-desktop(.exe)
```

On Linux the desktop app needs WebKitGTK 4.1 and a few more libraries; on Debian or Ubuntu:

```sh
sudo apt-get install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev
```

The installers are built with the Tauri CLI, which needs Node: `cd crates/haul-desktop && npx @tauri-apps/cli@2 build` puts them in `target/release/bundle`.

[Development](development.md) covers changing the web UI and running the tests.
