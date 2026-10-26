"""Stand-in pages for testing how Haul's desktop app passes pages out of sight,
without any real site.

Every /guard/<name> page (and /guard/<name>-<anything>, for a fresh address)
answers as a page behind Cloudflare's check does (403 with
`cf-mitigated: challenge`), so Haul passes it in Edge. /files/<name> serves
the sparse files made in FILES_DIR.

    python3 -I standin.py FILES_DIR [PORT]

Reachable from Windows at 127.0.0.1:PORT and localhost:PORT, which Haul
counts as two sites: pages on one can link files on the other.
"""

import http.server
import os
import sys

FILES = os.path.abspath(sys.argv[1])
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 8799

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
    # A Turnstile that never answers: the pass runs until its time is up.
    "/guard/stuck": "<title>Just a moment...</title><form><input type=hidden name=cf-turnstile-response value=''></form>",
}


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write(f"{self.command} {self.path}\n")
        sys.stderr.flush()

    def do_HEAD(self):
        self.answer(False)

    def do_GET(self):
        self.answer(True)

    def answer(self, body):
        path = self.path.split("?")[0]
        page = next((k for k in PAGES if path == k or path.startswith(k + "-")), None)
        file = os.path.join(FILES, os.path.basename(path))
        if page:
            data = f"<!doctype html><html><body>{PAGES[page]}</body></html>".encode()
            self.send_response(403)
            self.send_header("cf-mitigated", "challenge")
            self.send_header("Content-Type", "text/html; charset=utf-8")
        elif path.startswith("/files/") and os.path.isfile(file):
            with open(file, "rb") as f:
                data = f.read()
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
        else:
            data = b"not found"
            self.send_response(404)
            self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if body:
            self.wfile.write(data)


os.makedirs(FILES, exist_ok=True)
for name in ("Ex.part1.rar", "Ex.part2.rar"):
    with open(os.path.join(FILES, name), "ab") as f:
        f.truncate(3 * 1024 * 1024)
http.server.ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
