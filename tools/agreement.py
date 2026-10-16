"""Picture-versus-query agreement for faceted smoke.

    SMOKE_DEMO_STYLE=faceted SMOKE_DEMO_SCRIPT=agreement SMOKE_DEMO_CAPTURE=agree \\
        cargo run --release --example demo
    python3 tools/agreement.py agree [--chord 0.8]

For every pixel of each view, compares the GPU's silhouette mask (faceted
smoke drawn flat magenta) with the CPU gameplay query for the segment from the
eye to the scene behind that pixel:

- hidden, drawn clear: the query blocks it but the picture shows through;
- drawn, not hidden: the picture is opaque but the query says visible.

Mismatches within one pixel of the silhouette or of a depth edge are counted
separately: a pixel there covers both sides, so either answer is defensible.

Also compares the picture with the core's rule (`{view}_rule_blocked.u8`,
`smokescreen`'s `blocks_sight` at the configured threshold) when the run saved
it. With --chord, compares it with a thickness rule computed here instead: a
segment is blocked once it passes through that many metres of cloud.

Needs numpy and Pillow.
"""
import sys

import numpy as np
from PIL import Image


def magenta(path):
    a = np.asarray(Image.open(path).convert("RGB")).astype(np.int32)
    return ((a[..., 0] - a[..., 1]) > 60) & ((a[..., 2] - a[..., 1]) > 60)


def near(mask, n=1):
    """Pixels within n pixels (8-neighbourhood) of a True pixel."""
    out = mask.copy()
    for _ in range(n):
        p = np.pad(out, 1)
        acc = out.copy()
        for dy in (-1, 0, 1):
            for dx in (-1, 0, 1):
                acc |= p[1 + dy : p.shape[0] - 1 + dy, 1 + dx : p.shape[1] - 1 + dx]
        out = acc
    return out


def boundary(mask):
    """Pixels whose 8-neighbourhood holds both values."""
    return near(mask) & near(~mask)


def depth_edges(depth, rel=0.03):
    p = np.pad(depth, 1, mode="edge")
    edge = np.zeros(depth.shape, bool)
    for dy, dx in ((0, 1), (1, 0), (0, -1), (-1, 0)):
        nb = p[1 + dy : p.shape[0] - 1 + dy, 1 + dx : p.shape[1] - 1 + dx]
        edge |= np.abs(nb - depth) > rel * np.minimum(nb, depth)
    return edge


def main(src, chord_rule=None):
    print(f"{'view':6} {'drawn':>8} {'hidden':>8} | {'hidden,clear':>12} {'(edge)':>7} | {'drawn,visible':>13} {'(edge)':>7}", end="")
    print(f" | rule: {'blocked,clear':>13} {'(edge)':>7} {'drawn,open':>10} {'(edge)':>7}", end="")
    if chord_rule:
        print(f" | thickness rule {chord_rule} m: {'drawn,thin':>10} {'(edge)':>7}", end="")
    print()
    for view in ["far", "front", "close", "edge"]:
        try:
            w, h = map(int, open(f"{src}/{view}_size.txt").read().split())
        except FileNotFoundError:
            continue
        gpu = magenta(f"{src}/{view}_gpu_mask.png")
        cpu = np.fromfile(f"{src}/{view}_cpu_blocked.u8", np.uint8).reshape(h, w) > 0
        depth = np.fromfile(f"{src}/{view}_depth.f32", "<f4").reshape(h, w)
        chord = np.fromfile(f"{src}/{view}_chord.f32", "<f4").reshape(h, w)
        assert gpu.shape == cpu.shape, (gpu.shape, cpu.shape)
        excused = near(boundary(gpu) | boundary(cpu) | depth_edges(depth))
        under = cpu & ~gpu
        over = gpu & ~cpu
        print(
            f"{view:6} {int(gpu.sum()):8d} {int(cpu.sum()):8d} | "
            f"{int((under & ~excused).sum()):12d} {int((under & excused).sum()):7d} | "
            f"{int((over & ~excused).sum()):13d} {int((over & excused).sum()):7d}",
            end="",
        )
        try:
            rule = np.fromfile(f"{src}/{view}_rule_blocked.u8", np.uint8).reshape(h, w) > 0
            excused_r = near(boundary(gpu) | boundary(rule) | depth_edges(depth))
            r_under = rule & ~gpu
            r_over = gpu & ~rule
            print(
                f" |       {int((r_under & ~excused_r).sum()):13d} {int((r_under & excused_r).sum()):7d}"
                f" {int((r_over & ~excused_r).sum()):10d} {int((r_over & excused_r).sum()):7d}",
                end="",
            )
        except FileNotFoundError:
            print(f" | {'':>41}", end="")
        if chord_rule:
            thick = chord >= chord_rule
            thin = gpu & ~thick
            excused_t = near(boundary(gpu) | boundary(thick) | depth_edges(depth))
            print(f" | {'':>26}{int((thin & ~excused_t).sum()):10d} {int((thin & excused_t).sum()):7d}", end="")
        print()


if __name__ == "__main__":
    args = sys.argv[1:]
    rule = None
    if "--chord" in args:
        i = args.index("--chord")
        rule = float(args[i + 1])
        del args[i : i + 2]
    main(args[0], rule)
