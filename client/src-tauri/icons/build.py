#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Draw the Øphio mark and render every icon made from it.

The mark is an ø seen from under a cap: gills fill the O, and the slash is an
uneven cord that frays into hyphae where it leaves the ring. One geometry feeds the
app icon, Android's adaptive and themed launcher layers, the alert icon, which
is a bolder single-colour cut of it, and the icons of the computer program.
Below 64 pixels the tiles carry a small cut: that bold ring and cord in colour,
over a few broad gills.

Needs rsvg-convert. Run from anywhere; it rewrites source.svg, icon.png, the
Android rasters and packaging/icons in place. With --site DIR it also writes
the website's icons into DIR, the site's public folder.
"""
import argparse
import math
import re
import struct
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
RES = HERE.parent / "gen/android/app/src/main/res"
PLUGIN_RES = HERE.parent / "plugins/device/android/src/main/res"
DESKTOP = HERE.parents[2] / "packaging/icons"

GREEN, SPORE, VIOLET, DEEP = "#c3ff24", "#dfbeff", "#ac71df", "#6b3a8f"
GROUND, GLOW, UNDERSIDE, WHITE = "#0a0605", "#2c1438", "#1b0c25", "#ffffff"
DENSITIES = {"mdpi": 1, "hdpi": 1.5, "xhdpi": 2, "xxhdpi": 3, "xxxhdpi": 4}

# The slash leans 33° from upright through the centre of a 1024 canvas.
THETA = math.radians(33)
U = (math.sin(THETA), -math.cos(THETA))
N = (math.cos(THETA), math.sin(THETA))
CX, CY, RX, RY = 512, 528, 172, 202
STEPS = 140


def add(a, b, k=1.0):
    return (a[0] + b[0] * k, a[1] + b[1] * k)


def along(t, off=0.0):
    return add(add((CX, CY), U, t), N, off)


def rot(v, degrees):
    c, s = math.cos(math.radians(degrees)), math.sin(math.radians(degrees))
    return (v[0] * c - v[1] * s, v[0] * s + v[1] * c)


def heading(pts, i):
    a, b = pts[max(i - 1, 0)], pts[min(i + 1, len(pts) - 1)]
    length = math.hypot(b[0] - a[0], b[1] - a[1]) or 1
    return ((b[0] - a[0]) / length, (b[1] - a[1]) / length)


def xy(p):
    return f"{p[0]:.1f} {p[1]:.1f}"


def ribbon(pts, widths, color):
    """A filled stroke through pts, as wide as widths at each, round at both ends."""
    left, right = [], []
    for i, p in enumerate(pts):
        h = heading(pts, i)
        w = widths[i] / 2
        left.append((p[0] - h[1] * w, p[1] + h[0] * w))
        right.append((p[0] + h[1] * w, p[1] - h[0] * w))
    outline = "M " + " L ".join(xy(p) for p in left + right[::-1]) + " Z"
    ends = "".join(
        f'<circle cx="{p[0]:.1f}" cy="{p[1]:.1f}" r="{w / 2:.1f}" fill="{color}"/>'
        for p, w in ((pts[0], widths[0]), (pts[-1], widths[-1]))
    )
    return f'<path d="{outline}" fill="{color}"/>{ends}'


def ring(color, side, top):
    """The O, heavier at its sides than at its top and bottom."""
    def ellipse(rx, ry):
        return f"M {CX - rx} {CY} a {rx} {ry} 0 1 0 {2 * rx} 0 a {rx} {ry} 0 1 0 {-2 * rx} 0 Z"
    outer = ellipse(RX + side / 2, RY + top / 2)
    inner = ellipse(RX - side / 2, RY - top / 2)
    return f'<path d="{outer} {inner}" fill="{color}" fill-rule="evenodd"/>'


def gills(count=96):
    """The underside of a cap: long, middling and short gills in turn."""
    out = [f'<ellipse cx="{CX}" cy="{CY}" rx="{RX}" ry="{RY}" fill="{UNDERSIDE}"/>']
    for i in range(count):
        a = 2 * math.pi * i / count + 0.02
        kind = 0 if i % 4 == 0 else (2 if i % 2 == 0 else 1)
        start = (0.16, 0.66, 0.42)[kind]
        width = (4.5, 2.6, 3.5)[kind]
        color = (SPORE, DEEP, VIOLET)[kind]

        def at(r, da):
            return (CX + RX * r * math.cos(a + da), CY + RY * r * math.sin(a + da))

        bend = at((start + 0.92) / 2, 0.05)
        out.append(
            f'<path d="M {xy(at(start, 0.16))} Q {xy(bend)} {xy(at(0.92, 0))}" fill="none" '
            f'stroke="{color}" stroke-width="{width}" stroke-linecap="round"/>'
        )
    return "".join(out)


def few_gills(count=24):
    """Gills broad enough to see at 24 pixels: one weight, long and short in turn."""
    out = [f'<ellipse cx="{CX}" cy="{CY}" rx="{RX}" ry="{RY}" fill="{UNDERSIDE}"/>']
    for i in range(count):
        a = 2 * math.pi * i / count + 0.2
        start, color = (0.2, VIOLET) if i % 2 == 0 else (0.5, DEEP)

        def at(r):
            return (CX + RX * r * math.cos(a), CY + RY * r * math.sin(a))

        out.append(f'<path d="M {xy(at(start))} L {xy(at(0.95))}" stroke="{color}" '
                   f'stroke-width="18" stroke-linecap="round"/>')
    return "".join(out)


def hypha(parts, start, direction, length, w0, curl, branches=()):
    """A thread that thins as it grows and curls by `curl` degrees on the way."""
    pts, p, h, n = [start], start, direction, 30
    for _ in range(n):
        h = rot(h, curl / n)
        p = add(p, h, length / n)
        pts.append(p)
    widths = [w0 + (2.0 - w0) * (i / n) ** 0.7 for i in range(n + 1)]
    parts.append((pts, widths))
    for at, side, angle, scale, bend in branches:
        j = round(at * n)
        hypha(parts, pts[j], rot(heading(pts, j), side * angle), length * scale,
              max(widths[j] * 0.8, 2.4), bend)


def bump(x, at, spread):
    return math.exp(-((x - at) / spread) ** 2)


# Where the cord swells (+) and pinches (-), from its lower end (-1) to its
# upper (1): a belly low in the O, a pinch above it, a knot where it crosses
# the ring at the top, and a thinner crossing at the bottom.
SWELLS = ((-0.8, 0.07, 0.26), (-0.6, 0.1, -0.4), (-0.16, 0.24, 0.48),
          (0.3, 0.12, -0.62), (0.64, 0.1, 0.52))


def cord(w_mid, w_end, swell=1.0):
    """The slash: a gentle S, tapering to its ends, uneven along the way."""
    pts, widths = [], []
    for i in range(STEPS + 1):
        u = -1 + 2 * i / STEPS
        pts.append(along(u * 300, 12 * math.sin(math.pi * u)))
        k = 1 + swell * sum(size * bump(u, at, spread) for at, spread, size in SWELLS)
        widths.append((w_end + (w_mid - w_end) * (1 - u * u) ** 0.6) * k)
    return pts, widths


def outward(pts, end):
    """Index and direction of travel at one end of the cord."""
    i = STEPS if end > 0 else 0
    h = heading(pts, i)
    return i, h if end > 0 else (-h[0], -h[1])


def slash():
    pts, widths = cord(46, 16)
    parts = [(pts, widths)]

    def vein(u, side, angle, length, w0, curl, branches=()):
        # Veins lean toward the end they grow from, so the mark keeps its
        # half-turn symmetry.
        i = round((u + 1) / 2 * STEPS)
        h = heading(pts, i)
        h = h if u > 0 else (-h[0], -h[1])
        start = add(pts[i], rot(h, side * 90), widths[i] * 0.25)
        hypha(parts, start, rot(h, side * angle), length, w0, -side * curl, branches)

    for end in (1, -1):
        i, h = outward(pts, end)
        for angle, length, w0, curl, branches in (
            (-46, 84, 8, -26, ((0.5, -1, 34, 0.42, -14),)),
            (-16, 112, 10, -12, ((0.58, 1, 30, 0.36, 14),)),
            (42, 78, 8, 28, ((0.55, 1, 36, 0.4, 16),)),
        ):
            hypha(parts, add(pts[i], h, -12), rot(h, angle), length, w0, curl, branches)
        vein(0.7 * end, 1, 58, 104, 8, 30, ((0.55, 1, 36, 0.36, 12),))
        vein(0.78 * end, -1, 56, 88, 7, 30)
    return parts


def bold_slash():
    """The slash for small single-colour icons: heavier, evener, ends forked."""
    pts, widths = cord(72, 44, swell=0.5)
    parts = [(pts, widths)]
    for end in (1, -1):
        i, h = outward(pts, end)
        for angle in (-28, 26):
            hypha(parts, pts[i], rot(h, angle), 64, 34, -angle * 0.4)
    return parts


def draw(parts, color, halo=0):
    return "".join(ribbon(p, [w + halo for w in ws], color) for p, ws in parts)


def mark():
    parts = slash()
    # A dark margin lifts the cord off the gills and the ring it crosses.
    return gills() + ring(GREEN, 60, 44) + draw(parts, GROUND, 10) + draw(parts, GREEN)


def small_mark(size):
    """The mark for 16 to 48 pixels; under 24 the gills give way to plain violet."""
    inside = (few_gills() if size >= 24
              else f'<ellipse cx="{CX}" cy="{CY}" rx="{RX}" ry="{RY}" fill="{DEEP}"/>')
    parts = bold_slash()
    return inside + ring(GREEN, 86, 70) + draw(parts, GROUND, 14) + draw(parts, GREEN)


def glyph():
    return ring(WHITE, 86, 70) + draw(bold_slash(), WHITE)


def placed(body, scale, dx=0, dy=0):
    return (f'<g transform="translate({512 + dx} {512 + dy}) scale({scale}) '
            f'translate(-512 -512)">{body}</g>')


def svg(content, defs=""):
    return ('<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" '
            f'viewBox="0 0 1024 1024">{defs}{content}</svg>')


GLOW_DEFS = (f'<radialGradient id="glow" cx=".5" cy=".46" r=".62"><stop stop-color="{GLOW}"/>'
             f'<stop offset=".72" stop-color="{GROUND}"/></radialGradient>')


CLIPS = {
    "square": '<rect width="1024" height="1024" rx="224"/>',
    "circle": '<circle cx="512" cy="512" r="512"/>',
    # For systems that cut their own shape.
    "full": '<rect width="1024" height="1024"/>',
}


def tile(shape, size=1024):
    """The whole icon on its ground, clipped to shape, cut for size in pixels."""
    body = placed(small_mark(size), 1.36, 0, -26) if size < 64 else placed(mark(), 1.12, 0, -19)
    defs = f'<defs>{GLOW_DEFS}<clipPath id="shape">{CLIPS[shape]}</clipPath></defs>'
    return svg(f'<g clip-path="url(#shape)"><rect width="1024" height="1024" fill="url(#glow)"/>'
               f'{body}</g>', defs)


# Adaptive layers are 108dp; launchers show the middle 72dp and keep the
# middle 66dp whatever their mask, so the mark stays inside that circle.
def foreground():
    return svg(placed(mark(), 0.82, 0, -14))


def monochrome():
    return svg(placed(glyph(), 0.7, 0, -14))


# Alert icons are 24dp, white on clear; the system tints them.
def alert():
    return svg(placed(glyph(), 1.5, 0, -24))


# The web's maskable icons keep what matters inside a circle 80% across.
def maskable():
    return svg(f'<rect width="1024" height="1024" fill="{GROUND}"/>'
               f'<rect width="1024" height="1024" fill="url(#glow)"/>'
               f'{placed(mark(), 0.95, 0, -16)}', f"<defs>{GLOW_DEFS}</defs>")


def emblem():
    """The mark alone on a clear ground, cropped close, to set beside a wordmark."""
    parts = slash()
    edges = [(p[0] + sx * (w / 2 + 5), p[1] + sy * (w / 2 + 5))
             for pts, ws in parts for p, w in zip(pts, ws) for sx in (-1, 1) for sy in (-1, 1)]
    edges += [(CX - RX - 30, CY - RY - 22), (CX + RX + 30, CY + RY + 22)]
    left, top = (math.floor(min(e[i] for e in edges)) for i in (0, 1))
    right, bottom = (math.ceil(max(e[i] for e in edges)) for i in (0, 1))
    return compact(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{left} {top} '
                   f'{right - left} {bottom - top}">{mark()}</svg>')


def compact(svg_source):
    """Whole units for the web: a third smaller, and no different at a page's sizes."""
    return re.sub(r'(?<!stroke-width=")\b\d+\.\d+', lambda m: str(round(float(m[0]))), svg_source)


def raster(source, size):
    return subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size)],
                          input=source.encode(), capture_output=True, check=True).stdout


def render(source, path, size):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(raster(source, size))


def ico(path, sizes):
    """A Windows icon file holding the square tile as a PNG at each size."""
    images = [(size, raster(tile("square", size), size)) for size in sizes]
    offset = 6 + 16 * len(images)
    entries, data = [], b""
    for size, png in images:
        # The directory has a byte for each side; 0 means 256.
        entries.append(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32,
                                   len(png), offset + len(data)))
        data += png
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(struct.pack("<HHH", 0, 1, len(images)) + b"".join(entries) + data)


def generated(svg_source):
    return ("<!-- SPDX-License-Identifier: Apache-2.0 -->\n"
            "<!-- Generated by build.py; edit that, not this. -->\n" + svg_source + "\n")


def desktop():
    """The computer program's icons: Windows' and the Linux icon theme's."""
    ico(DESKTOP / "ophio.ico", (16, 20, 24, 32, 40, 48, 64, 256))
    for size in (16, 24, 32, 48, 64, 128, 256, 512):
        render(tile("square", size), DESKTOP / f"hicolor/{size}x{size}/apps/ophio.png", size)
    scalable = DESKTOP / "hicolor/scalable/apps/ophio.svg"
    scalable.parent.mkdir(parents=True, exist_ok=True)
    scalable.write_text(generated(tile("square")))


def site(public):
    """The website's tab, home-screen and manifest icons, and its header mark."""
    (public / "favicon.svg").write_text(compact(tile("square", 32)) + "\n")
    ico(public / "favicon.ico", (16, 32, 48))
    render(tile("full", 180), public / "apple-touch-icon.png", 180)
    for size in (192, 512):
        render(tile("square", size), public / f"icon-{size}.png", size)
    render(maskable(), public / "icon-maskable-512.png", 512)
    (public / "art/mark.svg").write_text(emblem() + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--site", type=Path, help="also write the website's icons here")
    args = parser.parse_args()
    square = tile("square")
    (HERE / "source.svg").write_text(generated(square))
    render(square, HERE / "icon.png", 512)
    layers = {
        "ic_launcher": (square, 48),
        "ic_launcher_round": (tile("circle"), 48),
        "ic_launcher_foreground": (foreground(), 108),
        "ic_launcher_monochrome": (monochrome(), 108),
    }
    for density, scale in DENSITIES.items():
        for name, (source, size) in layers.items():
            render(source, RES / f"mipmap-{density}/{name}.png", round(size * scale))
        render(alert(), PLUGIN_RES / f"drawable-{density}/ic_stat_ophio.png", round(24 * scale))
    desktop()
    if args.site:
        site(args.site)


if __name__ == "__main__":
    main()
