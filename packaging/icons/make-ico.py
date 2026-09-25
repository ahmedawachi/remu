#!/usr/bin/env python3
"""Cuts packaging/windows/remu.ico from the 1024 icon master.

An ICO directory entry may hold a PNG rather than a BMP, which every Windows
version since Vista reads. That is the whole reason this is fifty lines of
standard library instead of a dependency on an image toolchain: the sizes are
produced by whatever is at hand (sips on macOS, a Pillow fallback elsewhere)
and then concatenated with a header.
"""

import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

# 256 is the largest size an ICO directory can name; it is encoded as 0.
SIZES = (16, 24, 32, 48, 64, 128, 256)

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "packaging" / "icons" / "icon-1024.png"
TARGET = ROOT / "packaging" / "windows" / "remu.ico"


def resize(source: Path, size: int, out: Path) -> None:
    """Writes `source` scaled to `size`x`size` as a PNG at `out`."""
    if shutil.which("sips"):
        subprocess.run(
            ["sips", "-z", str(size), str(size), str(source), "--out", str(out)],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        return
    try:
        from PIL import Image
    except ImportError:
        sys.exit(
            "need either sips (macOS) or Pillow (pip install pillow) to resize"
        )
    with Image.open(source) as image:
        image.convert("RGBA").resize((size, size), Image.LANCZOS).save(out)


def main() -> None:
    if not SOURCE.exists():
        sys.exit(
            f"{SOURCE} is missing; regenerate it with\n"
            "  cargo test -p remu-desk --test media app_icon -- --ignored"
        )

    with tempfile.TemporaryDirectory() as tmp:
        images = []
        for size in SIZES:
            png = Path(tmp) / f"{size}.png"
            resize(SOURCE, size, png)
            images.append((size, png.read_bytes()))

        # ICONDIR, then one ICONDIRENTRY per image, then the image data.
        header = struct.pack("<HHH", 0, 1, len(images))
        offset = len(header) + 16 * len(images)
        entries = bytearray()
        for size, data in images:
            entries += struct.pack(
                "<BBBBHHII",
                size if size < 256 else 0,
                size if size < 256 else 0,
                0,  # no colour palette
                0,  # reserved
                1,  # colour planes
                32,  # bits per pixel
                len(data),
                offset,
            )
            offset += len(data)

        TARGET.parent.mkdir(parents=True, exist_ok=True)
        with TARGET.open("wb") as ico:
            ico.write(header)
            ico.write(entries)
            for _, data in images:
                ico.write(data)

    print(f"wrote {TARGET} ({TARGET.stat().st_size} bytes, {len(SIZES)} sizes)")


if __name__ == "__main__":
    main()
