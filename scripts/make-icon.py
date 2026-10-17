#!/usr/bin/env python3
"""Generate the CXTasks app icon set from the icon master.

Usage: python3 scripts/make-icon.py

# The master is `icons/icon_source.png`

Every file this writes is a downscale of that one 1024px raster. Nothing is
drawn.

# Why this stopped drawing the icon

It used to. It reproduced the geometry with PIL — a superellipse tile, a card,
two ruled lines, a check — on the reasoning that a drawing is reproducible
where an exported raster is a binary somebody has to remember to keep.

The drawing and the shipped icon then diverged, and nobody noticed. The script
drew a flat card with a **dark** check; `icons/` has held a textured card with
a **green** check for some time. Two descriptions of one picture, and the one
that shipped was not the one in the script.

That is worse than having no script at all. This file is a documented golden
command in `CLAUDE.md` described as "Regenerate the app icon set", so running
it did not regenerate the icon — it REPLACED it with a different one, `.icns`
included, meaning the Dock icon would change on the next install. The failure
is silent and the diff is binary, so nothing would have flagged it.

So the raster is the master now, and the drawing is gone. The rationale for
drawing was that it kept the icon reproducible, but a deterministic downscale
of a committed PNG is equally reproducible and has the advantage of being the
picture that actually ships. Verified when this was written: LANCZOS downscales
of the master reproduce every file then on disk to a mean channel difference of
0.1–0.4, the remainder being resampler disagreement on the tile's antialiased
edge.

# Where the picture came from, since it is now a binary

Design direction came from gpt-image-2 candidates. The geometry is measured
from `cxmail/src-tauri/icons/128x128@2x.png` so the two icons sit together in
the Dock as a family:

    tile      206 x 206 on a 256 canvas (80.5%), symmetric 25px margins
    corner    superellipse, n ≈ 5.5 — Apple's squircle. A plain rounded
              rectangle is visibly wrong: cxmail's corner insets measure
              50/33/23/14/7/3/1 at dy=0/2/5/10/20/30/40 from the top, where a
              circular arc of the same radius would give 50/…/20/10/… — the
              real corner is squarer for longer.
    object    154 x 94 (74.8% x 45.6% of the tile), aspect 1.64, dead-centred,
              covering ~34% of the tile area
    tile      vertical gradient, roughly #34343A → #25252A
    card      near-white with a soft drop shadow; check in green

cxmail's object is an envelope; ours is a task card.

# The iOS icon is NOT generated here

`~/Projects/xcode/cxtasks/scripts/make-icon.py` derives it from this same
master, because iOS needs the tile extended to full bleed with no alpha rather
than a tile floating on a transparent canvas. It is a different transform of
the same picture, not a copy of these files.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image

ICONS = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"
MASTER = ICONS / "icon_source.png"

# Everything `tauri.conf.json` bundles, plus the Windows/Store sizes the Tauri
# scaffold expects. `icon.png` is not bundled but is tracked and is the
# conventional Tauri master name, so it is kept in step rather than left to rot
# at whatever size it happened to be.
PNGS = [
    ("icon.png", 512),
    ("32x32.png", 32),
    ("128x128.png", 128),
    ("128x128@2x.png", 256),
    ("Square30x30Logo.png", 30),
    ("Square44x44Logo.png", 44),
    ("Square71x71Logo.png", 71),
    ("Square89x89Logo.png", 89),
    ("Square107x107Logo.png", 107),
    ("Square142x142Logo.png", 142),
    ("Square150x150Logo.png", 150),
    ("Square284x284Logo.png", 284),
    ("Square310x310Logo.png", 310),
    ("StoreLogo.png", 50),
]

ICNS_BASES = (16, 32, 128, 256, 512)  # each also emitted @2x — 512@2x is 1024
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)


def scaled(master: Image.Image, size: int) -> Image.Image:
    """One size, LANCZOS.

    LANCZOS specifically, not the default: at 32px the card's two ruled lines
    are about a pixel tall each, and a softer filter turns them into one grey
    smear. It is also the filter the files on disk were produced with, so this
    reproduces them rather than quietly restyling every icon on first run.
    """
    return master.resize((size, size), Image.LANCZOS)


def main() -> None:
    if not MASTER.exists():
        sys.exit(f"icon master not found: {MASTER}")

    master = Image.open(MASTER).convert("RGBA")
    if master.size != (1024, 1024):
        sys.exit(f"{MASTER.name} is {master.size}, expected 1024x1024 — it is the master, not an output")

    for name, size in PNGS:
        scaled(master, size).save(ICONS / name)
        print(f"  {name}")

    if shutil.which("iconutil") is None:
        print("WARNING: iconutil not found — icon.icns NOT regenerated (macOS only)")
    else:
        with tempfile.TemporaryDirectory() as tmp:
            iconset = Path(tmp) / "icon.iconset"
            iconset.mkdir()
            for base in ICNS_BASES:
                scaled(master, base).save(iconset / f"icon_{base}x{base}.png")
                scaled(master, base * 2).save(iconset / f"icon_{base}x{base}@2x.png")
            subprocess.run(
                ["iconutil", "-c", "icns", str(iconset), "-o", str(ICONS / "icon.icns")],
                check=True,
            )
        print("  icon.icns")

    scaled(master, 256).save(ICONS / "icon.ico", sizes=[(s, s) for s in ICO_SIZES])
    print("  icon.ico")


if __name__ == "__main__":
    main()
