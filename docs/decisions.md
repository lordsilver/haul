# Decisions

Design decisions and why they were made. What the behaviour is, in full, is in [How downloads behave](downloading.md), [Using Haul](usage.md) and [Configuration](configuration.md). Newest last.

## Rust rewrite of a Python prototype

The first version was a Python (aiohttp) script with a web UI. Its logic (mirror grouping, probing names before downloading, per-host slots and cooldowns, the shared speed limit, proxy routing with explicit direct approval, the browser hand-off for interactive host pages) was tested and kept; Haul ports it to Rust. The prototype kept its queue next to the script, so a second copy loaded and rewrote the user's real queue; Haul's state lives in a per-user data folder, an exclusive lock refuses a second instance on the same folder, and saves go through a temporary file and a rename.

## Tauri 2, not the Tauri 3 alpha, and not Electron

When the project started (October 2026), Tauri 3 was at `3.0.0-alpha.4`; there was no beta. Its changes so far are plugin and Linux runtime refactors that this app does not need, while its plugins and documentation were alpha too. Tauri 2.12 is current and stable; the shell is small enough that moving to 3 when it ships is a short change.

On Windows, Tauri renders with WebView2, which is the same Chromium engine Electron bundles, so the page performs the same; Tauri leaves out the bundled Chromium and Node.js, which makes for a small installer, and its webview reports downloads, which the in-app browser needs. Reports of Electron outperforming Tauri mostly come from Linux, where Tauri uses WebKitGTK, or from apps that push a lot of data through Tauri's IPC; Haul's UI does not use the IPC at all.

## One UI, served by the engine

The web UI is a page served by the engine's local HTTP server, and the desktop window simply loads that page. The same UI then works headless in any browser (or from another machine's browser through an SSH tunnel), the browser-facing API is the one the UI uses, and the desktop shell stays a few hundred lines. The shell adds only what a page cannot do itself: the tray, notifications, single-instance handling, file associations and the in-app browser, the latter reached through one API route.

The page is React and TypeScript, built by Vite into `crates/haul-web/ui/dist`, which haul-web embeds. Git keeps the built files, so `cargo build` and `cargo install` still need no Node; CI rebuilds them and fails when they no longer match their sources. The page's types for the state and settings are generated from the Rust ones with ts-rs, so a change in the engine's state breaks the UI's build instead of the running page. The page only lays out what the engine derived.

## shadcn look

The UI uses shadcn/ui components, copied into `ui/src/components/ui` and styled with Tailwind: neutral colors, Geist, Lucide icons. The window has a sidebar with the lists, the download folder and the pages; a header with the title on the left and the commands on the right; a status bar. It replaced a Windows 11 Fluent look because its user prefers this one, and the same page serves headless users in any browser.

Color says what a download's state is, and only that: blue runs, green is done, amber wants the user, red failed, grey is paused or waiting. The sidebar's counts sit in pills of those colors (Active only while something downloads), so the lists that need the user stand out; Wrappers, a source rather than a state, turns amber or red only when one of its wrappers waits for the user or failed.

A download that needs the user is a row of its own, tinted amber, with what the host wants at the end of its host line and one button where its bar would be: **Open here** in the desktop app, the page itself in a browser. The other ways (the page in the system's browser, pasting a link, trying again) are in that button's menu, so ten such downloads are ten rows, not ten boxes. The list never scrolls sideways: a row's columns wrap instead.

Each thing that needs attention is shown once. A download that needs you is flagged by its own row, and its count rides on **Needs you** in the sidebar (the desktop app also sends a notification); the bars above the list are kept for state that affects every list: paused downloads (only while the pause holds a download back; with nothing waiting, the status bar alone says Paused), a settings file with an error, a lost connection. The status bar has the speed, the counts and what is left with when it should be done, so the header carries no summary of its own.

The theme follows the system unless the user picks one; the choice is saved in `config.toml`, so the window frame can match it from the first paint.

## Local API

Loopback-only servers are still reachable from any page the user visits, so the API guards itself with `Host` and `Origin` checks and a framing ban ([HTTP API](api.md) has the details). There are no tokens to manage, and scripts without an `Origin` keep working.

## One settings file, two editors

Every setting lives in `config.toml` and can also be changed in the app. The app writes into the file with `toml_edit`, which keeps the user's comments and layout (a cleared value is commented out, not deleted, so its explanation stays). A file that disappears while Haul runs leaves the settings in effect because some editors delete the file before they write it anew, and falling back to the defaults for that moment would drop the proxy and move the download folder. There is never a second store that silently overrides the file, per-host route choices included.

Account secrets are part of the settings, but they never ride the state stream that every open view receives: the settings view fetches them separately when it opens. Values an environment variable sets cannot change in the app, since the file would not win anyway.

## Retries

Errors are sorted by what retrying can achieve. A dead link or a refusal fails its mirror at once, so the next mirror takes over without delay. A 403 while other downloads from the same host are running is different: that is the host taking no more at once, as pixeldrain does, so the download waits for a slot instead of failing; the two-second gap after a transfer gives the host time to count the finished one out. Network trouble is retried on the same mirror because the host usually comes back and the bytes already downloaded are kept, and an attempt that got data in starts the count over so a flaky connection or a change of network does not use up a mirror. Failures fade with a ten-minute half-life so one bad minute does not exile a host for the rest of the session. Failed downloads are tried again because hosts that are down tend to come back and the cost of another attempt is small.

## Choosing among mirrors

A preferred host that is only busy is waited for, since a user who stars a host (often one they hold an account with) wants the file from it; one that is cooling down or has failed for the file is passed over, so a preference never stalls a download for good.

Preferred hosts are tried in the order the user gives them, and the **Hosts** view changes that order two ways: dragging a preferred host's card by its grip, which suits a mouse, and its up and down arrows, which work from the keyboard and on a touch screen. Both send the whole list at once (`POST /api/preferred-hosts`), so the order on screen is the order saved; the star still adds a host at the end or takes it out.

Speeds come from real downloads. Haul does not test mirrors by downloading a piece of each: file hosts count every download start toward free-mode limits and wait timers, so a test can cost the real download. Nor does it switch mirrors in the middle of a file: without a checksum there is no telling whether two mirrors hold the same bytes, and resuming one from the other's offset would corrupt the file silently.

## Files already downloaded

Hosts' checksums could prove more than name and size, but only by reading the file on disk in full: doing that for every match, four at a time, left a hard disk full of 5 GB parts seeking between them for minutes. Name and size decide by default now, and the checksum comparison is opt-in, one file at a time. Checksums come only from answers Haul already receives. rapidgator, nitroflare and 1fichier publish one too, but only through a separate file-info request per file, which can be added once it can be tried against files of our own.

## Removing downloads and host folders

A removed file goes to the Recycle Bin rather than being deleted, so a slip can be undone. Only the remove button of a finished download does that: **Clear completed** never touches files, because one click there would empty a folder of finished work into the bin. A file that cannot be moved (open in another program, say) keeps its row, so the user sees why and can try again. Files that older versions had megatools download are left alone, since Haul only knows the folder they share with other downloads.

A host's folder is decided when a file finishes, by the host that delivered it: a file merged from several mirrors goes where the mirror that won sends it, and a changed folder applies to downloads already running. Folder names are cleaned like file names and cannot climb out of the download folder.

## Installing for one user or for everyone

Tauri's NSIS installer offers both with the standard MultiUser support, which runs at the highest privilege the account has, so an administrator sees the UAC prompt as the installer starts, whichever mode they then pick. An existing installation's mode is preselected, so updates stay where they are. Haul's data lives in each user's profile either way, never next to the program.

## Updating over an older version

Tauri's installer recommends uninstalling an older version before installing, with that option preselected, so an update ran the old uninstaller first. Haul replaces its program files in place and keeps its data elsewhere, so uninstalling first only costs time. `crates/haul-desktop/windows/installer.nsi` is Tauri's installer template with that page skipped for updates, which install over the older version as **Do not uninstall** did. Reinstalling the same version, going back to an older one and replacing a version installed from the `.msi` still show the page. The template comes from the Tauri CLI version the release workflow uses and changes with it.

Haul looks for its own updates on GitHub (the latest release of `lordsilver/haul`, after start and every six hours, unless `check_updates` is off) rather than with Tauri's updater, which needs its own signing key and an update manifest in every release. The desktop app downloads the new setup program, checks it against the release's `SHA256SUMS`, runs it with `/P /UPDATE /R` (progress only, install over this version, start Haul again) and closes; the installer is the same one a person downloads, so there is one way to install and one to check.

## The data folder on Windows

The data folder is `%LOCALAPPDATA%\io.github.garysassano.haul`, not `%LOCALAPPDATA%\Haul`: that is where a per-user install puts the program, and the uninstaller's option to delete the app's data removes `%LOCALAPPDATA%\<identifier>`, which is also where WebView2 already keeps the app's browser data. On Linux nothing shares `~/.local/share/haul`, so it stays there.

## Browser hand-off and proxy routing

Haul uses public APIs, guest tokens, the user's own accounts, and the pages themselves once their gates are passed. The browser hand-off lets the desktop app catch a download the user starts in its own browser window when a host page requires interaction. That window goes through the same proxy as the downloads (by way of a local, credential-free SOCKS5 bridge, because browser engines cannot pass proxy credentials), so opening a page does not reveal the real address that the proxy hides.

## Reaching the user when a host waits

Interactive host steps should be short and waiting downloads visible, so a download entering **Needs you** raises a toast whose button is the step itself, and a click on the notification lands on the waiting list. When exactly one download waits, its page opens right away, since that is what the notification asked for, and a single link pasted then is taken as that page's final link rather than a new row. A burst of waits shares one toast, and the first state after Haul opens raises none, so a queue full of old waits does not shout on every start.

## A default preference for pixeldrain

Mirrors are not equal: pixeldrain hands a file to its API without an account or a browser step, while other hosts need an account, a browser step or a paid tier for a free download. `preferred_hosts` therefore starts as `["pixeldrain.com"]`, so a file with a pixeldrain mirror starts there and waits for it when it is only busy, exactly as for a host the user stars; the measured-speed order still decides among the rest. Taking the last preferred host out writes `preferred_hosts = []` rather than leaving the key out, because a missing key reads as the default and the preference would come back. The **Hosts** view lists every preferred host, reached or not, so the default can be seen and unstarred before Haul has used pixeldrain at all.

## Wrapper pages resolve on their own, and their files wait

A filecrypt.cc or fileguard.cc link wraps one release's files on many hosts. fileguard.cc's gate is a proof of work rather than a captcha, so Haul solves it itself; filecrypt.cc's is answered too, as [its own decision](#filecrypts-checkbox-is-answered-without-a-browser) explains, and waits for a person only behind a password or when filecrypt.cc turns the answer away; the `.dlc` a person's browser then hands back is fetched and read through the JDownloader key service like any dropped container. A `.dlc` link handed back stays beside the page it came from, as any link handed back does, so when it fails the page is asked for again.

A wrapper is a source of files, not a download, so it lives in a section of its own and its files never start on their own. A release behind one wrapper is often in many parts on a dozen hosts, most of them needing an account, a person or a paid tier, and starting all of them at once filled **Failed** and **Needs you** with rows that were only alternatives. So a wrapper is unwrapped the moment it is added, its files join the list not started, and the user picks what to start and what to remove; the wrapper's own trouble (a gate for a person, a dead link) shows in **Wrappers**, not among the downloads.

A file is one row with every host as a mirror, so one host failing is a failover, not a failed download. fileguard.cc lists each host's links in the same order, so the n-th link of every host is the same file; when the hosts list different counts, or a file's links disagree on its name, the links fall back to matching by name. A link the queue already holds keeps its row, state and turn, and the wrapper's links join it as mirrors.

Every link of a fileguard.cc vault shows the same page, and a burst of solves from one address was refused (HTTP 403), so a vault is added and solved once, solves run one at a time, a solved vault is read without a new solve, and a refused solve is tried again later rather than handed to a person who could do no better.

## filecrypt's checkbox is answered without a browser

filecrypt.cc's gate first waited for a person, because a replay of its proof of work alone was turned away and a real browser passed. Reading the widget's scripts showed why, and that a browser is not needed: next to the SHA-1 proof of work, the page posts what its automation checks found and a report of the visit (pointer, click, canvas, graphics, audio, fonts, screen), encoded but not encrypted. Tried against the live site, the answer is accepted whenever no automation check is flagged and the report holds the pointer's moves and its click, and only then; the user agent, the request headers and every fingerprint can say what they are. So Haul works the proof and sends exactly that: no automation found, the moves and click a person makes on the box, its own user agent, and "not available" for everything it has no browser for.

An embedded browser would also pass, at the cost of shipping or driving one for a page Haul can answer in a few requests; a browser extension, JDownloader's way, would tie Haul to the user's browser. The answer depends on filecrypt.cc's scripts, which can change: when they do, the answer is turned away and the container waits for a person, as before, so nothing breaks worse than it was. The solved page's Click'n'Load data carries its key, so the links are decrypted locally instead of through JDownloader's key service, which the `.dlc` still needs.

## The in-app browser keeps to the host's site

Host pages carry ads that act on a click: they send the page to an ad's site, or open the wanted page in a new window and send this one to the ad. The in-app browser has no tabs and refuses pop-ups, so that left the person on an ad with no way back. Measured on the hosts of one release, each page pulls 0.3–0.7 MB from 5 to 14 ad sites, on random domains that rotate, so a block list would be stale within weeks. The window therefore keeps to the host's site instead: a page from another site that replaces the host's is sent back (back in history first, which keeps a countdown that has run), to the wanted page when an ad opened it in a new window; a new window on the host's own site loads in place unless the page goes on by itself; downloads, which replace no page, are untouched. The host's site includes wherever its first page lands, since some hosts serve their pages from another domain, and a site the page keeps going to by itself is let be after three times within half a minute. A wrapper's page is exempt, because its links lead to other hosts. A site a window was sent back from has shown itself to be an ad's, so while Haul runs no window loads a page from it again: the navigation is refused before anything is fetched. Only a page that loaded marks a site so, never a download, and a site later let be as part of a host's flow is taken off.

Ads also lay a layer over the page, an ad's frame or an invisible sheet above everything, so the first click lands on the ad instead of the button under it. A script the window runs before the page's own takes such layers off as they appear: one above everything that covers much of the window and holds an ad's frame or nothing at all. A captcha floats above the page the same way, so a layer holding a captcha provider's frame stays.

## Pages come compressed, files as stored

Resolvers read whole pages, and some are large: a filecrypt.cc container is 660 KB of HTML, 169 KB gzipped. Haul asks for gzip, which costs one small crate beside the deflate code already in the build, and decompresses as it reads. A file is asked for uncompressed (`Accept-Encoding: identity`) on every request that downloads it or reads its size, because resuming counts the stored bytes and a compressed answer would report another length.

## Hosts can be switched off

A release behind a wrapper sits on a dozen hosts, and some of them only ever ask for an account, a person or a paid tier. The user chooses which hosts Haul may use at all, next to the order of the preferred ones: a host switched off (`disabled_hosts`) is never picked, probed or asked for a person, so its links stay in a file's row without costing anything. A file whose links are all on hosts that are off fails with that reason rather than waiting for nothing, and switching one of those hosts back on starts it again. Switching a host off takes it out of the preferred hosts, and starring one switches it back on. This is the deny list the first wrapper review left out; the owner asked for it.

## A log kept over time, and each download's own history

The first report carried the end of the log as text, notable events only, and only the desktop app wrote one. A report has to show how a download got where it is, so `haul.log` is now JSON Lines (time, level, message and fields such as the download and the link), written by the desktop app and the server alike, kept across runs up to 10 MB a file with five older files. Every download also keeps its last 40 events in memory, debug ones included whatever the log level, and the report carries them. A **Detailed log** switch writes Haul's debug events to the file too while it is on; it applies at once, and `RUST_LOG` still decides when set. OpenTelemetry export was left for later: end users have no collector, and the file is what they can send.

## Many proxies, each its own address

Hosts limit what one address may do: pixeldrain takes five downloads at once and serves 6 GB a day at full speed, then 1 MiB/s for the whole address. One proxy only moves that limit to another address, so Haul takes a list of them (`proxies`, beside `proxy`) and keeps a host's slots, the limit it showed, the waits it asked for and its quota per address. A download takes the address with the fewest of that host's downloads, preferring one the host has not slowed, and keeps it until it ends: a resume or a session that changed address halfway would be refused. Speeds and failures stay per host, since they describe the host. The user's own line can be one more address (`proxy_with_direct`), which shows their real address and so is off by default, and a proxy that gives each connection another address gets a new connection per download (`proxy_fresh_connections`). Lookups and gates use the first proxy, as they did. No proxy service is assumed: anything that speaks SOCKS5 or HTTP fits, so a VPN subscription's SOCKS5 servers can be reused rather than buying another service.

## One description per host

What Haul knew about a host was spread over a list of names, three `match` statements and a pixeldrain case in the engine, so a second host with a daily allowance would have needed another special case. Each host is now one entry of a table (`hosts::KNOWN`): its names, its code, the accounts it takes, what a free user gets where it was measured, and whether it reports how much an address has left. The engine asks the table: a known free limit holds from the first download rather than after a refusal (and an account lifts it), and every host that reports an allowance is asked through each address. Limits nobody measured are left unknown and still learned from refusals.

## A new address from the router, found and tested by Haul

Hosts limit an address, and most home lines get a new one when the router redials, so a redial is a proxy-free way past a slowed or waiting address. Download managers have offered that for long, but left the setup to the user: pick a router, record or import a script, try. Haul finds the router itself (an SSDP search for an Internet Gateway Device, and a FRITZ!Box's known addresses), asks it to redial over UPnP, which needs no login where the router allows it, falls back to TR-064 with the router's login where it does not, and checks that the line came back with another address. A command of the user's own covers anything else. Redials cut the line for the whole home, so they are off unless switched on, come at most every ten minutes, only when the user's own address is what holds a download back and no proxy can take over, and stop for good when a redial brings the same address back, as DS-Lite and shared-address lines do. The router is only looked for once the feature is on or its settings are shown.

## Connections in one place, integrations in one shape

Proxies, the router and VPN services all decide what downloads go out through, so they share a **Connections** page instead of sitting among the other settings. They also share one shape underneath: a source of addresses (the proxy list, a VPN service's servers) only adds exits to the pool, which the engine treats alike, and a source of new addresses for the line itself (a router, a command) only answers "get a new one". A VPN service is an integration only for finding its servers: NordVPN publishes its SOCKS5 servers and their load, so the user gives its service credentials and a number, and Haul keeps the least loaded ones in the pool, refreshed hourly. Another service is another list, not new engine code.

## Hosts' own pages sign for Haul, out of sight

Some hosts hand their links only to their own website's code: vikingfile behind Cloudflare Turnstile, gofile behind a request signature from a secret it rotates, where a request signed with a retired secret gets the address banned. Haul does not imitate that code (no copied secrets, no embedded JavaScript engine): the desktop app opens the host's page in its own browser, off screen, and a small recipe per host (`handoff.js`) reads what the page receives (vikingfile's download button, gofile's folder listing) and hands it to the engine, or says the file is gone. The page comes forward only when it wants a person. Each host's capabilities stay in one table (`hosts::KNOWN`): the accounts it takes, whether it gives anything without one (`needs_account`), what a free user gets, whether it tells its allowance, and whether its page passes this way (`page_passes`); a host that needs an account counts as off without one, unless the desktop app passes its page.

### Which browser passes those pages

Measured on Windows 11 against vikingfile's Turnstile, 36 interleaved trials (fresh profile each, off-screen window, on the line and through a NordVPN server): Edge, Chrome, CloakBrowser and WebKitGTK passed 6 of 6 each without anyone ticking anything (median 3.6 to 4.6 s on the line, 5.8 to 6.7 s through the VPN); Cromite and the app's own WebView2 window got the box 6 of 6 each. WebView2's box was not the off-screen window nor Haul's scripts (both tested): Cloudflare tells an embedded WebView2 apart from Edge. So on Windows the desktop app passes such pages in the Edge that comes with Windows (or Chrome), off screen with a profile of its own, driven over DevTools only to add its page scripts and read what they leave (`chromium.rs`); `Runtime.enable`, which automation is told apart by, stays off. The DevTools port is one Haul picks and names: started with `--remote-debugging-port=0` (the browser picks), the same Edge got the box 4 of 4 times, with a named port, low or high, it passed 8 of 9. CloakBrowser passed as well but brings a 562 MB download and a company's licence; a click on the box (as Turnstile solvers do) did not pass in the browsers that got it. Linux keeps WebKitGTK, which passes.

## Extracting archives inside Haul

Archives are unpacked by Haul itself rather than by an installed 7-Zip: a release's parts can be RAR, 7-Zip or zip whatever their names say, and an extractor that comes with Haul handles all of them on every machine. RAR goes through RARLAB's own unrar library (as `unrar_sys` vendors it), 7-Zip through `sevenz-rust2` and zip through the `zip` crate. The `unrar` crate around the library was left out: its callback for the next volume reads 2048 characters from a shorter name, past its end, on every archive in volumes, and `unrar_sys` lays out the library's structures without the byte packing `dll.hpp` gives them, so Haul declares those structures itself and a test pins their offsets to the header's. Which parts make an archive follows the forms JDownloader's extraction recognises (its `ArchiveType`), including telling from the numbers which part is missing, so a release waits for it rather than failing to unpack. JDownloader opens a zip split into `.z01` pieces with a separate library, Zip4J; Haul reads the pieces as they are and adds a copy of the central directory with each file's place made an offset into all of them, which is all that differs from one zip, so the `zip` crate reads it without the pieces being written again. Old-style RAR volumes would reach `name.z00` after 2600 parts, so a `.zNN` name is taken for a zip's piece.

Extraction runs one archive at a time and writes into a hidden folder beside the first part, moving each entry out under a free name only once all of it is unpacked, for the same reasons downloads never overwrite a file and partial files stay out of sight. It is off by default because it doubles the disk space a release takes until the parts go; the parts go to the Recycle Bin rather than being deleted, as removed downloads do, and only after an extraction that succeeded.

## The engine works out what the views show

The web UI renders what the engine's state says rather than deriving it: which list a download is in, the host it is shown under, which buttons it has, an archive's parts summed up, which wrappers share a card, the counts of the navigation pane and the status bar, how the hosts group, and the pool of addresses in words come worked out in each state (see [HTTP API](api.md)). The rules then live once, in Rust, with tests beside them, where the page's JavaScript has no tests; a script reading the API sees what the window shows; and the page does less on every state it receives. The page keeps what belongs to the browser alone: the selection, dragging, which archives are opened, the theme, dialogs and toasts, search as you type, and the wording of numbers (sizes, durations).
