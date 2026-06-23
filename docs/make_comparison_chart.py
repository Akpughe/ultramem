#!/usr/bin/env python3
"""Render the LongMemEval-S comparison chart (UltraMem vs the reference set).

Pure-Pillow (no matplotlib) so it runs anywhere. Outputs docs/longmemeval-comparison.png.

IMPORTANT honesty note baked into the subtitle: UltraMem's bars are a 120-question
slice (20/category) judged by Gemini 2.5 Flash; the other four are the full-set,
GPT-4o-judged leaderboard figures. Same benchmark, NOT an apples-to-apples judge/N.
"""
from PIL import Image, ImageDraw, ImageFont

W, H = 2200, 1080
BG = (38, 34, 31)            # dark warm near-black, matches the reference
INK = (243, 239, 230)        # cream text
MUTE = (154, 145, 134)       # muted label text
BASELINE = (90, 83, 75)

CATS = [
    "Single-Session\nUser (overall)", "Single-Session\nAssistant",
    "Single-Session\nPreference", "Knowledge\nUpdate",
    "Temporal\nReasoning", "Multi-Session",
]
# (name, color, values per category). UltraMem first = accent spotlight.
SERIES = [
    ("UltraMem",     (111, 168, 158), [90.0, 70.0, 45.0, 80.0, 85.0, 65.0]),   # sage-teal accent
    ("Shram",        (243, 238, 226), [100.0, 100.0, 90.0, 93.6, 71.4, 72.9]), # cream
    ("Supermemory",  (169, 155, 135), [97.1, 96.4, 70.0, 88.4, 76.7, 71.4]),   # tan
    ("Zep",          (137, 124, 108), [92.9, 80.4, 56.7, 83.3, 62.4, 57.9]),   # taupe
    ("Full context", (95, 83, 70),    [81.4, 94.6, 20.0, 78.2, 45.1, 44.3]),   # dark brown
]

def font(sz, bold=False):
    paths = ([
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
    ] if bold else [
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
    ])
    for p in paths:
        try:
            return ImageFont.truetype(p, sz)
        except Exception:
            continue
    return ImageFont.load_default()

f_title = font(46, True)
f_sub   = font(20)
f_cat   = font(27, True)
f_val   = font(16)
f_leg   = font(25)

img = Image.new("RGB", (W, H), BG)
d = ImageDraw.Draw(img)

def ctext(x, y, s, fnt, fill, anchor="la"):
    d.text((x, y), s, font=fnt, fill=fill, anchor=anchor)

# Title + honesty subtitle
ctext(70, 46, "LongMemEval-S Benchmark", f_title, INK)
ctext(72, 104,
      "UltraMem = 120-question slice (20/category), Gemini 2.5 Flash judge  ·  "
      "others = full-set, GPT-4o-judged leaderboard figures (same benchmark, not an apples-to-apples judge/N)",
      f_sub, MUTE)

# Legend (top-right, single row)
lx = W - 70
items = list(reversed(SERIES))  # draw right-to-left
sq = 26
gap_after_sq = 12
gap_between = 34
for name, color, _ in items:
    tw = d.textlength(name, font=f_leg)
    block = sq + gap_after_sq + tw
    lx -= block
    d.rounded_rectangle([lx, 52, lx + sq, 52 + sq], radius=6, fill=color)
    ctext(lx + sq + gap_after_sq, 52 + sq / 2, name, f_leg, INK, anchor="lm")
    lx -= gap_between

# Plot geometry
L, R, TOP = 100, 80, 210
base_y = H - 188
full_h = base_y - (TOP + 30)          # pixels for 100%
plot_w = W - L - R
gw = plot_w / len(CATS)               # group slot width
bar_w, bar_gap = 46, 8
n = len(SERIES)
group_bars_w = n * bar_w + (n - 1) * bar_gap
pad = (gw - group_bars_w) / 2

def lum(c):
    return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]

for gi, cat in enumerate(CATS):
    gx = L + gi * gw
    cx = gx + gw / 2
    for si, (name, color, vals) in enumerate(SERIES):
        v = vals[gi]
        bx = gx + pad + si * (bar_w + bar_gap)
        bh = max(6, full_h * v / 100.0)
        top_y = base_y - bh
        d.rounded_rectangle([bx, top_y, bx + bar_w, base_y], radius=10, fill=color)
        # square off the very bottom so bars sit flat on the baseline
        d.rectangle([bx, base_y - 10, bx + bar_w, base_y], fill=color)
        # value label near the bottom of the bar
        lab = f"{v:.0f}%" if float(v).is_integer() else f"{v:.1f}%"
        tcol = (58, 53, 48) if lum(color) > 150 else (222, 216, 206)
        d.text((bx + bar_w / 2, base_y - 14), lab, font=f_val, fill=tcol, anchor="mb")
    # category label(s) below baseline
    for li, line in enumerate(cat.split("\n")):
        d.text((cx, base_y + 34 + li * 34), line, font=f_cat, fill=(232, 227, 218), anchor="ma")

# baseline
d.line([L - 10, base_y, W - R + 10, base_y], fill=BASELINE, width=2)

out = "docs/longmemeval-comparison.png"
img.save(out)
print("wrote", out, img.size)
