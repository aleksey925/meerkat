---
description: Update app icons from a source PNG file (pass file path as argument)
---

Update all Tauri app icons from the source image at: $ARGUMENTS

The source image is expected to be a 1024x1024 PNG, typically RGB without alpha, with a white/light background and macOS-style shadow/reflection around a rounded-rectangle icon.

## Prerequisites

Ensure ImageMagick and Python3+Pillow are available. If not:

```bash
sudo apt-get update -qq && sudo apt-get install -y -qq imagemagick
sudo pip3 install --break-system-packages Pillow
```

## Step 1: Detect Icon Boundaries

Use brightness threshold scanning to find the edges of the rounded rectangle icon. Do NOT use gradient analysis — it breaks on icons with high-contrast content (e.g. silhouettes create false boundaries).

Scan at multiple positions across each edge and take the mode (most common value) to filter out noise from icon content.

```python
from PIL import Image
from collections import Counter

img = Image.open("SOURCE.PNG").convert("L")
w, h = img.size
pixels = img.load()

tops, bottoms, lefts, rights = [], [], [], []

for x in range(w // 4, 3 * w // 4, 10):
    for y in range(h):
        if pixels[x, y] < 100: tops.append(y); break
    for y in range(h - 1, 0, -1):
        if pixels[x, y] < 100: bottoms.append(y); break

for y in range(h // 4, 3 * h // 4, 10):
    for x2 in range(w):
        if pixels[x2, y] < 100: lefts.append(x2); break
    for x2 in range(w - 1, 0, -1):
        if pixels[x2, y] < 100: rights.append(x2); break

top = Counter(tops).most_common(1)[0][0]
bottom = Counter(bottoms).most_common(1)[0][0]
left = Counter(lefts).most_common(1)[0][0]
right = Counter(rights).most_common(1)[0][0]
```

Typical values for 1024x1024: Top~152-191, Bottom~814-820, Left~168-193, Right~830-856.

## Step 2: Apply Squircle Mask

The macOS icon shape is a superellipse (squircle) with exponent n=5. Create a mask and apply it to the source image.

Calculate parameters from detected boundaries:
- Center: `cx = (left + right) / 2`, `cy = (top + bottom) / 2`
- Half-dimensions with ~14-16px margin inward to avoid white corners: `hw = (right - left) / 2 - 14`, `hh = (bottom - top) / 2 - 14`

```python
from PIL import Image

orig = Image.open("SOURCE.PNG").convert("RGBA")
w, h = orig.size

n = 5  # superellipse exponent
mask = Image.new("L", (w, h), 0)
mp = mask.load()

for y in range(h):
    for x in range(w):
        nx = abs(x - cx) / hw
        ny = abs(y - cy) / hh
        val = nx**n + ny**n
        if val <= 1.0:
            mp[x, y] = 255
        elif val <= 1.03:
            # anti-aliased edge
            mp[x, y] = int(255 * (1 - (val - 1.0) / 0.03))

orig.putalpha(mask)
orig.save("/tmp/icon_masked.png")
```

## Step 3: Verify No White Remnants

Check that no near-white pixels (brightness > 240) remain visible outside the main icon content area. If they do, reduce hw/hh by a few more pixels and re-run step 2.

## Step 4: Trim and Add macOS Padding

```bash
magick /tmp/icon_masked.png -trim +repage -gravity center -background none -extent 690x690 +repage /tmp/icon_final.png
```

The extent value controls icon size relative to other macOS apps:
- 690x690 = standard size (default)
- 770x770 = smaller icon, more padding
- 824x824 = even smaller icon, even more padding

**Show the result to the user for visual confirmation before proceeding.**

## Step 5: Generate All Icon Sizes

Output directory: `src-tauri/icons/`

```bash
ICONS=src-tauri/icons
APP=/tmp/icon_final.png

magick "$APP" -resize 32x32 "$ICONS/32x32.png"
magick "$APP" -resize 128x128 "$ICONS/128x128.png"
magick "$APP" -resize 256x256 "$ICONS/128x128@2x.png"
magick "$APP" -resize 30x30 "$ICONS/Square30x30Logo.png"
magick "$APP" -resize 44x44 "$ICONS/Square44x44Logo.png"
magick "$APP" -resize 71x71 "$ICONS/Square71x71Logo.png"
magick "$APP" -resize 89x89 "$ICONS/Square89x89Logo.png"
magick "$APP" -resize 107x107 "$ICONS/Square107x107Logo.png"
magick "$APP" -resize 142x142 "$ICONS/Square142x142Logo.png"
magick "$APP" -resize 150x150 "$ICONS/Square150x150Logo.png"
magick "$APP" -resize 284x284 "$ICONS/Square284x284Logo.png"
magick "$APP" -resize 310x310 "$ICONS/Square310x310Logo.png"
magick "$APP" -resize 50x50 "$ICONS/StoreLogo.png"
magick "$APP" -define icon:auto-resize=256,128,64,48,32,16 "$ICONS/icon.ico"
magick "$APP" -resize 1024x1024 "$ICONS/icon.icns"
```

## Step 6: Enforce RGBA Format

ImageMagick does NOT guarantee RGBA output for black-and-white images — it may save as Bilevel/Grayscale even with `PNG32:` prefix. Always re-save all generated PNGs through Pillow as a final step:

```python
import os
from PIL import Image

icons_dir = "src-tauri/icons"
for f in os.listdir(icons_dir):
    if f.endswith(".png") and f != "icon.png":
        path = os.path.join(icons_dir, f)
        img = Image.open(path).convert("RGBA")
        img.save(path)
```

Tauri build will fail with "icon is not RGBA" if the format is wrong.

## Important Rules

- Do NOT overwrite `icon.png` — that is the tray icon (separate image, managed by `/update-tray-icon`)
- All generated PNGs must be RGBA format (enforced by Step 6)
- Never use color-based background removal (floodfill, -transparent) — it bleeds into icon content. Always use the squircle mask approach.
- Stage changed files with `git add` after generating
