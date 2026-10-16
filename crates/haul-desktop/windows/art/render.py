"""Renders the installers' artwork from art.html into the BMPs they take.

    python3 render.py CHROME

CHROME is a Chrome or Edge executable. On WSL, a Windows one
(/mnt/c/Program Files/Google/Chrome/Application/chrome.exe) draws with the
Windows fonts the installers are seen with. Each piece is shot at twice its
size, so it stays sharp on high-DPI screens; the installers scale it to fit.
Standard library only.
"""

import os
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

HERE = Path(__file__).parent
PIECES = {
    "sidebar": (164, 314),
    "header": (150, 57),
    "dialog": (493, 312),
    "banner": (493, 58),
}
SCALE = 2


def host_path(path, chrome):
    # A Windows Chrome run from WSL wants Windows paths.
    if chrome.startswith("/mnt/"):
        return subprocess.check_output(["wslpath", "-w", str(path)], text=True).strip()
    return str(path)


def shoot(chrome, page, piece, width, height, out_png):
    page = host_path(page, chrome).replace("\\", "/")
    url = ("file:///" + page.lstrip("/")) + "#" + piece
    subprocess.run(
        [
            chrome,
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            f"--force-device-scale-factor={SCALE}",
            f"--window-size={width},{height}",
            "--timeout=2000",
            f"--screenshot={host_path(out_png, chrome)}",
            url,
        ],
        check=True,
        capture_output=True,
        timeout=60,
    )


def read_png(path):
    """Width, height and rows of RGB bytes of an 8-bit RGB or RGBA PNG."""
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat = 8, b""
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos : pos + 8])
        body = data[pos + 8 : pos + 8 + length]
        if kind == b"IHDR":
            width, height, depth, colour, _, _, interlace = struct.unpack(">IIBBBBB", body)
            assert depth == 8 and colour in (2, 6) and interlace == 0, "unexpected PNG"
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    channels = 4 if colour == 6 else 3
    stride = width * channels
    raw = zlib.decompress(idat)
    rows, prev = [], bytearray(stride)
    for y in range(height):
        kind = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1 : (y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - channels] if i >= channels else 0
            b = prev[i]
            c = prev[i - channels] if i >= channels else 0
            if kind == 1:
                line[i] = (line[i] + a) & 0xFF
            elif kind == 2:
                line[i] = (line[i] + b) & 0xFF
            elif kind == 3:
                line[i] = (line[i] + (a + b) // 2) & 0xFF
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 0xFF
        prev = line
        rgb = bytearray()
        for x in range(width):
            rgb += line[x * channels : x * channels + 3]
        rows.append(rgb)
    return width, height, rows


def write_bmp(path, width, height, rows):
    """A 24-bit bottom-up BMP, the kind NSIS and WiX both take."""
    pad = (4 - width * 3 % 4) % 4
    pixels = bytearray()
    for rgb in reversed(rows):
        for x in range(width):
            r, g, b = rgb[x * 3 : x * 3 + 3]
            pixels += bytes((b, g, r))
        pixels += b"\0" * pad
    header = struct.pack("<2sIHHI", b"BM", 54 + len(pixels), 0, 0, 54)
    info = struct.pack("<IiiHHIIiiII", 40, width, height, 1, 24, 0, len(pixels), 3780, 3780, 0, 0)
    path.write_bytes(header + info + pixels)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    chrome = sys.argv[1]
    # ART_TMP: a folder the browser can read and write, such as a Windows
    # one for a Windows browser run from WSL.
    with tempfile.TemporaryDirectory(dir=os.environ.get("ART_TMP")) as tmp:
        page = Path(tmp) / "art.html"
        page.write_bytes((HERE / "art.html").read_bytes())
        for piece, (width, height) in PIECES.items():
            png = Path(tmp) / f"{piece}.png"
            shoot(chrome, page, piece, width, height, png)
            w, h, rows = read_png(png)
            assert (w, h) == (width * SCALE, height * SCALE), f"{piece}: {w}x{h}"
            write_bmp(HERE / f"{piece}.bmp", w, h, rows)
            print(f"{piece}.bmp {w}x{h}")


if __name__ == "__main__":
    main()
