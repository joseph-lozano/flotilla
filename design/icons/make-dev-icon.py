# AppIcon-dev.png, Flotilla Dev's icon, from AppIcon.png: the same sails a little higher, on
# sea green, over "DEV" in pixels, so it's told apart from the release app at a glance.
#
#   uv run --with pillow python design/icons/make-dev-icon.py
from pathlib import Path
from PIL import Image

here = Path(__file__).parent
icon = Image.open(here / "AppIcon.png").convert("RGB")
W, H = icon.size
img = Image.new("RGB", (W, H), (0x0E, 0x3B, 0x36))
# Only the sails: the plum ground is a little noisy.
mask = icon.convert("L").point(lambda _: 0)
mask.putdata([max(0, min(255, (max(px) - 70) * 255 // 110)) for px in icon.get_flattened_data()])
img.paste(icon, (0, -H // 16), mask)
# "DEV" in a 3×5 pixel font under the sails.
font = {"D": ["110", "101", "101", "101", "110"], "E": ["111", "100", "110", "100", "111"], "V": ["101", "101", "101", "101", "010"]}
cell = W // 40
word = "DEV"
x0 = (W - (len(word) * 4 - 1) * cell) // 2
y0 = int(H * 0.72)
for i, ch in enumerate(word):
    for r, row in enumerate(font[ch]):
        for c, on in enumerate(row):
            if on == "1":
                x, y = x0 + (i * 4 + c) * cell, y0 + r * cell
                img.paste((0xF4, 0xEE, 0xF8), (x, y, x + cell, y + cell))
img.save(here / "AppIcon-dev.png")
