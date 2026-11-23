#!/usr/bin/env python3
"""Bakes the device bodies' drop shadow (design: 0 16 40 #00000033) into
dithered PNGs at 2x. A 20 % black shadow on the window's near-black leaves
only a few color steps across its fade, which a GPU draws as bands; small
noise in the alpha dithers those steps away.

Writes assets/device/shadow-<model>.png. Each image covers the body plus
MARGIN on every side (at 1x), shifted down by the shadow's offset."""
import math, os, random, struct, zlib

SCALE = 2
MARGIN = 80          # px at 1x: 4 sigma
OFFSET_Y = 16
SIGMA = 20.0         # CSS blur radius 40 = 2 sigma
ALPHA = 0x33 / 255
NOISE = 5 / 255      # about half a color level after blending

MODELS = {           # body width, height, corner radius (from the .pen file)
    'ipad-home': (260, 367, 22),
    'ipad-no-home': (265, 367, 27),
    'iphone-home': (178, 367, 26),
    'iphone-no-home': (181, 367, 30),
}

def sdf(px, py, w, h, r):
    qx = abs(px) - (w / 2 - r)
    qy = abs(py) - (h / 2 - r)
    outside = math.hypot(max(qx, 0), max(qy, 0))
    return outside + min(max(qx, qy), 0) - r

def png(path, width, height, rows):
    raw = b''.join(b'\x00' + row for row in rows)
    chunk = lambda t, d: struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
    data = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0))
    data += chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b'')
    open(path, 'wb').write(data)

def bake(name, w, h, r, out):
    rng = random.Random(name)
    cw, ch = (w + 2 * MARGIN) * SCALE, (h + 2 * MARGIN) * SCALE
    rows = []
    for y in range(ch):
        row = bytearray()
        py = (y + 0.5) / SCALE - MARGIN - h / 2 - OFFSET_Y
        for x in range(cw):
            px = (x + 0.5) / SCALE - MARGIN - w / 2
            d = sdf(px, py, w, h, r)
            a = ALPHA * 0.5 * math.erfc(d / (SIGMA * math.sqrt(2)))
            if a > 0.0005:
                a += (rng.random() * 2 - 1) * NOISE * min(1.0, a / ALPHA * 4)
            row += bytes((0, 0, 0, max(0, min(255, round(a * 255)))))
        rows.append(bytes(row))
    png(os.path.join(out, f'shadow-{name}.png'), cw, ch, rows)

if __name__ == '__main__':
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'assets', 'device')
    for name, (w, h, r) in MODELS.items():
        bake(name, w, h, r, out)
        print('baked', name)
