#!/usr/bin/env python3
"""Regenerate the boot splash assets in `kernel/src/`.

Two artefacts are produced, both as 1-bit-per-pixel bitmaps in `.rodata`:

* `logo.rs`    -- the project mark, split into two planes so it keeps its
                  two-tone reading on a console with no palette.
* `banner.rs`  -- the letterspaced tagline, set on the build host so the
                  splash does not fall back to the 8x8 console font.

Rasterising here rather than in the kernel is the point: the kernel gains no
font parser, no layout engine and no heap allocation, and the generated tables
are constant. Output is committed, so contributors do not need Python or a
font installed to build.

Usage:
    python3 tools/gen_splash.py [--logo PATH] [--font PATH] [--out DIR]
"""

import argparse
import sys
from pathlib import Path

try:
    import numpy as np
    from PIL import Image, ImageDraw, ImageFont
except ImportError:  # pragma: no cover - developer tooling only
    sys.exit("this script needs Pillow and numpy: pip install pillow numpy")

LOGO_PX = 192
TAG_TEXT = "EXPERIMENTAL OPERATING SYSTEM"
TAG_SIZE = 17
TAG_TRACKING = 7
TAG_HEIGHT = 26

# Searched in order when --font is not given. Kept short and cross-platform so
# a contributor on Windows or macOS can regenerate without editing this file.
FONT_CANDIDATES = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "C:/Windows/Fonts/arial.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
]

HEADER = """//! {what} -- GENERATED, do not edit by hand.
//!
//! Produced by `tools/gen_splash.py`. Rasterising on the build host keeps the
//! kernel free of a font parser, a layout engine and any heap allocation --
//! it only blits. Bits are LSB-first within each byte, matching the
//! convention `font8x8::pixel_is_set` already uses.
//!
//! Source: {source}

use crate::framebuffer_console::Bitmap;
"""


def find_font(explicit):
    if explicit:
        if not Path(explicit).is_file():
            sys.exit(f"font not found: {explicit}")
        return explicit
    for candidate in FONT_CANDIDATES:
        if Path(candidate).is_file():
            return candidate
    sys.exit(
        "no usable font found. Pass --font /path/to/font.ttf\n"
        "Searched:\n  " + "\n  ".join(FONT_CANDIDATES)
    )


def pack(mask):
    """1bpp, row-major, LSB-first within each byte."""
    height, width = mask.shape
    stride = (width + 7) // 8
    out = bytearray()
    for y in range(height):
        row = bytearray(stride)
        for x in range(width):
            if mask[y, x]:
                row[x >> 3] |= 1 << (x & 7)
        out += row
    return bytes(out), width, height


def emit(name, data, width, height):
    lines = [f"static {name}_ROWS: [u8; {len(data)}] = ["]
    for i in range(0, len(data), 24):
        lines.append("    " + ", ".join(f"0x{b:02X}" for b in data[i : i + 24]) + ",")
    lines.append("];")
    lines.append(
        f"pub static {name}: Bitmap = "
        f"Bitmap {{ width: {width}, height: {height}, rows: &{name}_ROWS }};"
    )
    return "\n".join(lines)


def build_logo(path, out_dir):
    """Split the mark into `STRUCTURE` (white) and `MONOGRAM` (accent).

    The two planes are separated by hue: the monogram is drawn in a khaki
    whose blue channel is clearly depressed relative to red and green, which
    distinguishes it from the neutral-white structure without needing a
    hand-authored mask.
    """
    rgb = np.array(Image.open(path).convert("RGB")).astype(int)
    red, green, blue = rgb[..., 0], rgb[..., 1], rgb[..., 2]
    luma = (299 * red + 587 * green + 114 * blue) // 1000

    lit = luma > 26
    monogram = lit & (((red + green) // 2 - blue) > 18)
    structure = lit & ~monogram

    ys, xs = np.where(lit)
    box = (xs.min(), ys.min(), xs.max() + 1, ys.max() + 1)
    width, height = box[2] - box[0], box[3] - box[1]
    side = max(width, height)

    def scale(mask):
        cropped = Image.fromarray((mask * 255).astype("uint8")).crop(box)
        square = Image.new("L", (side, side), 0)
        square.paste(cropped, ((side - width) // 2, (side - height) // 2))
        resized = square.resize((LOGO_PX, LOGO_PX), Image.LANCZOS)
        return np.array(resized.point(lambda p: 255 if p > 76 else 0)) > 0

    def despeckle(mask):
        # Drop pixels with fewer than two lit neighbours: source compression
        # artefacts survive thresholding as isolated dots, real strokes do not.
        padded = np.pad(mask, 1)
        neighbours = (
            padded[:-2, :-2].astype(int)
            + padded[:-2, 1:-1]
            + padded[:-2, 2:]
            + padded[1:-1, :-2]
            + padded[1:-1, 2:]
            + padded[2:, :-2]
            + padded[2:, 1:-1]
            + padded[2:, 2:]
        )
        return mask & (neighbours >= 2)

    mono, struct = despeckle(scale(monogram)), despeckle(scale(structure))
    struct = struct & ~mono  # planes must not overlap

    body = [HEADER.format(what="TuwaiqOS boot logo", source=Path(path).name), ""]
    for name, mask in (("STRUCTURE", struct), ("MONOGRAM", mono)):
        data, w, h = pack(mask)
        body.append(emit(name, data, w, h))
        body.append("")
    body.append("/// Logo edge length in pixels.")
    body.append(f"pub const SIZE: usize = {LOGO_PX};")

    (out_dir / "logo.rs").write_text("\n".join(body) + "\n", encoding="utf-8")
    return sum(len(pack(m)[0]) for m in (struct, mono))


def build_tagline(font_path, out_dir):
    """Set the tagline as letterspaced caps.

    Tracking is applied by placing each glyph individually rather than by
    relying on the font, so the result is identical regardless of which face
    is available on the build machine.
    """
    font = ImageFont.truetype(font_path, TAG_SIZE)
    advances = [
        font.getbbox(c)[2] - font.getbbox(c)[0] if c != " " else TAG_SIZE // 3
        for c in TAG_TEXT
    ]
    total = sum(advances) + TAG_TRACKING * (len(TAG_TEXT) - 1)

    canvas = Image.new("L", (total + 8, TAG_HEIGHT), 0)
    draw = ImageDraw.Draw(canvas)
    x = 4
    for char, advance in zip(TAG_TEXT, advances):
        if char != " ":
            bbox = font.getbbox(char)
            draw.text(
                (x - bbox[0], (TAG_HEIGHT - (bbox[3] - bbox[1])) // 2 - bbox[1]),
                char,
                fill=255,
                font=font,
            )
        x += advance + TAG_TRACKING

    trimmed = canvas.crop(canvas.getbbox())
    mask = np.array(trimmed.point(lambda p: 255 if p > 96 else 0)) > 0
    data, w, h = pack(mask)

    body = [
        HEADER.format(what="Boot splash typography", source=Path(font_path).name),
        "",
        f"/// {TAG_TEXT!r}, letterspaced.",
        emit("TAGLINE", data, w, h),
    ]
    (out_dir / "banner.rs").write_text("\n".join(body) + "\n", encoding="utf-8")
    return len(data)


def main():
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--logo", default=str(root / "docs" / "logo.png"))
    parser.add_argument("--font", default=None)
    parser.add_argument("--out", default=str(root / "kernel" / "src"))
    args = parser.parse_args()

    out_dir = Path(args.out)
    if not Path(args.logo).is_file():
        sys.exit(f"logo not found: {args.logo}  (pass --logo)")

    logo_bytes = build_logo(args.logo, out_dir)
    tag_bytes = build_tagline(find_font(args.font), out_dir)

    print(f"logo.rs   : {logo_bytes} bytes ({logo_bytes / 1024:.1f} KiB)")
    print(f"banner.rs : {tag_bytes} bytes")
    print(f"total     : {(logo_bytes + tag_bytes) / 1024:.1f} KiB of .rodata")


if __name__ == "__main__":
    main()
