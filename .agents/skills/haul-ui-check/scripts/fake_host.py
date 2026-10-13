"""A fake file host for the in-app browser hand-off.

/captcha sets a session cookie and starts a download after a moment, as a
host does once its captcha is solved; /files/<name> only serves requests that
carry that cookie. Haul parks /captcha under Needs you (it is a web page), and
"Open here" should end with the file downloaded by the engine, with the cookie.

Usage: python3 fake_host.py [port]   (default 8798; logs requests to stderr)
"""

import http.server
import sys

PAGE = b"""<!doctype html><html><head><title>Solve to download</title></head>
<body style="font:16px sans-serif;padding:40px"><h1>Fake host</h1>
<p>Pretend a captcha was solved. Download starts in a moment.</p>
<a id="go" href="/files/solved-file.bin">Download</a>
<script>setTimeout(() => document.getElementById('go').click(), 1500)</script></body></html>"""

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s | cookie=%s | ua=%s\n" % (self.command, self.path, self.headers.get("Cookie"), (self.headers.get("User-Agent") or "")[:60]))
    def do_HEAD(self):
        self.do_GET(head=True)
    def do_GET(self, head=False):
        if self.path == "/captcha":
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Set-Cookie", "session=s3cret; Path=/")
            self.send_header("Content-Length", str(len(PAGE)))
            self.end_headers()
            if not head: self.wfile.write(PAGE)
        elif self.path.startswith("/files/"):
            if "session=s3cret" not in (self.headers.get("Cookie") or ""):
                self.send_response(403); self.send_header("Content-Length", "0"); self.end_headers(); return
            body = bytes(range(256)) * 4096  # 1 MiB
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Disposition", 'attachment; filename="solved-file.bin"')
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if not head: self.wfile.write(body)
        else:
            self.send_response(404); self.send_header("Content-Length", "0"); self.end_headers()

port = int(sys.argv[1]) if len(sys.argv) > 1 else 8798
http.server.ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
