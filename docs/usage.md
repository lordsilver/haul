# Using Haul

## Adding links

Paste anywhere in the window (Ctrl+V), use **Add links**, drop links or `.dlc` / `.txt` files on the window, or pass them on the command line: `Haul.exe <link or file>…`. A second launch hands its links to the running app. Each way fills the Add dialog, so nothing downloads until you click **Add**; **Settings › Add dropped links and files right away** skips the dialog for dropped and opened ones.

From any page, this bookmarklet opens Haul's Add dialog with the page you are on (adding still takes your click):

```text
javascript:location.href='http://127.0.0.1:8765/#/add/'+encodeURIComponent(location.href)
```

With clipboard watching on (desktop app), links you copy anywhere are added when they point at a supported host or a file.

## Needs you

A host page requiring browser interaction parks its download under **Needs you**, and Haul comes to you: a toast in the open window, and a click on the desktop notification that lands on the waiting list — with a single download waiting, straight on its page.

In the desktop app, **Open here** opens the page in Haul's own browser window: complete the page's steps and press the host's download button, and Haul takes the download over (with the page's cookies, through your proxy if the host uses it). The window keeps to the host's site: when a click lets an ad send the page elsewhere, the window comes straight back, and that site's pages are not loaded again while Haul runs, and a page the host opens in a new window loads in the same one; pop-ups stay closed, and a layer an ad lays over the page to catch your first click is taken off it (a captcha's is left). A host that really moves you to another site gets there after the window has come back three times within half a minute. Anywhere else, **Open page** and **Paste link** do the same by hand; with one download waiting, Ctrl+V of its final link anywhere in Haul gives it to that download.

## Hosts

The **Hosts** view lists each host Haul has reached and each preferred one, with the transfer speed per host, the star that makes it preferred (pixeldrain.com is starred from the start), slots (files downloaded from it at once), the folder its files go to (a subfolder of the download folder), and the proxy route. Preferred hosts come first, in the order downloads try them: drag one by its grip, or use its arrows, to move it, and a host you star joins the end. A host that refused more downloads at once shows _Takes N at a time_; the slot buttons set your own limit and drop the learned one. A host that looks blocked through the proxy offers **Allow direct**, which needs a second click because it reveals your real IP address to that host.

## Wrappers

A wrapper link (fileguard.cc, filecrypt.cc, a `.dlc` link) is unwrapped as soon as it is added, and **Wrappers** shows each one: unwrapping, how many files it found from how many links, waiting for you (a filecrypt.cc container behind a password, or one that turned Haul's answer away: **Open here** in the desktop app, or open the page and paste the `.dlc` link it offers), or failed and why. Its files appear in the list **Not started**; start one with its ▶, an archive's parts with the group's ▶, or all of them with **Start all**. **Remove** on a wrapper takes the files that never started with it, after a second click. More in [Containers and wrappers](containers.md#wrapper-pages).

## Selecting and removing

Click a row to select it, Ctrl+click to add or drop one, Shift+click for a range, Ctrl+A for everything in the view, Esc to let go. The toolbar then acts on the selection: Retry, Pause, Resume and Remove (the Delete key too), asking twice when that deletes partial files. In **Failed**, the broom clears every failed download.

The ✕ on a row takes the download off the list and leaves its file alone, unless **Settings › Move removed files to the Recycle Bin** is on; then a finished download's file goes to the Recycle Bin. On a download that has started, the ✕ asks for a second click, since its partial file goes too. **Clear completed** only tidies the list.

## Archives

The parts of a multi-part archive (`name.part01.rar`, `name.rar` with `name.r00`…, `name.zip` with `name.z01`…, `name.7z.001`) show as one row, with their combined progress and what needs attention; the chevron opens it to the parts, and a click on it selects them all.

## Reporting a problem

**Settings › Diagnostics › Save diagnostics…** saves a report of what Haul is doing: every download and wrapper with its links, state and error, the hosts, the settings, and the end of the desktop app's log, this run's and the last one's. Account keys and the proxy's user and password are left out, and so are the keys in mega.nz links. Send it with a description of what happened.

## Status bar

The speed and what is downloading. Click the speed limit to type a new one, the proxy to switch it on or off or change it, and the clipboard watch (desktop app) to stop it. The download folder is in the left bar, above Hosts: one click opens it.
