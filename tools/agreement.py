"""Picture-versus-query agreement.

Run the demo's agreement script, then point this at its output.

Faceted or crystal smoke:

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

Volumetric smoke (the demo's default style; add `SMOKE_DEMO_SWEEP_AGE=16.5`
for a fading cloud, `SMOKE_DEMO_NOISE=0` for the picture without its noise,
`SMOKE_DEMO_NOISE_WITHIN_RULE=capped` or `filled` for the noise kept within the rule,
`SMOKE_DEMO_TIER=0` for the cheapest dial step instead of the top one):

    SMOKE_DEMO_SCRIPT=agreement SMOKE_DEMO_CAPTURE=agree cargo run --release --example demo
    python3 tools/agreement.py agree

The demo saves how much of the scene shows through the smoke at every pixel
(`debug_transmittance`, with tonemapping off) and the rule's answers along
the same segments. Per view:

- blocked, shows through: the rule blocks the segment, but the picture lets
  more than 1 %, 10 % or 30 % of the scene through;
- opaque, not blocked: the picture lets less than 1 % through (the march
  draws that as opaque) where the rule doesn't block. The picture errs
  towards more smoke there, which is fair.

Then the picture's own amount of cloud, -ln(transmittance) / sigma (metres of
full-density cloud, as `occlusion` counts them), against the rule's amount,
by the rule's amount and away from edges: the median of picture / rule, and
how many pixels. Past -ln(0.01) / sigma (0.66 m by default) the picture is
opaque and says no more. Without the noise the two differ only where the rule
and the picture work out the density differently; with it, the noise thins
the picture. With `SmokeSettings::noise_within_rule` capped or filled (the
demo's `SMOKE_DEMO_NOISE_WITHIN_RULE`), nothing the rule blocks shows through.
`overlay.py` paints where the two disagree.

Needs numpy and Pillow.
"""
import sys

import numpy as np
from PIL import Image

VIEWS = ["far", "front", "close", "edge"]


def magenta(path):
    a = np.asarray(Image.open(path).convert("RGB")).astype(np.int32)
    return ((a[..., 0] - a[..., 1]) > 60) & ((a[..., 2] - a[..., 1]) > 60)


def srgb_to_linear(v):
    c = v / 255.0
    return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)


def transmittance(path):
    """How much of the scene shows through, per pixel, from a
    `debug_transmittance` shot: where smoke is drawn, red and blue are full
    and green is the transmittance; anywhere else the scene shows, clear."""
    a = np.asarray(Image.open(path).convert("RGB")).astype(np.float64)
    drawn = (a[..., 0] >= 254) & (a[..., 2] >= 254)
    return np.where(drawn, srgb_to_linear(a[..., 1]), 1.0)


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


def read_meta(src):
    """The run's settings (`meta.txt`, one `key value` per line), if saved."""
    try:
        lines = open(f"{src}/meta.txt").read().splitlines()
    except FileNotFoundError:
        return {}
    return dict(line.split(" ", 1) for line in lines if " " in line)


def load(src, view):
    """The CPU's answers for a view: size, any cloud, amount, depth, rule."""
    try:
        w, h = map(int, open(f"{src}/{view}_size.txt").read().split())
    except FileNotFoundError:
        return None
    cpu = np.fromfile(f"{src}/{view}_cpu_blocked.u8", np.uint8).reshape(h, w) > 0
    chord = np.fromfile(f"{src}/{view}_chord.f32", "<f4").reshape(h, w)
    depth = np.fromfile(f"{src}/{view}_depth.f32", "<f4").reshape(h, w)
    try:
        rule = np.fromfile(f"{src}/{view}_rule_blocked.u8", np.uint8).reshape(h, w) > 0
    except FileNotFoundError:
        rule = None
    return cpu, chord, depth, rule


def hard(src, chord_rule=None):
    print(f"{'view':6} {'drawn':>8} {'hidden':>8} | {'hidden,clear':>12} {'(edge)':>7} | {'drawn,visible':>13} {'(edge)':>7}", end="")
    print(f" | rule: {'blocked,clear':>13} {'(edge)':>7} {'drawn,open':>10} {'(edge)':>7}", end="")
    if chord_rule:
        print(f" | thickness rule {chord_rule} m: {'drawn,thin':>10} {'(edge)':>7}", end="")
    print()
    for view in VIEWS:
        loaded = load(src, view)
        if loaded is None:
            continue
        cpu, chord, depth, rule = loaded
        gpu = magenta(f"{src}/{view}_gpu_mask.png")
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
        if rule is not None:
            excused_r = near(boundary(gpu) | boundary(rule) | depth_edges(depth))
            r_under = rule & ~gpu
            r_over = gpu & ~rule
            print(
                f" |       {int((r_under & ~excused_r).sum()):13d} {int((r_under & excused_r).sum()):7d}"
                f" {int((r_over & ~excused_r).sum()):10d} {int((r_over & excused_r).sum()):7d}",
                end="",
            )
        else:
            print(f" | {'':>41}", end="")
        if chord_rule:
            thick = chord >= chord_rule
            thin = gpu & ~thick
            excused_t = near(boundary(gpu) | boundary(thick) | depth_edges(depth))
            print(f" | {'':>26}{int((thin & ~excused_t).sum()):10d} {int((thin & excused_t).sum()):7d}", end="")
        print()


def volumetric(src, meta):
    sigma = float(meta["sigma"])
    threshold = float(meta["threshold"])
    opaque = -np.log(0.01) / sigma
    print(
        f"age {meta.get('age', '?')} s, dial step {meta.get('tier', 'ultra')}, erosion {meta.get('erosion', '?')}, "
        f"detail erosion {meta.get('detail_erosion', '?')}, noise within the rule "
        f"{meta.get('noise_within_rule', 'Off')}; the rule blocks from {threshold} m of cloud, the picture is "
        f"opaque from {opaque:.2f} m"
    )
    print(
        f"{'view':6} {'smoke':>8} {'blocked':>8} | blocked, shows through: {'>1%':>7} {'>10%':>6} {'>30%':>6} "
        f"{'(edge,>1%)':>10} | opaque, not blocked: {'px':>7} {'(edge)':>7}"
    )
    edges = [0.0, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, opaque]
    ratios = {}
    for view in VIEWS:
        loaded = load(src, view)
        if loaded is None:
            continue
        _, amount, depth, rule = loaded
        t = transmittance(f"{src}/{view}_gpu_transmittance.png")
        assert t.shape == amount.shape, (t.shape, amount.shape)
        dark = t < 0.01
        excused = near(boundary(rule) | boundary(dark) | depth_edges(depth))
        smoke = (t < 0.999) | (amount > 0)
        shows = [rule & (t > x) & ~excused for x in (0.01, 0.1, 0.3)]
        shows_edge = rule & (t > 0.01) & excused
        dark_open = dark & ~rule
        print(
            f"{view:6} {int(smoke.sum()):8d} {int(rule.sum()):8d} |                        "
            + " ".join(f"{int(s.sum()):{n}d}" for s, n in zip(shows, (7, 6, 6)))
            + f" {int(shows_edge.sum()):10d} |                      {int((dark_open & ~excused).sum()):7d}"
            f" {int((dark_open & excused).sum()):7d}"
        )
        # The picture's amount, where it says anything (it isn't opaque).
        picture = -np.log(np.clip(t, 1e-6, 1.0)) / sigma + 0.0
        inside = ~excused & (amount > 0) & (amount < opaque) & ~dark
        ratios[view] = [
            picture[inside & (amount >= lo) & (amount < hi)] / amount[inside & (amount >= lo) & (amount < hi)]
            for lo, hi in zip(edges, edges[1:])
        ]
    print()
    print("The picture's amount of cloud over the rule's, median (pixels), by the rule's amount:")
    print(f"{'rule (m)':12}" + "".join(f"{v:>16}" for v in ratios))
    for k, (lo, hi) in enumerate(zip(edges, edges[1:])):
        cells = []
        for v in ratios:
            r = ratios[v][k]
            cells.append(f"{np.median(r):6.3f} ({len(r):6d})" if len(r) else f"{'':>6} ({0:6d})")
        print(f"{lo:4.2f}-{hi:4.2f}   " + "".join(f"{c:>16}" for c in cells))
    every = np.concatenate([r for v in ratios for r in ratios[v]]) if ratios else np.zeros(0)
    if len(every):
        q = np.percentile(every, [10, 50, 90])
        print(f"all bins: median {q[1]:.3f}, 10th to 90th percentile {q[0]:.3f} to {q[2]:.3f}, {len(every)} pixels")


def main(src, chord_rule=None):
    meta = read_meta(src)
    if meta.get("style") == "Volumetric":
        volumetric(src, meta)
    else:
        if "tier" in meta:
            print(f"dial step {meta['tier']}")
        hard(src, chord_rule)


if __name__ == "__main__":
    args = sys.argv[1:]
    rule = None
    if "--chord" in args:
        i = args.index("--chord")
        rule = float(args[i + 1])
        del args[i : i + 2]
    main(args[0], rule)
