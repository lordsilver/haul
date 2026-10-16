# Containers and wrappers

A container is a file that holds links: a `.dlc` from JDownloader, or plain text with links in it. Dropping one on the window, opening it, passing it on the command line or importing it through the [HTTP API](api.md) shows the links in the Add dialog — nothing downloads until **Add** — or adds them at once with **Settings › Add dropped links and files right away**. From there each link behaves as if it had been pasted; what that means is in [How downloads behave](downloading.md).

## Text files

Any file whose name does not end in `.dlc` is read as text, so `.txt` is the common case but neither the name nor any format matters: every `http(s)` link in the file is taken, one per line or not, with trailing punctuation trimmed.

## `.dlc` files

A `.dlc` holds an encrypted list of links, and decrypting it needs JDownloader's key service once: the file's last 88 characters go to the service, its answer decrypts the file key, and that key decrypts the list. Import therefore works while the service is reachable, and the names and the package (the subfolder its links get) come out of the decrypted list.

## Wrapper pages

A wrapper page holds one file's links on many hosts behind a gate; filecrypt.cc and fileguard.cc are the common ones. A wrapper link is a page to Haul: it waits under **Needs you**, and the desktop app can open it in its in-app browser, which goes through the proxy like the downloads. After the gate the solved page lists the host links; copy them out, or save the `.dlc` such a page offers and drop that on Haul like any other container.
