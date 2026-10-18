#!/usr/bin/env python3
"""Render Krust's brand assets: the Strata app icon and the cartoon wordmark.

Run from the repo root:

    python3 resources/brand/render_brand.py [--site-wordmark PATH]

Writes:
  app/channels/oss/icon/no-padding/512x512.png  app icon (macOS .icns + Linux)
  app/channels/oss/icon/no-padding/icon.ico     app icon (Windows)
  app/assets/bundled/png/krust-logo.png         wordmark (About page)
  app/assets/bundled/png/krust-crusty.png       Crusty face (swear-mode Get Started screen)
  PATH (optional)                               the same wordmark, for the website

Needs Pillow. The wordmark font is Fredoka (SIL OFL 1.1, see fonts/Fredoka/OFL.txt).
"""
import argparse
import os

from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
FONT = os.path.join(ROOT, "resources/brand/fonts/Fredoka/Fredoka[wdth,wght].ttf")

# Icon geometry is authored on a 1024 grid; the squircle spans 64..960. The
# "no-padding" icon slot expects the squircle to fill the whole canvas.
SS = 4  # supersampling factor
SQ0, SQ1, SQ_RADIUS = 64, 960, 200

INK = "#1E1714"
CREAM = "#F6EBDD"
EMBER = "#F26B1D"
BANDS = [  # (color, start, [(c1, c2, end), ...]) -- cubic segments, then down to the bottom
    ("#8E3B1E", (64, 600), [((260, 556), (430, 640), (620, 592)), ((810, 544), (900, 560), (960, 596))]),
    ("#C25A26", (64, 692), [((250, 650), (470, 734), (660, 684)), ((850, 634), (900, 664), (960, 700))]),
    ("#E3953A", (64, 782), [((300, 742), (520, 822), (720, 772)), ((920, 722), (920, 762), (960, 790))]),
    ("#F6CC5E", (64, 870), [((280, 842), (560, 902), (760, 862)), ((960, 822), (930, 852), (960, 872))]),
]
# k_ : the k in cream, the underscore cursor in ember (round-capped strokes).
STROKE = 58
K_STROKES = [((310, 216), (310, 468)), ((318, 404), (440, 316)), ((362, 372), (442, 468))]
CURSOR = ((516, 468), (676, 468))


def s(p):
    return (p[0] * SS, p[1] * SS)


def cubic(p0, p1, p2, p3, n=96):
    pts = []
    for i in range(1, n + 1):
        t = i / n
        mt = 1 - t
        pts.append((
            mt ** 3 * p0[0] + 3 * mt * mt * t * p1[0] + 3 * mt * t * t * p2[0] + t ** 3 * p3[0],
            mt ** 3 * p0[1] + 3 * mt * mt * t * p1[1] + 3 * mt * t * t * p2[1] + t ** 3 * p3[1],
        ))
    return pts


def stroke(draw, a, b, color, width):
    a, b = s(a), s(b)
    w = width * SS
    draw.line([a, b], fill=color, width=w)
    r = w / 2
    for x, y in (a, b):
        draw.ellipse([x - r, y - r, x + r, y + r], fill=color)


def render_icon():
    size = 1024 * SS
    art = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(art)
    d.rectangle([s((SQ0, SQ0)), s((SQ1, SQ1))], fill=INK)
    for color, start, segments in BANDS:
        pts, cur = [start], start
        for c1, c2, end in segments:
            pts += cubic(cur, c1, c2, end)
            cur = end
        pts += [(960, 960), (64, 960)]
        d.polygon([s(p) for p in pts], fill=color)
    for a, b in K_STROKES:
        stroke(d, a, b, CREAM, STROKE)
    stroke(d, CURSOR[0], CURSOR[1], EMBER, STROKE)

    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle([s((SQ0, SQ0)), s((SQ1, SQ1))], radius=SQ_RADIUS * SS, fill=255)
    clipped = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    clipped.paste(art, (0, 0), mask)
    return clipped.crop((SQ0 * SS, SQ0 * SS, SQ1 * SS, SQ1 * SS))


def ring(draw, center, r, width, color):
    """A circle outline centred on radius r (SVG-style centred stroke)."""
    cx, cy = s(center)
    outer, inner = (r + width / 2) * SS, (r - width / 2) * SS
    draw.ellipse([cx - outer, cy - outer, cx + outer, cy + outer], fill=color)
    return inner


def thick_path(draw, pts, color, width):
    """Round-capped, round-joined polyline through pts (1024-grid coords)."""
    scaled = [s(p) for p in pts]
    w = width * SS
    draw.line(scaled, fill=color, width=w, joint="curve")
    r = w / 2
    for x, y in (scaled[0], scaled[-1]):
        draw.ellipse([x - r, y - r, x + r, y + r], fill=color)


def quadratic(p0, p1, p2, n=64):
    return [((1 - t) ** 2 * p0[0] + 2 * (1 - t) * t * p1[0] + t * t * p2[0],
             (1 - t) ** 2 * p0[1] + 2 * (1 - t) * t * p1[1] + t * t * p2[1])
            for t in (i / n for i in range(n + 1))]


def render_crusty(size):
    """Crusty: a googly-eyed terminal window with its tongue out (swear mode)."""
    canvas = 1024 * SS
    img = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    line = "#161211"
    d.rounded_rectangle([s((SQ0, SQ0)), s((SQ1, SQ1))], radius=SQ_RADIUS * SS, fill=EMBER)
    # Window: 30px outline centred on the 200,250 624x500 r72 rect.
    d.rounded_rectangle([s((185, 235)), s((839, 765))], radius=87 * SS, fill=line)
    d.rounded_rectangle([s((215, 265)), s((809, 735))], radius=57 * SS, fill="#FFF4E6")
    for x in (270, 320, 370):
        d.ellipse([s((x - 16, 290)), s((x + 16, 322))], fill=line)
    # Googly eyes, cross-eyed: white with a 26px outline, pupils pulled inward.
    for eye, pupil in (((418, 496), (452, 518)), ((606, 496), (572, 518))):
        inner = ring(d, eye, 92, 26, line)
        cx, cy = s(eye)
        d.ellipse([cx - inner, cy - inner, cx + inner, cy + inner], fill="#FFFFFF")
        d.ellipse([s((pupil[0] - 38, pupil[1] - 38)), s((pupil[0] + 38, pupil[1] + 38))], fill=line)
    thick_path(d, quadratic((430, 644), (512, 694), (600, 644)), line, 24)
    # Tongue: the cubic is outlined, its open top edge tucks under the mouth.
    tongue = [(516, 662)] + cubic((516, 662), (512, 800), (626, 800), (618, 652))
    d.polygon([s(p) for p in tongue], fill="#F28AA8")
    thick_path(d, tongue, line, 22)
    thick_path(d, [(567, 684), (567, 738)], "#C9567A", 10)
    return img.crop((SQ0 * SS, SQ0 * SS, SQ1 * SS, SQ1 * SS)).resize((size, size), Image.LANCZOS)


def render_wordmark(height):
    """White Fredoka Bold with a heavy black outline and a soft drop shadow."""
    big = height * 4
    font = ImageFont.truetype(FONT, big)
    names = [a["name"].decode() if isinstance(a["name"], bytes) else a["name"]
             for a in font.get_variation_axes()]
    font.set_variation_by_axes([700 if "weight" in n.lower() else 100 for n in names])
    outline = round(big * 0.05)
    shadow_dy = round(big * 0.10)
    probe = ImageDraw.Draw(Image.new("RGBA", (1, 1)))
    l, t, r, b = probe.textbbox((0, 0), "krust", font=font, stroke_width=outline)
    pad = outline * 2
    w, h = r - l + pad * 2, b - t + pad * 2 + shadow_dy
    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    origin = (pad - l, pad - t)
    d.text((origin[0], origin[1] + shadow_dy), "krust", font=font, fill=(0, 0, 0, 46),
           stroke_width=outline, stroke_fill=(0, 0, 0, 46))
    d.text(origin, "krust", font=font, fill="#FFFFFF", stroke_width=outline, stroke_fill="#161211")
    img = img.crop(img.getchannel("A").getbbox())
    return img.resize((round(img.width * height / img.height), height), Image.LANCZOS)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--site-wordmark", help="also write the wordmark PNG here (for the website)")
    args = ap.parse_args()

    icon = render_icon()
    icon_dir = os.path.join(ROOT, "app/channels/oss/icon/no-padding")
    icon.resize((512, 512), Image.LANCZOS).save(os.path.join(icon_dir, "512x512.png"), optimize=True)
    icon.resize((256, 256), Image.LANCZOS).save(
        os.path.join(icon_dir, "icon.ico"), sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])

    render_crusty(256).save(os.path.join(ROOT, "app/assets/bundled/png/krust-crusty.png"), optimize=True)

    wordmark = render_wordmark(240)
    wordmark.save(os.path.join(ROOT, "app/assets/bundled/png/krust-logo.png"), optimize=True)
    if args.site_wordmark:
        wordmark.save(args.site_wordmark, optimize=True)
    print("icon 512 + ico, wordmark", wordmark.size)


if __name__ == "__main__":
    main()
