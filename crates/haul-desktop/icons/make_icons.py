"""Renders the app icons from the design in crates/haul-web/ui/icon.svg.

Standard library only, so it runs anywhere: python3 make_icons.py
Writes 32x32.png, 128x128.png, 128x128@2x.png, icon.png (512) and icon.ico.
"""

import math
import struct
import zlib
from pathlib import Path

HERE = Path(__file__).parent


def arc(cx, cy, r, start, end, steps=8):
    # Points along a circle, angles in degrees (y grows downwards).
    return [
        (cx + r * math.cos(math.radians(a)), cy + r * math.sin(math.radians(a)))
        for a in (start + (end - start) * i / steps for i in range(steps + 1))
    ]


def polyline(points):
    return list(zip(points, points[1:]))


# Each shape: segments, half stroke width, gradient (top y, colour, bottom y, colour).
TRAY = (
    polyline([(12, 38), (12, 46)] + arc(18, 46, 6, 180, 90)[1:] + arc(46, 46, 6, 90, 0)[1:] + [(52, 38)]),
    3.5,
    (34, (0x1B, 0x7F, 0xE0), 56, (0x0B, 0x4F, 0xA8)),
)
ARROW = (
    [((32, 9), (32, 36)), ((20, 25), (32, 37)), ((44, 25), (32, 37))],
    4.0,
    (6, (0x4C, 0xC2, 0xFF), 42, (0x0A, 0x6F, 0xD8)),
)


def segment_distance(x, y, a, b):
    (ax, ay), (bx, by) = a, b
    dx, dy = bx - ax, by - ay
    length = dx * dx + dy * dy
    t = 0.0 if length == 0 else max(0.0, min(1.0, ((x - ax) * dx + (y - ay) * dy) / length))
    return math.hypot(x - (ax + t * dx), y - (ay + t * dy))


def gradient(y, spec):
    y0, c0, y1, c1 = spec
    t = max(0.0, min(1.0, (y - y0) / (y1 - y0)))
    return tuple(c0[i] + (c1[i] - c0[i]) * t for i in range(3))


def render(size):
    scale = size / 64
    rows = []
    for py in range(size):
        row = bytearray([0])  # PNG filter type: none
        for px in range(size):
            x, y = (px + 0.5) / scale, (py + 0.5) / scale
            r = g = b = a = 0.0
            for segments, half, colours in (TRAY, ARROW):  # later shapes on top
                d = min(segment_distance(x, y, p, q) for p, q in segments) - half
                cover = max(0.0, min(1.0, 0.5 - d * scale))
                if cover <= 0:
                    continue
                cr, cg, cb = gradient(y, colours)
                r, g, b = (cr * cover + r * (1 - cover), cg * cover + g * (1 - cover), cb * cover + b * (1 - cover))
                a = cover + a * (1 - cover)
            if a > 0:
                # The blend above is premultiplied; PNG wants straight alpha.
                r, g, b = r / a, g / a, b / a
            row += bytes((round(r), round(g), round(b), round(255 * a)))
        rows.append(bytes(row))
    return png(size, b"".join(rows))


def png(size, raw):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico(images):
    # PNG-compressed entries, which Windows supports since Vista.
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    body = b""
    for size, data in images:
        dim = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset + len(body))
        body += data
    return out + body


def main():
    cache = {size: render(size) for size in (16, 24, 32, 48, 64, 128, 256, 512)}
    (HERE / "32x32.png").write_bytes(cache[32])
    (HERE / "128x128.png").write_bytes(cache[128])
    (HERE / "128x128@2x.png").write_bytes(cache[256])
    (HERE / "icon.png").write_bytes(cache[512])
    (HERE / "icon.ico").write_bytes(ico([(s, cache[s]) for s in (16, 24, 32, 48, 64, 256)]))


if __name__ == "__main__":
    main()
