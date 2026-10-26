# Haul

A download manager for file hosts. Paste links and Haul finds the actual file behind each host's page, merges copies of the same file on different hosts into one download, and fetches it from the host you prefer or the fastest one.

It comes as a Windows desktop app and as `haul`, a headless server with the same web UI for any browser, on Windows or Linux.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://github.com/lordsilver/haul/raw/main/docs/images/haul-dark.png">
  <img alt="Haul's download list: two downloads running, one waiting for a page under Needs you, one failed, two completed and one paused" src="https://github.com/lordsilver/haul/raw/main/docs/images/haul-light.png">
</picture>

## Features

- **Mirrors**: the same file on several hosts is one row, downloaded from your preferred hosts in your order, or the fastest, with failover to the next.
- **Reliable downloads**: resume after pauses and restarts, automatic retries, one shared speed limit, parallel downloads and per-host slots.
- **Connections**: a SOCKS5 or HTTP proxy, VPN servers, and router reconnects.
- **Adding links**: paste them, watch the clipboard, or drop a `.dlc`, `.txt` or `.crawljob` file; pages that hold links for many hosts become one row per file.
- **Archives**: split RAR, 7-Zip and zip archives are held back until every part is there, then extracted.
- **Needs you**: a host page that wants a person waits in its own list, and the desktop app's built-in browser hands the download back once you have dealt with it.
- **Desktop app**: light and dark themes, tray icon, notifications and one-click updates.

## Install

**Windows**: download `haul-desktop-v<version>-x86_64-pc-windows-msvc-setup.exe` from the [latest release](https://github.com/lordsilver/haul/releases/latest) and run it. The installer is not code-signed yet, so SmartScreen asks once: **More info**, then **Run anyway**. Haul tells you when a new version is out and installs it with one click.

**Headless server**: download the `haul` archive for Windows or Linux from the same release, or build it with Rust 1.96 or later:

```sh
cargo install --locked --git https://github.com/lordsilver/haul haul-web
haul --dir ~/Downloads          # then open http://127.0.0.1:8765/
```

The server listens on 127.0.0.1 only. [Installing](https://github.com/lordsilver/haul/blob/main/docs/install.md) has the rest: install for one user or everyone, silent installs, the `.msi`, and building from source.

## Documentation

- [Using Haul](https://github.com/lordsilver/haul/blob/main/docs/usage.md): adding links, **Needs you**, sites, wrappers, archives.
- [Installing](https://github.com/lordsilver/haul/blob/main/docs/install.md): the Windows installer, the headless server, building from source.
- [Configuration](https://github.com/lordsilver/haul/blob/main/docs/configuration.md): `config.toml`, environment variables and the data folder.
- [How downloads behave](https://github.com/lordsilver/haul/blob/main/docs/downloading.md): mirrors, files already there, retries, cooldowns, the proxy.
- [Containers and wrappers](https://github.com/lordsilver/haul/blob/main/docs/containers.md): `.dlc` and text files, and the pages that hold one file's links on many hosts.
- [Sites](https://github.com/lordsilver/haul/blob/main/docs/hosts.md): how each site is handled.
- [HTTP API](https://github.com/lordsilver/haul/blob/main/docs/api.md): the local API the UI, the bookmarklet and scripts use.
- [Debug mode](https://github.com/lordsilver/haul/blob/main/docs/debugging.md) and [development](https://github.com/lordsilver/haul/blob/main/docs/development.md): for diagnosing Haul and working on it.
- [Decisions](https://github.com/lordsilver/haul/blob/main/docs/decisions.md): design decisions and why they were made.
