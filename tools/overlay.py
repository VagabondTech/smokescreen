"""Where the volumetric picture and the rule disagree, painted over the lit view.

Run the demo's agreement script for volumetric smoke (see `agreement.py`), for
each build or setting to compare, then:

    python3 tools/overlay.py out.png before=Before after=After [--view close]

Each `<dir>=<title>` becomes a panel, side by side, of one view (default
`close`): grey where the rule blocks and the picture lets at most 10 % of the
scene through, orange where 10–30 % shows through, red where more than 30 %
does, and blue where the picture is opaque but the rule doesn't block (fair:
more smoke). Pixels along edges are left alone, as in `agreement.py`.

Needs numpy and Pillow.
"""
import os
import sys

import numpy as np
from PIL import Image, ImageDraw

# Next to agreement.py, also under `python -I`.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from agreement import boundary, depth_edges, near, transmittance  # noqa: E402


def panel(src, title, view):
    w, h = map(int, open(f"{src}/{view}_size.txt").read().split())
    lit = np.asarray(Image.open(f"{src}/{view}_lit.png").convert("RGB")).astype(np.float64)
    t = transmittance(f"{src}/{view}_gpu_transmittance.png")
    rule = np.fromfile(f"{src}/{view}_rule_blocked.u8", np.uint8).reshape(h, w) > 0
    depth = np.fromfile(f"{src}/{view}_depth.f32", "<f4").reshape(h, w)
    excused = near(boundary(rule) | boundary(t < 0.01) | depth_edges(depth))
    img = lit * 0.45 + 255 * 0.55 * 0.35
    for mask, colour in [
        (rule & (t > 0.3) & ~excused, (230, 30, 30)),
        (rule & (t > 0.1) & (t <= 0.3) & ~excused, (255, 150, 0)),
        (rule & (t <= 0.1), (90, 90, 90)),
        ((t < 0.01) & ~rule & ~excused, (40, 110, 255)),
    ]:
        img[mask] = colour
    tile = Image.fromarray(img.clip(0, 255).astype(np.uint8))
    d = ImageDraw.Draw(tile)
    d.rectangle([0, 0, w, 22], fill=(255, 255, 255))
    d.text((8, 5), title, fill=(0, 0, 0))
    return tile


def main(args):
    view = "close"
    if "--view" in args:
        i = args.index("--view")
        view = args[i + 1]
        del args[i : i + 2]
    out, specs = args[0], args[1:]
    tiles = [panel(*spec.split("=", 1), view) for spec in specs]
    w, h = tiles[0].size
    sheet = Image.new("RGB", (w * len(tiles), h), (255, 255, 255))
    for k, tile in enumerate(tiles):
        sheet.paste(tile, (k * w, 0))
    sheet.save(out)


if __name__ == "__main__":
    main(sys.argv[1:])
