"""Draws the app icon (stacked "versions" on a dark rounded square) with the stdlib only.

Writes assets/icon.ico (16-256 px, PNG entries) and assets/icon64.rgba (raw RGBA for the window icon).
Run: python assets/make_icon.py
"""
import os
import struct
import zlib

BG = (30, 30, 30)
ACCENT = (255, 155, 69)
DIM = (150, 96, 52)


def draw(size):
    px = bytearray(size * size * 4)
    r = size * 0.22  # corner radius
    ss = 4           # supersampling

    def inside_round_rect(x, y, x0, y0, x1, y1, rad):
        cx = min(max(x, x0 + rad), x1 - rad)
        cy = min(max(y, y0 + rad), y1 - rad)
        return (x - cx) ** 2 + (y - cy) ** 2 <= rad * rad

    # three layers, newest (bottom) brightest
    bars = [
        (0.26, 0.24, 0.74, 0.36, DIM),
        (0.20, 0.44, 0.80, 0.56, DIM),
        (0.14, 0.64, 0.86, 0.78, ACCENT),
    ]
    for py in range(size):
        for pxl in range(size):
            acc = [0, 0, 0, 0]
            for sy in range(ss):
                for sx in range(ss):
                    x = (pxl + (sx + 0.5) / ss) / size
                    y = (py + (sy + 0.5) / ss) / size
                    if not inside_round_rect(x * size, y * size, 0, 0, size, size, r):
                        continue
                    col = BG
                    for x0, y0, x1, y1, c in bars:
                        if inside_round_rect(x, y, x0, y0, x1, y1, (y1 - y0) * 0.35):
                            col = c
                    acc[0] += col[0]
                    acc[1] += col[1]
                    acc[2] += col[2]
                    acc[3] += 255
            n = ss * ss
            i = (py * size + pxl) * 4
            a = acc[3] / n
            if a:
                px[i:i + 4] = bytes([int(acc[0] / (acc[3] / 255)), int(acc[1] / (acc[3] / 255)),
                                     int(acc[2] / (acc[3] / 255)), int(a)])
    return bytes(px)


def png(size, rgba):
    raw = b"".join(b"\0" + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    images = [(s, png(s, draw(s))) for s in sizes]
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, data = b"", b""
    for s, blob in images:
        entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(blob), offset + len(data))
        data += blob
    with open(os.path.join(here, "icon.ico"), "wb") as f:
        f.write(header + entries + data)
    with open(os.path.join(here, "icon64.rgba"), "wb") as f:
        f.write(draw(64))
    with open(os.path.join(here, "icon256.png"), "wb") as f:
        f.write(dict(images)[256])
    print("wrote icon.ico, icon64.rgba, icon256.png")


if __name__ == "__main__":
    main()
