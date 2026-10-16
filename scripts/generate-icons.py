#!/usr/bin/env python3
"""Render the four-tile SVG into the desktop icon formats, using only Python."""

import argparse
import math
from pathlib import Path
import struct
import xml.etree.ElementTree as ET
import zlib


ROOT = Path(__file__).resolve().parent.parent


def png_chunk(kind, content):
    return struct.pack(">I", len(content)) + kind + content + struct.pack(">I", zlib.crc32(kind + content))


def render_png(svg, size):
    scale = size / float(svg.attrib["viewBox"].split()[2])
    pixels = bytearray(size * size * 4)
    for tile in svg:
        assert tile.tag == "{http://www.w3.org/2000/svg}rect"
        left, top, width, height, radius = (
            float(tile.attrib[key]) * scale for key in ("x", "y", "width", "height", "rx")
        )
        assert width == height and 0 < radius <= width / 2
        color = bytes.fromhex(tile.attrib["fill"].removeprefix("#"))
        assert len(color) == 3

        def inside(horizontal, vertical):
            distance_x = max(abs(horizontal - left - width / 2) - width / 2 + radius, 0)
            distance_y = max(abs(vertical - top - height / 2) - height / 2 + radius, 0)
            return math.hypot(distance_x, distance_y) - radius

        for vertical in range(math.floor(top), math.ceil(top + height)):
            for horizontal in range(math.floor(left), math.ceil(left + width)):
                distance = inside(horizontal + 0.5, vertical + 0.5)
                if distance > 0.75:
                    continue
                coverage = 16 if distance < -0.75 else sum(
                    inside(horizontal + (sample_x + 0.5) / 4, vertical + (sample_y + 0.5) / 4) <= 0
                    for sample_y in range(4) for sample_x in range(4)
                )
                offset = (vertical * size + horizontal) * 4
                pixels[offset:offset + 4] = color + bytes([round(coverage * 255 / 16)])
    if size == 256:
        for horizontal, vertical in [(64, 64), (192, 64), (64, 192), (192, 192)]:
            offset = (vertical * size + horizontal) * 4
            assert pixels[offset:offset + 4] == bytes([119, 131, 255, 255])
        assert pixels[(128 * size + 128) * 4 + 3] == 0
    rows = b"".join(b"\0" + pixels[row * size * 4:(row + 1) * size * 4] for row in range(size))
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + png_chunk(b"IDAT", zlib.compress(rows, 9))
        + png_chunk(b"IEND", b"")
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if checked-in icons differ from the SVG")
    arguments = parser.parse_args()
    svg = ET.parse(ROOT / "assets/unidirstat.svg").getroot()
    assert svg.attrib["viewBox"] == "0 0 24 24" and len(svg) == 4
    sizes = [16, 24, 32, 48, 64, 128, 256, 512, 1024]
    images = {size: render_png(svg, size) for size in sizes}
    ico_sizes = sizes[:7]
    offset = 6 + 16 * len(ico_sizes)
    directory = bytearray(struct.pack("<HHH", 0, 1, len(ico_sizes)))
    for size in ico_sizes:
        directory.extend(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(images[size]), offset))
        offset += len(images[size])
    ico = bytes(directory) + b"".join(images[size] for size in ico_sizes)
    icns = b"".join(
        kind + struct.pack(">I", len(images[size]) + 8) + images[size]
        for size, kind in [(16, b"icp4"), (32, b"icp5"), (64, b"icp6"), (128, b"ic07"),
                           (256, b"ic08"), (512, b"ic09"), (1024, b"ic10")]
    )
    outputs = {
        "unidirstat.png": images[256],
        "unidirstat.ico": ico,
        "unidirstat.icns": b"icns" + struct.pack(">I", len(icns) + 8) + icns,
    }
    for filename, content in outputs.items():
        path = ROOT / "assets" / filename
        if arguments.check:
            if not path.exists() or path.read_bytes() != content:
                raise SystemExit(f"{path} is outdated; run scripts/generate-icons.py")
        else:
            path.write_bytes(content)
    print("Desktop icons match the SVG." if arguments.check else "Desktop icons generated.")


if __name__ == "__main__":
    main()
