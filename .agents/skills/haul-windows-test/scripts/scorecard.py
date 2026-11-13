#!/usr/bin/env python3
"""The live scorecard: gives a private list of real links to a Haul test copy
in debug mode, follows its signals, and tells how far each link got, how long
it took and whether anyone was asked. Each run is kept in a history file next
to the list, so a run is compared with the one before it.

The list and the history are private: they hold real links, and stay outside
the repository. Nothing of them belongs in a commit, a pull request or a note.

    python3 -I scorecard.py LINKS DATA_DIR [--port 8791] [--minutes 12]

LINKS: one link per line, then a label of your own for it (any words); lines
that start with # are left out. DATA_DIR: the test copy's data folder, as WSL
sees it (it holds `debug-token`). The test copy must be running.
"""

import argparse
import datetime
import json
import os
import queue
import subprocess
import sys
import threading
import time

CURL = "/mnt/c/Windows/System32/curl.exe"


def host_of(url):
    return url.split("/")[2] if url.count("/") >= 2 else url


class Link:
    def __init__(self, url, label):
        self.url, self.label = url, label
        self.result, self.detail, self.secs = "nothing yet", "", None
        self.led, self.led_secs, self.asks = [], None, 0

    def set(self, result, detail, now):
        # A link that was read stays read, whatever its rows do later.
        if self.result == "read":
            return
        if (result, detail) != (self.result, self.detail):
            self.result, self.detail, self.secs = result, detail, now


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("links")
    ap.add_argument("data_dir")
    ap.add_argument("--port", type=int, default=8791)
    ap.add_argument("--minutes", type=float, default=12)
    args = ap.parse_args()

    links = []
    with open(args.links, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            url, _, label = line.partition(" ")
            if not any(l.url == url for l in links):
                links.append(Link(url, label.strip() or f"link {len(links) + 1}"))
    if not links:
        sys.exit("no links in the list")
    with open(os.path.join(args.data_dir, "debug-token"), encoding="utf-8") as f:
        token = f.read().strip()
    base = f"http://127.0.0.1:{args.port}"
    head = ["-H", f"X-Haul-Debug: {token}", "-H", "Content-Type: application/json"]

    signals = queue.Queue()
    stream = subprocess.Popen([CURL, "-sN", *head, f"{base}/api/debug/stream"], stdout=subprocess.PIPE)

    def read():
        for line in stream.stdout:
            try:
                signals.put(json.loads(line))
            except ValueError:
                pass

    threading.Thread(target=read, daemon=True).start()
    try:
        hello = signals.get(timeout=10)
    except queue.Empty:
        sys.exit("the test copy's debug stream does not answer: is it running in debug mode?")
    version = hello.get("version", "?")

    by_url = {l.url: l for l in links}
    by_id = {}
    start = time.monotonic()
    text = "\n".join(l.url for l in links)
    subprocess.run([CURL, "-s", *head, "-d", json.dumps({"text": text}), f"{base}/api/add"], stdout=subprocess.DEVNULL, check=False)
    deadline = start + args.minutes * 60
    while time.monotonic() < deadline and any(l.result != "read" for l in links):
        try:
            s = signals.get(timeout=1)
        except queue.Empty:
            continue
        now = round(time.monotonic() - start)
        kind, rid = s.get("kind"), s.get("id")
        if kind == "status" and s.get("from") is None and s.get("name") in by_url:
            by_id[rid] = by_url[s["name"]]
        link = by_id.get(rid)
        if link is None:
            continue
        if kind == "pass" and s.get("event") == "end":
            outcome = s.get("outcome")
            led = outcome.get("led_to") if isinstance(outcome, dict) else None
            if led:
                link.led, link.led_secs = led, now
                for to in led:
                    by_url.setdefault(to, link)
                link.set("led on", "to " + ", ".join(sorted({host_of(u) for u in led})), now)
            elif isinstance(outcome, dict) and "files" in outcome and s.get("url") == link.url:
                link.set("read", "a file", now)
        elif kind == "listing":
            rows = len(s.get("rows") or [])
            # The listing of the link's own page that only led on is not the read.
            if rows and not (s.get("url") == link.url and link.led):
                link.set("read", f"{rows} row{'s' if rows != 1 else ''}", now)
        elif kind == "ask":
            link.asks += 1
            link.set("asks a person", s.get("gate") or s.get("need") or "", now)
        elif kind == "status" and s.get("to") in ("waiting", "error") and s.get("error"):
            words = s["error"]
            link.set("waits" if s["to"] == "waiting" else "failed", words[:70], now)
        elif kind == "status" and s.get("to") == "downloading":
            link.set("read", "downloading", now)
    stream.kill()

    width = max(len(l.label) for l in links)
    print(f"\nHaul {version}, {len(links)} links, {round(time.monotonic() - start)} s\n")
    for l in links:
        secs = f"{l.secs:>4} s" if l.secs is not None else "      "
        led = f" (led on after {l.led_secs} s)" if l.led_secs is not None and l.result != "led on" else ""
        asks = f", asked {l.asks}x" if l.asks else ""
        print(f"  {l.label:<{width}}  {l.result:<13} {secs}  {l.detail}{led}{asks}")
    read_ = sum(l.result == "read" for l in links)
    print(f"\n  read: {read_} of {len(links)}; asked a person: {sum(l.asks > 0 for l in links)}\n")

    history = args.links + ".history.jsonl"
    last = None
    if os.path.exists(history):
        with open(history, encoding="utf-8") as f:
            lines = [line for line in f if line.strip()]
        last = json.loads(lines[-1]) if lines else None
    run = {
        "when": datetime.datetime.now().isoformat(timespec="seconds"),
        "version": version,
        "links": {l.url: {"label": l.label, "result": l.result, "detail": l.detail, "secs": l.secs, "asks": l.asks} for l in links},
    }
    with open(history, "a", encoding="utf-8") as f:
        f.write(json.dumps(run) + "\n")
    if last:
        worse = []
        for l in links:
            was = last["links"].get(l.url)
            if not was:
                continue
            if was["result"] == "read" and l.result != "read":
                worse.append(f"  {l.label}: was read in {was['secs']} s (Haul {last['version']}), now {l.result}")
            elif was["result"] == "read" and l.secs and was["secs"] and l.secs > 1.5 * was["secs"] + 10:
                worse.append(f"  {l.label}: read in {l.secs} s, was {was['secs']} s (Haul {last['version']})")
        print("worse than the run before:\n" + "\n".join(worse) if worse else f"nothing worse than the run before (Haul {last['version']}, {last['when']})")
    sys.exit(0 if read_ == len(links) else 1)


if __name__ == "__main__":
    main()
