# /// script
# requires-python = ">=3.10"
# dependencies = ["resvg-py==0.5.0"]
# ///
"""Regenerate the Windows icon from the approved SVG with uv run assets/generate-icon.py."""

from pathlib import Path
from struct import pack

import resvg_py


assets = Path(__file__).resolve().parent
source = assets / "icon-concepts" / "01-layout.svg"
# The 20-DIP menu logo occupies 20–100 physical pixels at 100–500% scale.
# Keep the smaller tray sizes and larger Explorer sizes in the same resource.
sizes = [*range(16, 101), 128, 256]
directory = bytearray(pack("<HHH", 0, 1, len(sizes)))
images = bytearray()
first_image = 6 + 16 * len(sizes)
for size in sizes:
    png = resvg_py.svg_to_bytes(
        svg_path=str(source), width=size, height=size, skip_system_fonts=True
    )
    dimension = size % 256  # ICO encodes a 256-pixel dimension as zero.
    directory.extend(
        pack("<BBBBHHII", dimension, dimension, 0, 0, 1, 32, len(png), first_image + len(images))
    )
    images.extend(png)

(assets / "rustile.ico").write_bytes(directory + images)
