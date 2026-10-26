---
name: host-research
description: Find out how a file host's API behaves before changing a Haul resolver — its official API docs, JDownloader's open-source plugin for the same host, and local stand-in tests — without ever trying the user's own links. Use when adding or changing a resolver in crates/haul-core/src/hosts, when a host starts answering differently, or when looking for what a host publishes (names, sizes, checksums, limits).
---

# Researching a file host

## Sources, best first

1. The host's official API docs. pixeldrain's (`https://pixeldrain.com/api`) reads fine with WebFetch. gofile's docs page renders client-side and comes back empty, and WebFetch cannot fetch 1fichier.com or rapidgator.net at all.
2. JDownloader's plugin for the host. JDownloader 2 is open source, and its host plugins hold years of each host's quirks. Its SVN needs a login; the public GitHub mirror stopped in 2024-09:

   ```sh
   gh api repos/mirror/jdownloader/contents/src/jd/plugins/hoster/RapidGatorNet.java --jq .content | base64 -d | rg -n 'hash|md5|api/'
   ```

   Plugin names follow the host: `PixeldrainCom`, `GofileIo`, `OneFichierCom`, `RapidGatorNet`, `NitroFlareCom`, `BuzzheavierCom`. JDownloader's own download logic (an existing file, renaming, hash checks) is in `src/jd/controlling/downloadcontroller/DownloadWatchDog.java`.

3. Other open-source downloaders (pyLoad, yt-dlp's extractors) as a cross-check.

All of these are leads to confirm, not truth: hosts change their APIs.

## Leads so far (2026-10)

| Host       | Name and size              | Checksum                        | Notes                                                                                                                                                 |
| ---------- | -------------------------- | ------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| pixeldrain | `/api/file/{id}/info`      | `hash_sha256`, hex (documented) | List entries may lack the hash. Takes 5 downloads at once per address and refuses more with 403; slows to about 2 MB/s per address after some volume. |
| gofile     | the contents API's entries | `md5`                           |                                                                                                                                                       |
| rapidgator | `file/info` (API v2)       | `hash` (MD5)                    | Haul calls only `file/download`.                                                                                                                      |
| nitroflare | `getFileInfo`              | `md5`                           | Haul calls only `getDownloadLink`.                                                                                                                    |
| 1fichier   | `file/info.cgi`            | `checksum` (Whirlpool)          | Haul calls only `get_token.cgi`.                                                                                                                      |

Using the rapidgator, nitroflare and 1fichier checksums costs one more request per file; add it once it can be tried on a file of the user's own.

## Testing

`crates/haul-core/src/hosts/resolver_tests.rs` runs each resolver against a local stand-in for the host's API: pin there the request and the answer the docs or the plugin describe. `crates/haul-core/tests/engine.rs` has stand-in hosts for whole downloads (`/busy` sends two files at once and refuses more, `/trickle` is slow, `/drip` breaks every third of a file, `/refuse` always says no). Add one when a behaviour needs it. Whether the real host still answers that way is a separate, live question, for the user's own files.
