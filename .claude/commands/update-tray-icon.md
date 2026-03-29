---
description: Update tray icon from a source PNG file (pass file path as argument)
---

Update the Tauri tray icon from the source image at: $ARGUMENTS

The source image is a PNG with a dark circle containing a white meerkat silhouette. The background can be either white or black.

The tray icon config is in `src-tauri/tauri.conf.json` with `iconAsTemplate: true`. This means macOS ignores colors and uses only the alpha channel: opaque pixels are drawn in the system color, transparent pixels are invisible. Therefore the meerkat must be **cut out** (transparent) from the circle, not drawn as white.

## Prerequisites

Ensure Python3+Pillow are available. If not:

```bash
sudo pip3 install --break-system-packages Pillow
```

## Step 1: Convert to Cutout

Process all pixels in a single pass: make bright pixels (background + meerkat silhouette) transparent, and dark pixels (the circle) solid black. This works regardless of whether the source background is white or black.

```python
from PIL import Image

img = Image.open("SOURCE.PNG").convert("RGBA")
pixels = img.load()
w, h = img.size

for y in range(h):
    for x in range(w):
        r, g, b, a = pixels[x, y]
        brightness = (r + g + b) / 3.0
        if brightness > 180:
            pixels[x, y] = (0, 0, 0, 0)       # transparent (background + meerkat cutout)
        else:
            pixels[x, y] = (0, 0, 0, 255)      # solid black (circle)

img.save("/tmp/tray_cutout.png")
```

**Show the result to the user for visual confirmation.**

## Step 2: Trim, Pad, Resize, Save

The canvas size MUST be calculated from the actual trimmed content size — never use a hardcoded extent value, as it may be smaller than the content and clip the circle edges.

```python
from PIL import Image

img = Image.open("/tmp/tray_cutout.png").convert("RGBA")

# trim to content
bbox = img.getbbox()
trimmed = img.crop(bbox)
tw, th = trimmed.size

# add ~3% padding so the circle isn't clipped
padding = int(max(tw, th) * 0.03)
canvas_size = max(tw, th) + padding * 2
canvas = Image.new("RGBA", (canvas_size, canvas_size), (0, 0, 0, 0))
ox = (canvas_size - tw) // 2
oy = (canvas_size - th) // 2
canvas.paste(trimmed, (ox, oy))

# resize to 512x512 and save as RGBA
result = canvas.resize((512, 512), Image.LANCZOS)
result = result.convert("RGBA")
result.save("src-tauri/icons/icon.png")
```

The padding percentage controls tray icon size:
- 3% = large icon, minimal padding (default)
- 10% = medium icon
- 15% = small icon, more padding

## Critical: RGBA Format Required

Tauri build will fail with "icon is not RGBA" if the format is wrong. The code above uses Pillow `.convert("RGBA")` which guarantees correct format. Do NOT rely on ImageMagick's `PNG32:` prefix — it does not guarantee RGBA for black-and-white images.

## Important Rules

- This command ONLY updates `icon.png` (the tray icon). App icons are managed by `/update-icons`.
- If the tray shows as a solid dot, the meerkat was not cut out — re-check step 1.
- Stage the changed file with `git add` after generating.
