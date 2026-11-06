#!/usr/bin/env python3
"""Runs real links through a running Haul, one at a time, and records what
each came to: the live half of the host suite.

    python3 -I scorecard.py --links LINKS --out DIR [--api URL] [--curl PATH]
                            [--mode NAME] [--wait SECONDS]

LINKS is a text file outside the repository, one link per line, optionally
`label<TAB>link`; lines starting with `#` are skipped. Haul is reached through
`--curl` (Windows' curl.exe from WSL, since WSL cannot reach Windows'
loopback). A file counts once bytes of it arrive; the row is then removed with
its partial file, so a scorecard downloads little; a wrapper is taken off with
the files it listed. Results go to DIR as
`scorecard-<mode>.json` and `scorecard-<mode>.md`, which name hosts and say
what happened, without the links.
"""

import argparse
import json
import re
import subprocess
import sys
import time
from urllib.parse import urlparse

URL = re.compile(r"https?://[^\s\"'<>)]+")


def host_of(link):
    host = urlparse(link).hostname or "?"
    return host[4:] if host.startswith("www.") else host


def without_links(text):
    """Reasons as Haul gives them, with every link shortened to its host."""
    return URL.sub(lambda m: f"<{host_of(m.group(0))}>", text or "")


class Haul:
    def __init__(self, api, curl):
        self.api = api.rstrip("/")
        self.curl = curl

    def call(self, path, body=None):
        cmd = [self.curl, "-s", "-m", "30"]
        if body is not None:
            cmd += ["-X", "POST", "-H", "Content-Type: application/json", "-d", json.dumps(body)]
        out = subprocess.run(cmd + [self.api + path], capture_output=True, text=True, check=False)
        try:
            return json.loads(out.stdout) if out.stdout.strip() else {}
        except json.JSONDecodeError:
            return {}

    def state(self):
        return self.call("/api/state")


def settle(row, wrapper):
    """What a row or wrapper came to, or None while it is still on its way."""
    if wrapper is not None:
        status = wrapper.get("status")
        if status == "done":
            found = wrapper.get("found") or {}
            return f"listed {found.get('files', 0)}", ""
        if status == "captcha":
            return "needs you", wrapper.get("error", "")
        if status == "error":
            return "failed", wrapper.get("error", "")
        return None
    status = row.get("status")
    if status == "done" or (status == "downloading" and row.get("done", 0) > 0):
        return "file", ""
    if status == "captcha":
        return "needs you", row.get("error", "")
    if status == "error":
        return "failed", row.get("error", "")
    if status == "waiting" and row.get("wait", 0) > 300:
        return "waits", row.get("error", "")
    return None


def run_one(haul, link, wait):
    before = haul.state()
    seen = {i["id"] for i in before.get("items", [])} | {w["id"] for w in before.get("wrappers", [])}
    added = haul.call("/api/add", {"text": link})
    if not any(added.get(k) for k in ("added", "merged", "wrappers")):
        return "not added", json.dumps(added), 0.0, [], []
    start = time.monotonic()
    while time.monotonic() - start < wait:
        time.sleep(3)
        state = haul.state()
        rows = [i for i in state.get("items", []) if i["id"] not in seen]
        wrappers = [w for w in state.get("wrappers", []) if w["id"] not in seen]
        wrapper = wrappers[0] if wrappers else None
        row = rows[0] if rows else None
        got = settle(row or {}, wrapper) if (row or wrapper) else None
        if got:
            return got[0], got[1], time.monotonic() - start, [i["id"] for i in rows], [w["id"] for w in wrappers]
    state = haul.state()
    rows = [i["id"] for i in state.get("items", []) if i["id"] not in seen]
    wrappers = [w["id"] for w in state.get("wrappers", []) if w["id"] not in seen]
    return "still going", "", time.monotonic() - start, rows, wrappers


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--links", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--api", default="http://127.0.0.1:8791")
    p.add_argument("--curl", default="curl")
    p.add_argument("--mode", default="normal", help="a name for the run, such as normal or generic-only")
    p.add_argument("--wait", type=float, default=240)
    args = p.parse_args()

    haul = Haul(args.api, args.curl)
    if not haul.state().get("version"):
        sys.exit(f"no Haul answers at {args.api}")
    results = []
    for line in open(args.links, encoding="utf-8"):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        label, _, link = line.rpartition("\t")
        label = label or host_of(link)
        outcome, reason, took, rows, wrappers = run_one(haul, link, args.wait)
        # Take off what it added, partial files and the files a wrapper listed
        # too, so the next link starts clean and little is downloaded.
        for wid in wrappers:
            haul.call(f"/api/wrapper/{wid}/remove", {})
        for rid in rows:
            haul.call(f"/api/item/{rid}/remove", {})
        result = {"label": label, "host": host_of(link), "outcome": outcome,
                  "reason": without_links(reason), "seconds": round(took)}
        results.append(result)
        print(f"{label:<32} {outcome:<12} {result['seconds']:>4}s  {result['reason'][:90]}", flush=True)

    out = f"{args.out.rstrip('/')}/scorecard-{args.mode}"
    with open(out + ".json", "w", encoding="utf-8") as f:
        json.dump({"mode": args.mode, "results": results}, f, indent=2)
    counts = {}
    for r in results:
        counts[r["outcome"]] = counts.get(r["outcome"], 0) + 1
    with open(out + ".md", "w", encoding="utf-8") as f:
        f.write(f"# Scorecard: {args.mode}\n\n")
        f.write(", ".join(f"{n} {k}" for k, n in sorted(counts.items())) + f" of {len(results)}\n\n")
        f.write("| Site | Outcome | Seconds | Reason |\n| --- | --- | --- | --- |\n")
        for r in results:
            f.write(f"| {r['label']} | {r['outcome']} | {r['seconds']} | {r['reason'].replace('|', '/')} |\n")
    print(f"\n{', '.join(f'{n} {k}' for k, n in sorted(counts.items()))} of {len(results)}: {out}.md")


if __name__ == "__main__":
    main()
