# Configuration

## `config.toml`

Everything under **Settings** is stored in `config.toml` in the data folder: the app writes each change to the file, keeping your comments and layout, and changes you save in the file apply within a second. If the file stops parsing, the previous settings stay in effect and the app shows the error until it is fixed; if it disappears while Haul runs, they stay in effect too.

**Settings › Settings file › Open** creates the file with every key explained. The keys, with their defaults:

```toml
download_dir = "D:/Downloads"   # default: Downloads\Haul
max_total = 8                   # files at the same time
rate_limit_mbps = 0             # MB/s, 0 = no limit
max_active_gb = 0               # most GB of files downloading at once (20: four 5 GB files), 0 = no limit
theme = "system"                # system, light or dark
watch_clipboard = false         # desktop app
recycle_on_remove = false       # removing a finished download also moves its file to the Recycle Bin
skip_add_dialog = false         # dropped and opened links are added without the Add dialog
verify_existing = false         # compare files already there with the host's checksum
auto_extract = false            # unpack an archive in parts once the last part is downloaded
delete_after_extract = false    # an archive's parts go to the Recycle Bin once it is unpacked without error
archive_passwords = []          # tried in order on an encrypted archive, after the one typed for it
detailed_log = false            # write every request, retry and mirror choice to haul.log
reconnect = false               # a new address from the router when yours holds downloads back
reconnect_command = ""           # a command that gets the new address instead of the router
router_user = ""                 # the router's login, for one that redials only with it
router_password = ""
nordvpn_user = ""                # NordVPN service credentials, for its SOCKS5 servers
nordvpn_password = ""
nordvpn_servers = 0              # how many of them join the proxies, least loaded first
nordvpn_countries = []           # the countries they come from; every one when empty
nordvpn_on = true                # false keeps the above but leaves NordVPN's servers out
resume_attempts = 5             # retries per mirror after network trouble
min_speed_kbps = 0              # give up a mirror slower than this (KiB/s) ...
min_speed_seconds = 60          # ... over this long; 0 = off
retry_failed_minutes = 15       # retry failed downloads (up to 3 times), 0 = never
proxy = "socks5://user:pass@host:1080" # every host goes through it unless allowed direct
use_proxy = true                # false keeps proxies and VPN servers but connects directly
proxies_on = true               # false leaves the proxies of proxy and proxies out
preferred_hosts = ["rapidgator.net"]   # downloaded from first, in this order (default: ["pixeldrain.com"])
disabled_hosts = ["gofile.io"]         # never downloaded from: their links are left alone
proxies = ["socks5://u:p@nl.example:1080"]  # more proxies; downloads spread over them
proxy_with_direct = false       # your own connection as one more of them
proxy_fresh_connections = false # a new connection per download, for rotating endpoints
megatools = "C:/Tools/megatools/megatools.exe" # default: found on PATH
user_agent = "..."              # default: Haul's own

# Accounts
1fichier_key = ""
pixeldrain_key = ""
rapidgator_user = ""
rapidgator_pass = ""
nitroflare_user = ""
nitroflare_key = ""
gofile_token = ""

[limits]            # files at once per host (default 32)
"1fichier.com" = 1

[routes]            # hosts allowed off the proxy
"example.com" = "direct"

[folders]           # subfolders of the download folder per host
"pixeldrain.com" = "pixeldrain"
```

What each download setting does is in [How downloads behave](downloading.md).

## Environment variables

`HAUL_PROXY` replaces `proxy`; `HAUL_1FICHIER_KEY`, `HAUL_PIXELDRAIN_KEY`, `HAUL_RAPIDGATOR_USER`, `HAUL_RAPIDGATOR_PASS`, `HAUL_NITROFLARE_USER`, `HAUL_NITROFLARE_KEY` and `HAUL_GOFILE_TOKEN` fill the account keys the file leaves unset. Settings shows those as set by the environment and does not change them. `HAUL_DATA_DIR` moves the data folder and `HAUL_PORT` the local port (default 8765).

## The data folder

The data folder is `%LOCALAPPDATA%\io.github.garysassano.haul` on Windows, which the uninstaller deletes when you ask it to remove the app's data, and `~/.local/share/haul` on Linux.

It holds `config.toml`, the queue (`queue.json`), what Haul measured per host (`hosts.json`), the desktop app's log (`haul.log`, with the last run's kept as `haul.old.log`), the in-app browser's profiles (`webview\`) and `haul.lock`, which stops a second Haul from loading and rewriting the same queue. Partial downloads sit in `.partial` inside the download folder until they finish, and move along when you change the download folder.
