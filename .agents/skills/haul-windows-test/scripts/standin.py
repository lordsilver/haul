"""Stand-in pages for testing Haul's desktop app without any real site.

    python3 -I standin.py FILES_DIR [PORT]

Two kinds of page, told apart by the Host header:

- On 127.0.0.1:PORT and localhost:PORT (which Haul counts as two sites),
  the /guard/<name> pages, each answering as a page behind Cloudflare's
  check does (403 with `cf-mitigated: challenge`), so Haul passes it in
  Edge, and /files/<name>, the sparse files made in FILES_DIR.
- On <site>.localhost:PORT, the stand-in for <site> in Haul's stand-in mode
  (HAUL_DEBUG=1 and HAUL_STAND_IN=PORT): a release page, pastes, filecrypt
  containers, file hosts' pages and APIs, all made up. Links on these pages
  are written in their real form (https://filecrypt.cc/...); Haul sends
  them here. See the skill's SKILL.md for each page and what to expect.

A stand-in page whose last path part ends in `-check` (or `-check2`) is
behind one (or two) rounds of the stand-in check: a 403 "Just a moment..."
page that sets a `cf_clearance` cookie and loads itself again after 1.5 s.
filecrypt containers always are, at least one round.

Click'n'Load data is encrypted with the `openssl` command.
"""

import base64
import http.server
import json
import os
import subprocess
import sys
import threading
import urllib.parse

FILES = os.path.abspath(sys.argv[1])
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 8799

# ---------------------------------------------------------------------------
# The /guard pages on 127.0.0.1 and localhost.

PAGES = {
    # Passes its "check" and lists nothing: settles with no links.
    "/guard/empty": "<title>Notes</title><p>Nothing to download here.</p><a href='/'>Home</a>",
    # Two links of its own, swapped by a script for two files on the other
    # site after 4 s: the same count, new addresses.
    "/guard/swap": f"""<title>Example R</title><a id=a href='/about'>About</a> <a id=b href='/contact'>Contact</a>
<script>setTimeout(() => {{
  document.getElementById('a').href = 'http://localhost:{PORT}/files/Ex.part1.rar';
  document.getElementById('b').href = 'http://localhost:{PORT}/files/Ex.part2.rar';
}}, 4000);</script>""",
    # A file host's page for one file (its path names it, as
    # /guard/filepage-a/Ex.part3.rar) that also links two other files: the
    # download stays itself, never a list of those.
    "/guard/filepage": f"""<title>Ex.part3.rar</title><h1>Ex.part3.rar</h1>
<p>More files:</p><a href='http://localhost:{PORT}/files/Ex.part1.rar'>Ex.part1.rar</a>
<a href='http://localhost:{PORT}/files/Ex.part2.rar'>Ex.part2.rar</a>""",
    # A file host's page (as /guard/adpage-a/Ex.part4.rar) whose ad sends
    # the whole window to another site a second after it shows, and asks
    # for a new window to it: the window keeps to the page's site.
    "/guard/adpage": f"""<title>Ex.part4.rar</title><h1>Ex.part4.rar</h1>
<script>setTimeout(() => {{
  window.open('http://localhost:{PORT}/ad/visit.php?al=2');
  location.href = 'http://localhost:{PORT}/ad/visit.php?al=1';
}}, 1000);</script>""",
    # A Turnstile that never answers: the pass runs until its time is up.
    "/guard/stuck": "<title>Just a moment...</title><form><input type=hidden name=cf-turnstile-response value=''></form>",
}

# ---------------------------------------------------------------------------
# The acceptance case: one release, Example.Set, in 67 parts on five hosts,
# and another split of it, 34 parts on 1fichier, deleted there.

GIB = 1024**3
PARTS = 67
# The gofile container's rows that show `n/a`, and the two of those
# folders that are still there; gofile turns away every fifth lookup.
GOFILE_UNNAMED = (5, 12, 19, 26, 33, 40, 47, 54, 61, 67)
GOFILE_ALIVE = (12, 47)
GOFILE_LIMIT_EVERY = 5
# The megaup row whose text is an interstitial's title.
MEGAUP_INTERSTITIAL = 23


def part(n, base="Example.Set"):
    return f"{base}.part{n:02d}.rar"


def size(n):
    """A part's exact size: 2 GiB, the last one 1 GiB."""
    return 2 * GIB if n < PARTS else GIB


def shown(nbytes):
    """A size as filecrypt's table shows it, in 1024s."""
    for unit, power in (("GB", 3), ("MB", 2), ("KB", 1)):
        if nbytes >= 1024**power:
            return f"{nbytes / 1024**power:.2f} {unit}"
    return f"{nbytes} B"


def row(at, name, link, size_text, online=True):
    host = urllib.parse.urlsplit(link).hostname
    status = "online" if online else "offline"
    return (
        f'<tr class="kwj3"><td class="status"><i class="{status} small"></i></td>'
        f'<td title="{name}">{name}<span><a href="https://{host}" class="external_link">{host}</a></span></td>'
        f'<td>{size_text}</td><td><a href="/Link/{at:08d}.html" class="button download"></a></td></tr>'
    )


# Each container: its heading and rows (link, name shown, size shown, online).
CONTAINERS = {
    # Another split, named by an ID, every file deleted on 1fichier.
    "AAAAAAAA": (
        "Example.Set",
        [(f"https://1fichier.com/?afile{n:02d}", f"0123abcd.part{n:02d}.rar", "4.00 GB", False) for n in range(1, 35)],
    ),
    # Parts named by an ID, sizes shown.
    "BBBBBBBB": (
        "Example.Set",
        [(f"https://ddownload.com/bbbbbbbbbb{n:02d}", f"4567cdef.part{n:02d}.rar", shown(size(n)), True) for n in range(1, PARTS + 1)],
    ),
    # 57 rows named, 10 `n/a`.
    "CCCCCCCC": (
        "Example.Set",
        [
            (f"https://gofile.io/d/cccc{n:02d}", "n/a" if n in GOFILE_UNNAMED else part(n), "n/a" if n in GOFILE_UNNAMED else shown(size(n)), True)
            for n in range(1, PARTS + 1)
        ],
    ),
    # 66 rows named, one with an interstitial's title; every link's path
    # ends in its part's name.
    "DDDDDDDD": (
        "Example.Set",
        [
            (f"https://megaup.net/dddd{n:02d}/{part(n)}", "Just a moment..." if n == MEGAUP_INTERSTITIAL else part(n), shown(size(n)), True)
            for n in range(1, PARTS + 1)
        ],
    ),
    # Every row named, sizes shown.
    "EEEEEEEE": (
        "Example.Set",
        [(f"https://pixeldrain.com/u/eeee{n:02d}", part(n), shown(size(n)), True) for n in range(1, PARTS + 1)],
    ),
    # Parts named by another ID.
    "FFFFFFFF": (
        "Example.Set",
        [
            (f"https://rapidgator.net/file/{'f' * 30}{n:02d}/89efab01.part{n:02d}.rar.html", f"89efab01.part{n:02d}.rar", shown(size(n)), True)
            for n in range(1, PARTS + 1)
        ],
    ),
    # The container the note links: two small files on pixeldrain, which
    # download from FILES_DIR.
    "GGGGGGGG": (
        "Example.Note",
        [(f"https://pixeldrain.com/u/note{n}", part(n, "Example.Note"), "3.00 MB", True) for n in (1, 2)],
    ),
}

# What pixeldrain's API says of each file: (name, size).
PIXELDRAIN = {f"eeee{n:02d}": (part(n), size(n)) for n in range(1, PARTS + 1)}
PIXELDRAIN.update({f"note{n}": (part(n, "Example.Note"), 3 * 1024 * 1024) for n in (1, 2)})
SMALL = [name for name, _ in PIXELDRAIN.values() if name.startswith("Example.Note")]

RELEASE = """<title>Example.Set</title><h1>Example.Set</h1>
<p>A made-up release for Haul's tests. Notes and links below.</p>
<p>Notes: <a href="https://paste2.org/paste/example-set">https://paste2.org/paste/example-set</a></p>
<ul>
<li><a href="https://filecrypt.cc/Container/AAAAAAAA.html">1fichier</a></li>
<li><a href="https://filecrypt.cc/Container/BBBBBBBB.html">ddownload</a></li>
<li><a href="https://filecrypt.cc/Container/CCCCCCCC.html">gofile</a></li>
<li><a href="https://filecrypt.cc/Container/DDDDDDDD.html">megaup</a></li>
<li><a href="https://filecrypt.cc/Container/EEEEEEEE.html">pixeldrain</a></li>
<li><a href="https://filecrypt.cc/Container/FFFFFFFF.html">rapidgator</a></li>
</ul>
<p><a href="/about">About</a> <a href="/contact">Contact</a></p>"""

PASTES = {
    # Only a description: no link at all.
    "example-set": "Example.Set: a made-up release in 67 parts. Unpack part 01 with any archiver.",
    # A note that links a container.
    "note": "A note: the files for Example.Note are in this container: https://filecrypt.cc/Container/GGGGGGGG.html (thanks)",
    # Two files, shown behind two rounds of the check (as /paste/links-check2).
    "links": "Example.Note\nhttps://pixeldrain.com/u/note1\nhttps://pixeldrain.com/u/note2",
}


def cnl(links):
    """Click'n'Load data for `links`: the key as hex, and the links
    encrypted with AES-128-CBC under that key (also the IV), zero-padded."""
    key = b"0123456789abcdef"
    plain = "\r\n".join(links).encode()
    plain += b"\0" * (-len(plain) % 16)
    crypted = subprocess.run(
        ["openssl", "enc", "-aes-128-cbc", "-K", key.hex(), "-iv", key.hex(), "-nopad", "-nosalt"],
        input=plain,
        capture_output=True,
        check=True,
    ).stdout
    return key.hex(), base64.b64encode(crypted).decode()


def container_page(cid):
    heading, rows = CONTAINERS[cid]
    jk, crypted = cnl([link for link, *_ in rows])
    table = "".join(row(at, name, link, size_text, online) for at, (link, name, size_text, online) in enumerate(rows, 1))
    return f"""<title>{heading} - filecrypt.cc</title><h2>{heading}</h2>
<button onclick="DownloadDLC(this.getAttribute('data-dlc'))" data-dlc="{cid}DLC">DLC</button>
<form onsubmit="CNLPOP('{cid}', '{jk}', '{crypted}', '{heading} - filecrypt.cc'); return false;"><button>Click'n'Load</button></form>
<table>{table}</table>"""


def gofile_page(fid):
    """A folder's page: its script asks the API for the folder, as gofile's
    own page does, and shows what it answers."""
    return f"""<title>gofile</title><h1>Folder {fid}</h1><div id=files>Loading...</div>
<script>
const api = location.protocol + "//api." + location.host + "/contents/{fid}?wt=standin";
fetch(api).then((r) => r.json()).then((answer) => {{
  const files = Object.values(answer.data?.children ?? {{}});
  document.getElementById("files").textContent = answer.status === "ok" ? files.map((f) => f.name).join(", ") : answer.status;
}}).catch((e) => {{ document.getElementById("files").textContent = String(e); }});
</script>"""


GOFILE_CALLS = 0
GOFILE_LOCK = threading.Lock()


def gofile_api(fid):
    """The content API: every fifth call is turned away, eight of the ten
    unnamed folders are deleted, and the rest hold one part each."""
    global GOFILE_CALLS
    with GOFILE_LOCK:
        GOFILE_CALLS += 1
        limited = GOFILE_CALLS % GOFILE_LIMIT_EVERY == 0
    if limited:
        return 429, {"status": "error-rateLimit", "data": {}}
    n = int(fid[4:]) if fid.startswith("cccc") and fid[4:].isdigit() else 0
    if not 1 <= n <= PARTS or (n in GOFILE_UNNAMED and n not in GOFILE_ALIVE):
        return 404, {"status": "error-notFound", "data": {}}
    name = part(n)
    child = {"id": f"file{n:02d}", "type": "file", "name": name, "size": size(n), "link": f"https://store1.gofile.io/download/web/file{n:02d}/{name}"}
    return 200, {"status": "ok", "data": {"id": fid, "type": "folder", "children": {child["id"]: child}}}


def file_page(name):
    """A file host's page for one file: its name, no download."""
    return f'<title>{name}</title><h1>{name}</h1><span class="dk-dl-name" title="{name}"></span><p>Downloads are not stood in for here.</p>'


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write(f"{self.command} {self.headers.get('Host', '')}{self.path}\n")
        sys.stderr.flush()

    def do_HEAD(self):
        self.answer(False)

    def do_GET(self):
        self.answer(True)

    def do_POST(self):
        self.answer(True)

    def do_OPTIONS(self):
        self.send(204, b"", "text/plain", {"Access-Control-Allow-Headers": "*", "Access-Control-Allow-Methods": "GET, POST"})

    def send(self, status, data, kind, headers=None, body=True):
        if isinstance(data, str):
            data = data.encode()
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Access-Control-Allow-Origin", "*")
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if body:
            self.wfile.write(data)

    def html(self, status, page, headers=None, body=True):
        self.send(status, f"<!doctype html><html><body>{page}</body></html>", "text/html; charset=utf-8", headers, body)

    def file(self, name, body, named=True):
        """A file from FILES_DIR; `named` says its name in the answer, as a
        host's download does."""
        name = os.path.basename(name)
        path = os.path.join(FILES, name)
        if not os.path.isfile(path):
            return self.send(404, "not found", "text/plain", body=body)
        with open(path, "rb") as f:
            data = f.read()
        headers = {"Content-Disposition": f'attachment; filename="{name}"'} if named else {}
        self.send(200, data, "application/octet-stream", headers, body)

    def answer(self, body):
        host = (self.headers.get("Host") or "").rsplit(":", 1)[0].lower()
        site = host.removesuffix(".localhost").removeprefix("www.")
        if host in ("127.0.0.1", "localhost", "") or not host.endswith(".localhost"):
            return self.guard(body)
        url = urllib.parse.urlsplit(self.path)
        last = url.path.rstrip("/").rsplit("/", 1)[-1]
        rounds = 2 if last.endswith("-check2") else 1 if last.endswith("-check") else 0
        path = url.path.replace("-check2", "").replace("-check", "")
        if site == "filecrypt.cc" and path.lower().startswith("/container/"):
            rounds = max(rounds, 1)
        if rounds and not self.passed(rounds, body):
            return
        self.site(site, path, url.query, body)

    def passed(self, rounds, body):
        """Whether the visitor passed `rounds` rounds of the stand-in check;
        if not, it is shown the next."""
        cookies = dict(c.strip().split("=", 1) for c in (self.headers.get("Cookie") or "").split(";") if "=" in c)
        done = cookies.get("cf_clearance", "")
        done = int(done.rsplit("-", 1)[-1]) if done.rsplit("-", 1)[-1].isdigit() else 0
        if done >= rounds:
            return True
        page = f"""<title>Just a moment...</title><p>Checking your browser: round {done + 1} of {rounds}.</p>
<script>setTimeout(() => {{ document.cookie = "cf_clearance=standin-{done + 1}; path=/"; location.reload(); }}, 1500);</script>"""
        self.html(403, page, {"cf-mitigated": "challenge"}, body)
        return False

    def site(self, site, path, query, body):
        if site == "releases.example" and path == "/example-set":
            return self.html(200, RELEASE, body=body)
        if site == "paste2.org" and path.startswith("/paste/") and path[7:] in PASTES:
            return self.html(200, f"<title>Paste</title><pre>{PASTES[path[7:]]}</pre>", body=body)
        if site == "filecrypt.cc" and path.lower().startswith("/container/"):
            cid = path.split("/")[-1].removesuffix(".html")
            if cid in CONTAINERS:
                return self.html(200, container_page(cid), body=body)
            return self.html(404, "<title>404</title><p>Not Found</p>", body=body)
        if site == "pixeldrain.com" and path.startswith("/api/file/"):
            fid = path.split("/")[3]
            if fid not in PIXELDRAIN:
                return self.send(404, json.dumps({"success": False, "value": "not_found"}), "application/json", body=body)
            name, nbytes = PIXELDRAIN[fid]
            if path.endswith("/info"):
                info = {"id": fid, "name": name, "size": nbytes, "availability": ""}
                return self.send(200, json.dumps(info), "application/json", body=body)
            return self.file(name, body)
        if site == "1fichier.com" and path == "/check_links.pl":
            length = int(self.headers.get("Content-Length") or 0)
            form = urllib.parse.parse_qs(self.rfile.read(length).decode())
            lines = "".join(f"{link};;;NOT FOUND\n" for link in form.get("links[]", []))
            return self.send(200, lines, "text/plain", body=body)
        if site == "1fichier.com":
            return self.html(200, "<title>1fichier</title><p>File not found</p>", body=body)
        if site == "gofile.io" and path.startswith("/d/"):
            return self.html(200, gofile_page(path[3:]), body=body)
        if site == "api.gofile.io" and path.startswith("/contents/"):
            status, answer = gofile_api(path.split("/")[2])
            return self.send(status, json.dumps(answer), "application/json", body=body)
        if site.endswith("gofile.io") and path.startswith("/download/"):
            return self.file(path.rsplit("/", 1)[-1], body)
        if site == "ddownload.com" and path.startswith("/bbbbbbbbbb"):
            n = int(path[11:13])
            return self.html(200, file_page(f"4567cdef.part{n:02d}.rar"), body=body)
        if site in ("megaup.net", "rapidgator.net"):
            name = urllib.parse.unquote(path.rsplit("/", 1)[-1]).removesuffix(".html")
            return self.html(200, file_page(name), body=body)
        return self.send(404, f"no stand-in for {site}{path}", "text/plain", body=body)

    def guard(self, body):
        path = self.path.split("?")[0]
        page = next((k for k in PAGES if path == k or path.startswith(k + "-")), None)
        if page:
            return self.html(403, PAGES[page], {"cf-mitigated": "challenge"}, body)
        if path.startswith("/files/"):
            return self.file(path, body, named=False)
        return self.send(404, "not found", "text/plain", body=body)


os.makedirs(FILES, exist_ok=True)
for name in ("Ex.part1.rar", "Ex.part2.rar", *SMALL):
    with open(os.path.join(FILES, name), "ab") as f:
        f.truncate(3 * 1024 * 1024)
http.server.ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
