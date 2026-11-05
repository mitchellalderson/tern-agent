"""Renders the JSON cell dump from the tern-tui demo_screenshot test to a PNG (needs Pillow, macOS Menlo).
   TERN_TUI_SHOT=out.json cargo test -p tern-tui demo_screenshot -- --ignored
   python3 scripts/render_tui_screenshot.py out.json docs/assets/tui.png"""
import json, sys
from PIL import Image, ImageDraw, ImageFont
src, out = sys.argv[1], sys.argv[2]
d = json.load(open(src))
BG = (0x1F, 0x18, 0x12); FG = (0xF3, 0xE8, 0xD2)
COL = {"Reset": None, "DarkGray": (0x9A, 0x8B, 0x74), "Green": (0x8D, 0xB5, 0x6F), "Cyan": (0x4D, 0xB3, 0xAE),
       "White": FG, "Yellow": (0xF9, 0xC8, 0x46), "Red": (0xF0, 0x8A, 0x7A), "Gray": (0xB3, 0xA5, 0x8F)}
BGC = {"Reset": BG, "DarkGray": (0x3D, 0x32, 0x26)}
SCALE = 2; SIZE = 15 * SCALE
reg = ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", SIZE, index=0)
bold = ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", SIZE, index=1)
cw = round(reg.getlength("M")); ch = round(SIZE * 1.32)
pad = 22 * SCALE; bar = 36 * SCALE
W = d["w"] * cw + pad * 2; H = d["h"] * ch + pad * 2 + bar
img = Image.new("RGB", (W, H), BG); dr = ImageDraw.Draw(img)
# title bar with the brand stripe
dr.rectangle([0, 0, W, bar], fill=(0x26, 0x1E, 0x17))
for i, c in enumerate([(0xF5, 0x8A, 0x3D), (0xF9, 0xC8, 0x46), (0x4D, 0xB3, 0xAE)]):
    dr.rectangle([0, i * 3 * SCALE, W, (i + 1) * 3 * SCALE], fill=c)
dr.text((pad, bar // 2 + 3 * SCALE), "tern", font=bold, fill=FG, anchor="lm")
for y, row in enumerate(d["rows"]):
    for x, (sym, fg, bg, mod) in enumerate(row):
        px, py = pad + x * cw, bar + pad + y * ch
        b = BGC.get(bg, BG)
        if b != BG: dr.rectangle([px, py, px + cw, py + ch], fill=b)
        if sym.strip():
            f = COL.get(fg) or FG
            if bg == "DarkGray" and fg == "White": f = FG
            dr.text((px, py), sym, font=bold if mod & 1 else reg, fill=f)
img.save(out)
print(img.size)
