"""Cost and fairness report for the quality dial.

Run the demo's tier sweep, then point this at its output:

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

Needs numpy and Pillow.
"""
import csv
import sys

import numpy as np
from PIL import Image


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


def main(src):
    rows = list(csv.DictReader(open(f"{src}/tiers.csv")))
    views = list(dict.fromkeys(r["view"] for r in rows))
    print(f"{'view':6} {'step':8} {'gpu ms':>9} {'rel':>5} {'smoke px':>9} {'missing':>8} {'missing>1px':>11} {'extra':>7}")
    worst = 0
    for v in views:
        top = max(int(r["tier"]) for r in rows if r["view"] == v)
        ref = mask(src, f"{v}_tier{top}_mask")
        top_ms = float(next(r["gpu_ms"] for r in rows if r["view"] == v and int(r["tier"]) == top))
        for r in [r for r in rows if r["view"] == v]:
            t = int(r["tier"])
            m = mask(src, f"{v}_tier{t}_mask")
            missing = ref & ~m
            missing_far = ref & ~dilate(m, 1)
            extra = m & ~ref
            worst = max(worst, int(missing_far.sum()))
            ms = float(r["gpu_ms"]) if r["gpu_ms"] else float("nan")
            print(
                f"{v:6} {r['tier_name']:8} {ms:9.2f} {ms / top_ms:5.2f} {int(ref.sum()):9d} "
                f"{int(missing.sum()):8d} {int(missing_far.sum()):11d} {int(extra.sum()):7d}"
            )
    print("fair" if worst == 0 else f"UNFAIR: {worst} pixels missing beyond the anti-aliasing band")
    return worst == 0


if __name__ == "__main__":
    sys.exit(0 if main(sys.argv[1]) else 1)
