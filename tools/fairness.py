"""Cost and fairness report for the quality dial.

Faceted or crystal smoke: run the demo's tier sweep, then point this at its
output:

    SMOKE_DEMO_STYLE=faceted SMOKE_DEMO_SCRIPT=tiers SMOKE_DEMO_CAPTURE=sweep \
        cargo run --release --example demo
    python3 tools/fairness.py sweep

For each view it compares every step's silhouette mask (faceted smoke drawn
flat magenta with `debug_silhouette`) with the top step's:

- missing: pixels that are smoke at the top step but not at this one. Anything
  beyond the one-pixel anti-aliasing band (`missing>1px`) is a fairness bug:
  a player on this step could see through smoke a player on the top step
  can't.
- extra: pixels that are smoke only at this step (a cheaper step may thicken
  silhouettes slightly; it must never thin them).

Volumetric smoke has no hard silhouette. Run the demo's agreement script at
the best step and at each step to check, with the smoke at the same age
(grown, or `SMOKE_DEMO_SWEEP_AGE`), then give the best step's run first:

    for t in 5 4 3 2 1 0; do
        SMOKE_DEMO_SCRIPT=agreement SMOKE_DEMO_TIER=$t SMOKE_DEMO_CAPTURE=agree/t$t \\
            cargo run --release --example demo
    done
    python3 tools/fairness.py --volumetric agree/t5 agree/t4 agree/t3 agree/t2 agree/t1 agree/t0

For each view it compares how much of the scene shows through every smoke
pixel at each step with the best step's, away from the smoke's outlines and
depth edges (a pixel there covers both sides):

- more>0.1: the share of the smoke's pixels where the step lets more than a
  tenth more of the scene through. The noise, sampled at fewer and longer
  steps, falls a little differently: most at the two cheapest steps, which
  march at a quarter of the resolution, and on small, far smokes, where a
  few pixels are a large share.
- less>0.1: where it lets a tenth less through (more smoke, which is fair).
- more>0.3: where it lets more than 0.3 more through, enough to make out
  something the best step hides. More than 1 % of a view's smoke pixels at
  any step is a fairness bug.

Needs numpy and Pillow, and agreement.py next to this file.
"""
import csv
import sys

import numpy as np
from PIL import Image

from agreement import VIEWS, boundary, depth_edges, load, near, read_meta, transmittance

# A cheaper step may let more than SHOWS_THROUGH more of the scene through
# than the best step on at most BOUND % of a view's smoke pixels.
SHOWS_THROUGH = 0.3
BOUND = 1.0


def mask(src, name):
    a = np.asarray(Image.open(f"{src}/{name}.png").convert("RGB")).astype(np.int32)
    r, g, b = a[..., 0], a[..., 1], a[..., 2]
    # Magenta after tonemapping: red and blue well above green.
    return (r - g > 60) & (b - g > 60)


def dilate(m, n=1):
    out = m.copy()
    for _ in range(n):
        p = np.pad(out, 1)
        out = p[1:-1, 1:-1] | p[:-2, 1:-1] | p[2:, 1:-1] | p[1:-1, :-2] | p[1:-1, 2:]
    return out


def cost(row):
    """GPU time where the run had GPU timings; else the frame time with smoke
    minus the same view's without (macOS, where timestamps lose the device)."""
    if row["gpu_ms"]:
        return float(row["gpu_ms"])
    if row.get("frame_ms") and row.get("empty_frame_ms"):
        return float(row["frame_ms"]) - float(row["empty_frame_ms"])
    return float("nan")


def main(src):
    rows = list(csv.DictReader(open(f"{src}/tiers.csv")))
    views = list(dict.fromkeys(r["view"] for r in rows))
    print(f"{'view':6} {'step':8} {'cost ms':>9} {'rel':>5} {'smoke px':>9} {'missing':>8} {'missing>1px':>11} {'extra':>7}")
    worst = 0
    for v in views:
        top = max(int(r["tier"]) for r in rows if r["view"] == v)
        ref = mask(src, f"{v}_tier{top}_mask")
        top_ms = cost(next(r for r in rows if r["view"] == v and int(r["tier"]) == top))
        for r in [r for r in rows if r["view"] == v]:
            t = int(r["tier"])
            m = mask(src, f"{v}_tier{t}_mask")
            missing = ref & ~m
            missing_far = ref & ~dilate(m, 1)
            extra = m & ~ref
            worst = max(worst, int(missing_far.sum()))
            ms = cost(r)
            print(
                f"{v:6} {r['tier_name']:8} {ms:9.2f} {ms / top_ms:5.2f} {int(ref.sum()):9d} "
                f"{int(missing.sum()):8d} {int(missing_far.sum()):11d} {int(extra.sum()):7d}"
            )
    print("fair" if worst == 0 else f"UNFAIR: {worst} pixels missing beyond the anti-aliasing band")
    return worst == 0


def volumetric(best, others):
    """Each step's agreement run against the best step's, view by view."""
    meta = read_meta(best)
    if meta.get("style") != "Volumetric":
        sys.exit(f"{best} is not an agreement run of volumetric smoke")
    for src in others:
        m = read_meta(src)
        keys = ("style", "age", "sigma", "erosion", "detail_erosion", "noise_within_rule")
        differ = [k for k in keys if m.get(k) != meta.get(k)]
        if differ:
            sys.exit(f"{src} differs from {best} in {', '.join(differ)}: compare one smoke at one age")
    print(
        f"age {meta.get('age', '?')} s, noise within the rule {meta.get('noise_within_rule', 'Off')}, "
        f"against {meta.get('tier', 'the best step')} ({best})"
    )
    print(f"{'view':6} {'step':8} {'smoke px':>9} {'more>0.1':>9} {'less>0.1':>9} {'more>0.3':>9} {'(px)':>6}")
    worst = (0.0, "")
    for view in VIEWS:
        loaded = load(best, view)
        if loaded is None:
            continue
        depth = loaded[2]
        t0 = transmittance(f"{best}/{view}_gpu_transmittance.png")
        for src in others:
            t1 = transmittance(f"{src}/{view}_gpu_transmittance.png")
            s0, s1 = t0 < 0.99, t1 < 0.99
            smoke = (s0 | s1) & ~near(boundary(s0) | boundary(s1) | depth_edges(depth))
            n = max(int(smoke.sum()), 1)
            more = 100.0 * float(((t1 - t0) > 0.1)[smoke].sum()) / n
            less = 100.0 * float(((t0 - t1) > 0.1)[smoke].sum()) / n
            through = int(((t1 - t0) > SHOWS_THROUGH)[smoke].sum())
            step = read_meta(src).get("tier", src)
            if 100.0 * through / n > worst[0]:
                worst = (100.0 * through / n, f"{view}, {step}")
            print(f"{view:6} {step:8} {n:9d} {more:8.2f}% {less:8.2f}% {100.0 * through / n:8.3f}% {through:6d}")
    fair = worst[0] <= BOUND
    print(
        f"{'fair: at most' if fair else 'UNFAIR:'} {worst[0]:.2f} % of a view's smoke pixels "
        f"({worst[1] or 'anywhere'}) let more than {SHOWS_THROUGH:g} more of the scene through than the best step "
        f"(the bound is {BOUND:g} %)"
    )
    return fair


if __name__ == "__main__":
    args = sys.argv[1:]
    if args[:1] == ["--volumetric"]:
        sys.exit(0 if volumetric(args[1], args[2:]) else 1)
    sys.exit(0 if main(args[0]) else 1)
