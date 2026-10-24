# How downloads behave

What Haul does with a link once it is added, for every host. How each host's link becomes a file is in [Hosts](hosts.md); why it works this way is in [Decisions](decisions.md).

## Names first

A link whose path names a file joins any row with the same file name as another mirror. Names are compared the way hosts tend to differ on them: `_` and a space count alike, as do upper and lower case, and a browser's copy mark (`name.part1 (2).rar`, as some uploads keep it) is taken off the name the file is saved and shown under. Character references a page writes in a name (`&#40;` for `(`) are decoded. A host's page for a file counts as naming it (rapidgator's `name.rar.html` names `name.rar`); a page of its own (`index.php`) names none. A link that hides the name is looked up first (pixeldrain's API, or the first 64 KiB of the page: `Content-Disposition`, `Content-Range`, the page title), and rows for the same name and size merge before anything downloads. A host that names the file only as its download starts (1fichier) gets the same check at that moment, before the file's bytes come: a file done or downloading in another row, or already in the download folder, is not fetched twice.

## Choosing a mirror

A file with mirrors downloads from the hosts you star (`preferred_hosts`, with pixeldrain.com preferred from the start), in that order, which **Hosts** changes when you drag a preferred host or use its arrows. A preferred host that is only busy is waited for; one that is cooling down or has failed for the file is passed over. Otherwise, among the mirrors whose host has a free slot and no cooldown, the host with the best measured speed per file wins; recent failures count against a host, and a host never tried scores high, so each gets a turn. Speeds come from earlier downloads and are remembered in `hosts.json` for a week.

A file can also be sent to one host of its own: the host named under a row, or under an archive for all its parts, opens **Download from**, where you pick one of the file's hosts, or **Automatic** for the order above. The host you pick goes first for that file, waited for when it is only busy and passed over while it cools down or once its link has failed, the other hosts standing by. The row stars the host it downloads from.

## Files already there

Before a download starts, once the host has reported the file's name, Haul looks for that file where the row would save it. The same name with exactly the size the host reported counts as the same file, and the name alone when the host did not say the size. A match finishes the row as **Already downloaded** and fetches nothing; a different file by that name is saved next to it as `name (1).ext`. Links Haul can look up cheaply (pixeldrain, direct links) are checked when added, the others after their download link is resolved.

With **Settings › Compare checksums of files already there** (`verify_existing`), each match is also read in full and compared with the host's checksum, one file at a time and never while downloads are paused. Checksums come from pixeldrain (SHA-256), gofile (MD5), and the `Repr-Digest`, `Digest`, `x-goog-hash` and `Content-MD5` headers of direct links (`Content-MD5` only on a full response). A mismatch means a different file, saved as `name (1).ext`.

A row that found its file never sends it to the Recycle Bin on removal, since it did not download it.

## Failures and retries

- **Dead links and refusals.** A 404, a 410 or a refusal fails that mirror at once, and the next mirror takes over; the row shows the host's reason when its answer gives one.
- **Network trouble.** A dropped connection, a timeout or an unreachable host is retried on the same mirror with growing pauses (2, 4, 8 seconds and so on), `resume_attempts` times (5 by default), keeping the bytes already downloaded; an attempt that got data in before it stopped starts the count over. After that the mirror counts as failed and the next one resumes the same partial file.
- **Busy hosts.** A 403 while other downloads from the same host are under way means the host takes no more at once. Haul then runs no more from it than were running, the refused download waits for a free slot, and the next transfer from that host starts two seconds after one ends. The learned cap is not saved; after ten minutes without another refusal Haul tries one more download at once, and setting the host's slots replaces it. It is learned per address: with several proxies each has its own, and when pixeldrain reports less used than before (your router got a new address, or a new day began), that address starts over. A link you pasted, or that the in-app browser caught, that stops working asks for a new one instead.
- **Cooldowns.** 429 and 503 put the whole host in a cooldown for its `Retry-After` (90 seconds if it gives none).
- **Slow mirrors.** With `min_speed_kbps` set (**Settings › Give up on slow downloads**), a transfer that averages less over `min_speed_seconds` (60 by default), a stalled one included, gives up its mirror: the next mirror resumes the same partial file, or the download fails with the speed it managed. A speed limit of your own turns this off, since it would make every download look slow.
- **Ranking.** Failures lower a host's rank, and each counts half as much every ten minutes, so a host that had a bad moment gets another chance.
- **Failed downloads.** When every mirror has failed, the download waits `retry_failed_minutes` (15 by default) and starts over, up to three times; the row counts down to it.

## Proxy

With a proxy configured, every host goes through it until you allow a direct route for that host under **Hosts**. A 403 or 451 through the proxy flags the host; Haul never leaves the proxy on its own. `use_proxy = false` switches the proxy off without forgetting it. The in-app browser goes through the same proxy.

## Needs you

A host page requiring interaction puts the download under **Needs you**; [Using Haul](usage.md#needs-you) describes how to hand it back.
