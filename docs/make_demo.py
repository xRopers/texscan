"""A demo archive of made-up textures for README screenshots: nothing copyrighted.

Usage: python docs/make_demo.py demo.pak (then open it in texscan-gui).

Writes demo.pak: a small header, then DDS textures (with mips) separated by filler.
Includes a tiny BC1/BC3/BC5 encoder (min/max endpoints), good enough for a demo.
"""
import math
import random
import struct
import sys

random.seed(11)
OUT = sys.argv[1]


def mips_of(img, w, h):
    """img: list of rows of (r,g,b,a) floats 0..1. Returns list of (w,h,img) halving."""
    out = [(w, h, img)]
    while w > 1 or h > 1:
        nw, nh = max(1, w // 2), max(1, h // 2)
        new = []
        for y in range(nh):
            row = []
            for x in range(nw):
                px = [img[min(h - 1, 2 * y + dy)][min(w - 1, 2 * x + dx)] for dy in (0, 1) for dx in (0, 1)]
                row.append(tuple(sum(p[c] for p in px) / 4 for c in range(4)))
            new.append(row)
        w, h, img = nw, nh, new
        out.append((w, h, img))
    return out


def q(v, bits):
    return max(0, min((1 << bits) - 1, round(v * ((1 << bits) - 1))))


def rgb565(c):
    return (q(c[0], 5) << 11) | (q(c[1], 6) << 5) | q(c[2], 5)


def unpack565(v):
    return ((v >> 11) / 31, ((v >> 5) & 63) / 63, (v & 31) / 31)


def blocks(img, w, h):
    for by in range(0, h, 4):
        for bx in range(0, w, 4):
            yield [img[min(h - 1, by + y)][min(w - 1, bx + x)] for y in range(4) for x in range(4)]


def color_block(px):
    lum = [0.3 * p[0] + 0.59 * p[1] + 0.11 * p[2] for p in px]
    hi, lo = px[lum.index(max(lum))], px[lum.index(min(lum))]
    c0, c1 = rgb565(hi), rgb565(lo)
    if c0 < c1:
        c0, c1 = c1, c0
    if c0 == c1:
        return struct.pack('<HHI', c0, c1, 0)
    e0, e1 = unpack565(c0), unpack565(c1)
    pal = [e0, e1, tuple((2 * a + b) / 3 for a, b in zip(e0, e1)), tuple((a + 2 * b) / 3 for a, b in zip(e0, e1))]
    idx = 0
    for i, p in enumerate(px):
        best = min(range(4), key=lambda k: sum((p[c] - pal[k][c]) ** 2 for c in range(3)))
        idx |= best << (2 * i)
    return struct.pack('<HHI', c0, c1, idx)


def alpha_block(vals):
    a0, a1 = q(max(vals), 8), q(min(vals), 8)
    if a0 == a1:
        return bytes([a0, a1]) + bytes(6)
    pal = [a0, a1] + [((7 - k) * a0 + k * a1) / 7 for k in range(1, 7)]
    bits = 0
    for i, v in enumerate(vals):
        best = min(range(8), key=lambda k: abs(v * 255 - pal[k]))
        bits |= best << (3 * i)
    return bytes([a0, a1]) + bits.to_bytes(6, 'little')


def encode(fmt, img, w, h):
    if fmt == 'bc1':
        return b''.join(color_block(b) for b in blocks(img, w, h))
    if fmt == 'bc3':
        return b''.join(alpha_block([p[3] for p in b]) + color_block(b) for b in blocks(img, w, h))
    if fmt == 'bc5':
        return b''.join(alpha_block([p[0] for p in b]) + alpha_block([p[1] for p in b]) for b in blocks(img, w, h))
    if fmt == 'rgba8':
        return bytes(q(v, 8) for row in img for p in row for v in p)
    if fmt == 'bgra8':
        return bytes(q(v, 8) for row in img for p in row for v in (p[2], p[1], p[0], p[3]))
    if fmt == 'l8':
        return bytes(q(p[0], 8) for row in img for p in row)
    if fmt == 'rgba16f':
        return b''.join(struct.pack('<4e', *p) for row in img for p in row)
    raise ValueError(fmt)


DXGI = {'bc1': 72, 'bc3': 78, 'bc5': 83, 'rgba8': 29, 'rgba16f': 10}
FOURCC = {'bc1': b'DXT1', 'bc3': b'DXT5', 'bc5': b'ATI2'}


def dds(fmt, faces_imgs, w, h, depth=1, legacy=True, cube=False, array=1, mip=True):
    """faces_imgs: list of images (layers), or for volume a list of slices."""
    head = bytearray(128)
    head[:4] = b'DDS '

    def put(pos, v):
        head[pos:pos + 4] = struct.pack('<I', v)

    levels = (max(w, h, depth)).bit_length() if mip else 1
    put(4, 124); put(8, 0x1007 | 0x20000 | (0x800000 if depth > 1 else 0)); put(12, h); put(16, w)
    put(24, depth if depth > 1 else 0); put(28, levels); put(76, 32)
    dx10 = b''
    if fmt in ('bgra8', 'l8') and legacy:
        if fmt == 'bgra8':
            put(80, 0x41); put(88, 32); put(92, 0xff0000); put(96, 0xff00); put(100, 0xff); put(104, 0xff000000)
        else:
            put(80, 0x20000); put(88, 8); put(92, 0xff)
    elif legacy and fmt in FOURCC:
        put(80, 4); head[84:88] = FOURCC[fmt]
    else:
        put(80, 4); head[84:88] = b'DX10'
        dx10 = struct.pack('<5I', DXGI[fmt], 4 if depth > 1 else 3, 4 if cube else 0, array, 0)
    put(108, 0x1000 | (0x400008 if levels > 1 else 0) | (0x8 if cube else 0))
    if legacy and cube:
        put(112, 0xFE00)
    if depth > 1:
        put(112, 0x200000)
    body = b''
    if depth > 1:
        # Volume: each mip holds all its slices.
        slices = [mips_of(s, w, h) for s in faces_imgs]
        for m in range(levels):
            d = max(1, depth >> m)
            for z in range(d):
                src = slices[min(depth - 1, z * (depth // d))][min(m, len(slices[0]) - 1)]
                body += encode(fmt, src[2], src[0], src[1])
    else:
        for img in faces_imgs:
            for (mw, mh, mi) in mips_of(img, w, h)[:levels]:
                body += encode(fmt, mi, mw, mh)
    return bytes(head) + dx10 + body


def image(w, h, f):
    return [[f(x / w, y / h) for x in range(w)] for y in range(h)]


def noise(seed):
    rnd = random.Random(seed)
    grid = [[rnd.random() for _ in range(17)] for _ in range(17)]

    def at(u, v, scale):
        x, y = (u * scale) % 16, (v * scale) % 16
        x0, y0 = int(x), int(y)
        fx, fy = x - x0, y - y0
        fx, fy = fx * fx * (3 - 2 * fx), fy * fy * (3 - 2 * fy)
        a = grid[y0][x0] * (1 - fx) + grid[y0][x0 + 1] * fx
        b = grid[y0 + 1][x0] * (1 - fx) + grid[y0 + 1][x0 + 1] * fx
        return a * (1 - fy) + b * fy

    return lambda u, v: sum(at(u, v, 4 * 2 ** o) / 2 ** o for o in range(4)) / 1.875


n1, n2, n3 = noise(1), noise(2), noise(3)


def bricks(u, v):
    rows = 8
    y = v * rows
    x = u * 4 + (0.5 if int(y) % 2 else 0)
    mortar = min(y % 1, 1 - y % 1) < 0.06 or min(x % 1, 1 - x % 1) < 0.03
    k = n1(u, v)
    if mortar:
        g = 0.62 + 0.1 * k
        return (g, g, g * 0.95, 1)
    tint = n2(int(x) / 4, int(y) / rows)
    return (0.55 + 0.25 * tint + 0.1 * k, 0.22 + 0.1 * tint + 0.08 * k, 0.16 + 0.06 * k, 1)


def marble(u, v):
    t = math.sin((u + 2.5 * n3(u, v)) * 9)
    g = 0.78 + 0.18 * t
    return (g, g * 0.97, g * 0.93, 1)


def height(u, v):
    return n1(u * 2, v * 2)


def normal(u, v):
    e = 1 / 256
    dx = (height(u + e, v) - height(u - e, v)) * 6
    dy = (height(u, v + e) - height(u, v - e)) * 6
    ln = math.sqrt(dx * dx + dy * dy + 1)
    return (-dx / ln * 0.5 + 0.5, -dy / ln * 0.5 + 0.5, 0, 1)


def orb(u, v):
    d = math.hypot(u - 0.5, v - 0.5) * 2
    a = max(0.0, min(1.0, (1 - d) * 3))
    glow = max(0.0, 1 - d)
    return (1.0, 0.55 + 0.45 * glow, 0.15 + 0.6 * glow ** 3, a)


def sky_face(face):
    def f(u, v):
        # Rough sky: up bright, down dark, horizon band; each side slightly tinted.
        tint = [(1, 0.95, 0.9), (0.9, 0.95, 1), (1, 1, 1), (0.35, 0.3, 0.25), (0.95, 1, 0.95), (1, 0.9, 1)][face]
        if face == 2:
            k = 0.55 + 0.45 * (1 - math.hypot(u - .5, v - .5))
        elif face == 3:
            k = 0.4 + 0.2 * n1(u, v)
        else:
            k = 0.35 + 0.6 * (1 - v) + 0.1 * n2(u + face, v)
        cloud = max(0, n3(u + face, v * 0.6) - 0.45) * 2 if face != 3 and v < 0.55 else 0
        base = (0.35 * k + cloud, 0.55 * k + cloud, 0.95 * k + cloud * 0.9)
        return tuple(min(1.0, c * t) for c, t in zip(base, tint)) + (1,)
    return f


def hdr_sun(u, v):
    d = math.hypot(u - 0.5, v - 0.35)
    sun = 12 * math.exp(-d * d * 400)
    sky = 0.2 + 0.8 * (1 - v)
    return (0.4 * sky + sun, 0.6 * sky + sun * 0.9, 1.0 * sky + sun * 0.7, 1)


def slice_fn(z):
    return lambda u, v: (lambda g: (g, g, g, 1))(n1(u + z * 0.13, v + z * 0.07))


parts = [
    dds('bc1', [image(256, 256, bricks)], 256, 256),
    dds('bc5', [image(256, 256, normal)], 256, 256),
    dds('bgra8', [image(128, 128, marble)], 128, 128),
    dds('bc3', [image(128, 128, orb)], 128, 128),
    dds('rgba8', [image(64, 64, sky_face(f)) for f in range(6)], 64, 64, legacy=False, cube=True),
    dds('rgba16f', [image(64, 64, hdr_sun)], 64, 64, legacy=False),
    dds('l8', [image(32, 32, slice_fn(z)) for z in range(8)], 32, 32, depth=8),
    dds('bc1', [image(64, 64, lambda u, v, k=k: (0.2 + 0.2 * k, 0.5 + 0.1 * k, 0.9 - 0.15 * k, 1) if (int(u * 8) + int(v * 8)) % 2 else (0.95, 0.95, 0.9, 1)) for k in range(3)], 64, 64, legacy=False, array=3),
    dds('bc1', [image(512, 256, lambda u, v: bricks(u * 2 % 1, v))], 512, 256, legacy=False),
]
out = bytearray(b'DEMOPAK\x00' + struct.pack('<I', len(parts)) + bytes(52))
for p in parts:
    out += p
    out += bytes(random.getrandbits(8) for _ in range(random.randint(32, 512)))
open(OUT, 'wb').write(out)
print(len(parts), 'textures,', len(out), 'bytes')
