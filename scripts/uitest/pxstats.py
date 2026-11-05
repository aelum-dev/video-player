#!/usr/bin/env python3
"""pxstats — brightness/lit statistics for a rectangle of a PPM image.

usage: pxstats.py image.ppm [x y w h]
Prints: size, mean/max per channel, share of "lit" pixels (any channel > 16),
and a coarse colour histogram. Used to check objectively whether the video
actually reaches the screen (display path), not just the FBO.
"""
import sys


def read_ppm(path):
    with open(path, "rb") as f:
        if f.readline().strip() != b"P6":
            raise SystemExit("not a P6 ppm")
        line = f.readline()
        while line.startswith(b"#"):
            line = f.readline()
        w, h = (int(v) for v in line.split())
        f.readline()  # maxval
        data = f.read()
    return w, h, data


def main():
    path = sys.argv[1]
    w, h, data = read_ppm(path)
    x, y, rw, rh = 0, 0, w, h
    if len(sys.argv) >= 6:
        x, y, rw, rh = (int(v) for v in sys.argv[2:6])
    x = max(0, min(x, w))
    y = max(0, min(y, h))
    rw = max(1, min(rw, w - x))
    rh = max(1, min(rh, h - y))

    lit = 0
    total = rw * rh
    hist = {}
    ssum = [0, 0, 0]
    mx = 0
    for row in range(y, y + rh):
        base = (row * w + x) * 3
        for col in range(rw):
            i = base + col * 3
            r, g, b = data[i], data[i + 1], data[i + 2]
            peak = max(r, g, b)
            ssum[0] += r
            ssum[1] += g
            ssum[2] += b
            if peak > mx:
                mx = peak
            if peak > 16:
                lit += 1
                hist[(r // 32, g // 32, b // 32)] = hist.get((r // 32, g // 32, b // 32), 0) + 1
    print(f"rect={x},{y} {rw}x{rh} lit={lit}/{total} ({100.0*lit/total:.1f}%) max={mx} "
          f"mean=({ssum[0]//total},{ssum[1]//total},{ssum[2]//total})")
    top = sorted(hist.items(), key=lambda kv: -kv[1])[:5]
    for (r, g, b), n in top:
        print(f"  bucket rgb~({r*32},{g*32},{b*32}) count={n}")


main()
