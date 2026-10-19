//! Exact amounts of cloud along a segment for clouds made of convex pieces:
//! the crystal style (unblended polyhedra) and the faceted style's core rule
//! (each puff's inscribed sphere).
//!
//! Both walk the puff bins along the segment and, in each bin cell, add up
//! the length of the union of the pieces' intervals inside that cell. Cells
//! split the segment into disjoint parts, so nothing is counted twice.

use glam::{IVec3, Vec3};

use crate::facets::{Bins, LivePuff, Puff};
use crate::holes::{HOLE_BUFFER, HoleShape};
use crate::math::{safe_recip, slab};

/// Intervals one bin cell collects before merging them.
const CELL_INTERVALS: usize = 64;

/// Words of the puffs-seen mask of [`ChordBounds`] (puffs past it are
/// tested again in every cell, which only costs time).
const SEEN_WORDS: usize = 32;

/// A few intervals along a ray, merged on demand, minus where the ray is
/// inside a hole. Never allocates.
struct Intervals<'h> {
    buf: [(f32, f32); CELL_INTERVALS],
    n: usize,
    /// Sorted, disjoint stretches that don't count.
    holes: &'h [(f32, f32)],
    /// After an overflow (see [`Self::push`]): length already counted, and
    /// the end of what it covered.
    banked: f32,
    floor: f32,
}

impl<'h> Intervals<'h> {
    fn new(holes: &'h [(f32, f32)]) -> Self {
        Self {
            buf: [(0.0, 0.0); CELL_INTERVALS],
            n: 0,
            holes,
            banked: 0.0,
            floor: f32::NEG_INFINITY,
        }
    }

    fn clear(&mut self) {
        self.n = 0;
        self.banked = 0.0;
        self.floor = f32::NEG_INFINITY;
    }

    fn push(&mut self, lo: f32, hi: f32) {
        let lo = lo.max(self.floor);
        if hi <= lo {
            return;
        }
        if self.n == CELL_INTERVALS {
            self.merge();
            if self.n == CELL_INTERVALS {
                // Pathological: more disjoint pieces than fit. Count them and
                // only count what lies past them from now on. This can only
                // undercount, never count anything twice.
                self.banked += self.merged_length();
                self.floor = self.buf[..self.n].iter().fold(self.floor, |f, iv| f.max(iv.1));
                self.n = 0;
                let lo = lo.max(self.floor);
                if hi <= lo {
                    return;
                }
            }
        }
        self.buf[self.n] = (lo, hi);
        self.n += 1;
    }

    /// Like [`Self::push`], but never drops anything: returns `false` if
    /// the interval wouldn't fit even after merging.
    fn push_exact(&mut self, lo: f32, hi: f32) -> bool {
        if hi <= lo {
            return true;
        }
        if self.n == CELL_INTERVALS {
            self.merge();
            if self.n == CELL_INTERVALS {
                return false;
            }
        }
        self.buf[self.n] = (lo, hi);
        self.n += 1;
        true
    }

    /// Sort by start and merge overlapping intervals, in place.
    fn merge(&mut self) {
        let v = &mut self.buf[..self.n];
        // Insertion sort: cells rarely see more than a handful.
        for i in 1..v.len() {
            let x = v[i];
            let mut j = i;
            while j > 0 && v[j - 1].0 > x.0 {
                v[j] = v[j - 1];
                j -= 1;
            }
            v[j] = x;
        }
        let mut out = 0;
        for i in 0..self.n {
            let (lo, hi) = self.buf[i];
            if out > 0 && lo <= self.buf[out - 1].1 {
                if hi > self.buf[out - 1].1 {
                    self.buf[out - 1].1 = hi;
                }
            } else {
                self.buf[out] = (lo, hi);
                out += 1;
            }
        }
        self.n = out;
    }

    /// Length of the (merged) intervals outside the holes.
    fn merged_length(&self) -> f32 {
        let mut total = 0.0;
        for &(lo, hi) in &self.buf[..self.n] {
            let mut len = hi - lo;
            for &(h0, h1) in self.holes {
                if h0 >= hi {
                    break;
                }
                let overlap = h1.min(hi) - h0.max(lo);
                if overlap > 0.0 {
                    len -= overlap;
                }
            }
            if len > 0.0 {
                total += len;
            }
        }
        total
    }

    /// Length of the union, outside the holes.
    fn covered(&mut self) -> f32 {
        self.merge();
        self.banked + self.merged_length()
    }
}

/// Walk the bin cells along `ro + rd·t`, `t0 ≤ t ≤ t1`, in order, calling
/// `cell(list, enter, exit)` for each; stops early when it returns `true`.
fn walk_bins(
    bins: &Bins,
    ro: Vec3,
    rd: Vec3,
    t0: f32,
    t1: f32,
    mut cell: impl FnMut(&[u32], f32, f32) -> bool,
) -> bool {
    let inv = safe_recip(rd);
    let (b0, b1) = slab(ro, inv, bins.min, bins.max());
    let mut t = t0.max(b0);
    let end = t1.min(b1);
    if end <= t {
        return false;
    }
    let p = ro + rd * t;
    // Non-negative after the max, so truncation is floor.
    let mut c = ((p - bins.min) / bins.cell)
        .max(Vec3::ZERO)
        .as_ivec3()
        .min(bins.dims - 1);
    let mut step = IVec3::ZERO;
    let mut t_max = Vec3::splat(f32::INFINITY);
    let mut t_delta = Vec3::splat(f32::INFINITY);
    for axis in 0..3 {
        let d = rd[axis];
        if d > 0.0 {
            step[axis] = 1;
            t_max[axis] = (bins.min[axis] + (c[axis] + 1) as f32 * bins.cell - ro[axis]) / d;
            t_delta[axis] = bins.cell / d;
        } else if d < 0.0 {
            step[axis] = -1;
            t_max[axis] = (bins.min[axis] + c[axis] as f32 * bins.cell - ro[axis]) / d;
            t_delta[axis] = -bins.cell / d;
        }
    }
    loop {
        let exit = t_max.min_element().min(end).max(t);
        if cell(bins.list(c), t, exit) {
            return true;
        }
        if exit >= end {
            return false;
        }
        t = exit;
        let axis = if t_max.x <= t_max.y && t_max.x <= t_max.z {
            0
        } else if t_max.y <= t_max.z {
            1
        } else {
            2
        };
        c[axis] += step[axis];
        t_max[axis] += t_delta[axis];
        if c[axis] < 0 || c[axis] >= bins.dims[axis] {
            return false;
        }
    }
}

/// Where the ray `ro + rd·t` (`rd` a unit vector) is inside the puff's
/// polyhedron, if anywhere. Always inlined: it is the inner loop of the
/// crystal rule, which called out of line runs 40 % slower in thin clouds.
#[inline(always)]
pub fn polyhedron_interval(p: &LivePuff, ro: Vec3, rd: Vec3) -> Option<(f32, f32)> {
    let r = p.radius;
    if r <= 0.0 {
        return None;
    }
    // Bounding sphere first: most listed puffs are missed.
    let oc = ro - p.center;
    let reach = r * p.ratio;
    let b = oc.dot(rd);
    if b * b - (oc.length_squared() - reach * reach) <= 0.0 {
        return None;
    }
    // Then the slab between each pair of opposite faces, in the puff's frame.
    let q = p.to_local * oc;
    let u = p.to_local * rd;
    let (mut enter, mut exit) = (f32::NEG_INFINITY, f32::INFINITY);
    for n in p.shape.face_normals() {
        let nq = n.dot(q);
        let nu = n.dot(u);
        if nu.abs() < 1e-12 {
            if nq.abs() > r {
                return None;
            }
            continue;
        }
        let ta = (-r - nq) / nu;
        let tb = (r - nq) / nu;
        enter = enter.max(ta.min(tb));
        exit = exit.min(ta.max(tb));
    }
    (exit > enter).then_some((enter, exit))
}

/// Is `x` inside the puff's polyhedron?
#[inline]
pub fn inside_polyhedron(p: &LivePuff, x: Vec3) -> bool {
    if p.radius <= 0.0 {
        return false;
    }
    let q = p.to_local * (x - p.center);
    p.shape.face_normals().iter().all(|n| n.dot(q).abs() <= p.radius)
}

/// Where the ray `ro + rd·t` (`rd` a unit vector) is inside a sphere.
#[inline]
fn sphere_interval(center: Vec3, radius: f32, ro: Vec3, rd: Vec3) -> Option<(f32, f32)> {
    if radius <= 0.0 {
        return None;
    }
    let oc = ro - center;
    let b = oc.dot(rd);
    let disc = b * b - (oc.length_squared() - radius * radius);
    if disc <= 0.0 {
        return None;
    }
    let s = disc.sqrt();
    Some((-b - s, -b + s))
}

/// Where the ray `ro + rd·t` (`rd` a unit vector) is within `radius` of the
/// segment `a..b`.
fn capsule_interval(a: Vec3, b: Vec3, radius: f32, ro: Vec3, rd: Vec3) -> Option<(f32, f32)> {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 < 1e-12 {
        return sphere_interval(a, radius, ro, rd);
    }
    let len = len2.sqrt();
    let u = ab / len;
    let oc = ro - a;
    // The capsule is convex, so the ray meets it in one interval: the hull
    // of where it meets its cylinder (between the end planes) and its caps.
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    let mut take = |iv: Option<(f32, f32)>| {
        if let Some((x, y)) = iv
            && y > x
        {
            lo = lo.min(x);
            hi = hi.max(y);
        }
    };
    take(sphere_interval(a, radius, ro, rd));
    take(sphere_interval(b, radius, ro, rd));
    // Infinite cylinder.
    let du = rd.dot(u);
    let ou = oc.dot(u);
    let d_perp = rd - u * du;
    let o_perp = oc - u * ou;
    let qa = d_perp.length_squared();
    let qb = o_perp.dot(d_perp);
    let qc = o_perp.length_squared() - radius * radius;
    let cylinder = if qa < 1e-12 {
        (qc <= 0.0).then_some((f32::NEG_INFINITY, f32::INFINITY))
    } else {
        let disc = qb * qb - qa * qc;
        (disc > 0.0).then(|| {
            let s = disc.sqrt();
            ((-qb - s) / qa, (-qb + s) / qa)
        })
    };
    // Between the end planes.
    let slab = if du.abs() < 1e-12 {
        (0.0..=len).contains(&ou).then_some((f32::NEG_INFINITY, f32::INFINITY))
    } else {
        let (x, y) = (-ou / du, (len - ou) / du);
        Some((x.min(y), x.max(y)))
    };
    if let (Some(c), Some(s)) = (cylinder, slab) {
        take(Some((c.0.max(s.0), c.1.min(s.1))));
    }
    (hi > lo).then_some((lo, hi))
}

/// A crystal cloud at one tick: unblended polyhedra, holes already applied
/// (they shrink whole puffs). Internal frame.
#[derive(Clone, Copy, Debug)]
pub struct CrystalField<'a> {
    pub bins: &'a Bins,
    pub live: &'a [LivePuff],
}

impl CrystalField<'_> {
    /// Is `p` inside any puff?
    pub fn inside(&self, p: Vec3) -> bool {
        let Some(c) = self.bins.cell_of(p) else {
            return false;
        };
        self.bins
            .list(c)
            .iter()
            .any(|&i| inside_polyhedron(&self.live[i as usize], p))
    }

    /// Add the length of `ro + rd·t`, `t0 ≤ t ≤ t1`, inside the cloud to
    /// `acc` (exactly: the union of the puffs), a bin cell at a time. Stops
    /// (and returns `true`) as soon as `stop(*acc)` holds.
    pub fn amount(&self, ro: Vec3, rd: Vec3, t0: f32, t1: f32, acc: &mut f32, stop: &dyn Fn(f32) -> bool) -> bool {
        let mut iv = Intervals::new(&[]);
        walk_bins(self.bins, ro, rd, t0, t1, |list, enter, exit| {
            iv.clear();
            for &i in list {
                if let Some((lo, hi)) = polyhedron_interval(&self.live[i as usize], ro, rd) {
                    iv.push(lo.max(enter), hi.min(exit));
                }
            }
            if iv.n == 0 && iv.banked == 0.0 {
                return false;
            }
            let covered = iv.covered();
            if covered > 0.0 {
                *acc += covered;
                return stop(*acc);
            }
            false
        })
    }
}

/// The faceted style's cheap rule: only each puff's core (its inscribed
/// sphere, kept clear of walls) counts as cloud, minus a generous bound on
/// every hole. Everything it counts is inside the drawn surface, so it never
/// blocks a line the picture leaves clear. Internal frame.
#[derive(Clone, Copy, Debug)]
pub struct CoreField<'a> {
    pub bins: &'a Bins,
    pub live: &'a [LivePuff],
    pub holes: &'a [HoleShape],
}

impl CoreField<'_> {
    /// Is `p` inside any puff's core (and outside every hole)?
    pub fn inside(&self, p: Vec3) -> bool {
        let Some(c) = self.bins.cell_of(p) else {
            return false;
        };
        let cored = self.bins.list(c).iter().any(|&i| {
            let puff = &self.live[i as usize];
            puff.core > 0.0 && p.distance_squared(puff.center) <= puff.core * puff.core
        });
        cored
            && !self
                .holes
                .iter()
                .any(|h| hole_bound(h).is_some_and(|r| crate::math::distance_to_segment(p, h.a, h.b) <= r))
    }

    /// Add the length of `ro + rd·t`, `t0 ≤ t ≤ t1`, inside the cores to
    /// `acc`, a bin cell at a time. Stops (and returns `true`) as soon as
    /// `stop(*acc)` holds.
    pub fn amount(&self, ro: Vec3, rd: Vec3, t0: f32, t1: f32, acc: &mut f32, stop: &dyn Fn(f32) -> bool) -> bool {
        let mut spans = [(0.0_f32, 0.0_f32); HOLE_BUFFER];
        let holes = hole_spans(self.holes, ro, rd, &mut spans);
        let mut iv = Intervals::new(holes);
        walk_bins(self.bins, ro, rd, t0, t1, |list, enter, exit| {
            iv.clear();
            for &i in list {
                let puff = &self.live[i as usize];
                if let Some((lo, hi)) = sphere_interval(puff.center, puff.core, ro, rd) {
                    iv.push(lo.max(enter), hi.min(exit));
                }
            }
            if iv.n == 0 && iv.banked == 0.0 {
                return false;
            }
            let covered = iv.covered();
            if covered > 0.0 {
                *acc += covered;
                return stop(*acc);
            }
            false
        })
    }
}

/// Bounds on the faceted style's exact rule ([`crate::FacetField`]) from
/// chords alone: the blended cloud contains every puff's polyhedron (the
/// smooth union lies at or below the plain minimum), clipped to the fill's
/// space (the ball of [`Puff::room`] about the centre is inside it) and cut
/// by the holes (a capsule around each contains everything it cuts); and it
/// lies inside every polyhedron grown by the blend radius (a chain of smooth
/// minima lies at most `blend` below the plain minimum), which holes and the
/// clip only shrink. Internal frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChordBounds<'a> {
    pub bins: &'a Bins,
    pub live: &'a [LivePuff],
    /// The puffs `live` was made from, in the same order (for their room).
    pub puffs: &'a [Puff],
    pub blend: f32,
    pub holes: &'a [HoleShape],
}

/// What [`ChordBounds::amount_until`] found along a segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bounds {
    /// At least this much cloud (metres).
    pub lower: f32,
    /// At most this much (infinite when stopped early, or past the room
    /// for keeping chords).
    pub upper: f32,
}

/// Puffs whose grown chords are kept for the upper bound.
const CHORD_PUFFS: usize = 96;

impl ChordBounds<'_> {
    /// Bounds on the length of `ro + rd·t`, `t0 ≤ t ≤ t1`, inside the cloud
    /// (`rd` a unit vector), stopping (with the lower bound so far and no
    /// upper bound) as soon as `enough(lower)` holds for the lower bound.
    /// Never allocates.
    pub fn amount_until(&self, ro: Vec3, rd: Vec3, t0: f32, t1: f32, enough: &dyn Fn(f32) -> bool) -> Bounds {
        let mut spans = [(0.0_f32, 0.0_f32); HOLE_BUFFER];
        let holes = hole_spans(self.holes, ro, rd, &mut spans);
        // Each puff's chord is one interval along the whole ray (puffs are
        // convex), found the first time a cell lists the puff; the unions
        // are taken over the segment, not per cell. The grown chords are
        // only worked out if the lower bound doesn't settle it first.
        let mut lower = Intervals::new(holes);
        let mut seen = [0u64; SEEN_WORDS];
        let mut within = [0u32; CHORD_PUFFS];
        let mut kept = 0;
        let mut overflow = false;
        let k = self.blend;
        let stopped = walk_bins(self.bins, ro, rd, t0, t1, |list, _enter, _exit| {
            let mut pushed = false;
            for &i in list {
                let (word, bit) = ((i / 64) as usize, 1u64 << (i % 64));
                if let Some(w) = seen.get_mut(word) {
                    if *w & bit != 0 {
                        continue;
                    }
                    *w |= bit;
                }
                let p = &self.live[i as usize];
                if p.radius <= 0.0 {
                    continue;
                }
                // Within the grown puff's bounding sphere?
                let reach = (p.radius + k) * p.ratio;
                let oc = ro - p.center;
                let b = oc.dot(rd);
                if b * b - (oc.length_squared() - reach * reach) <= 0.0 {
                    continue;
                }
                if kept < CHORD_PUFFS {
                    within[kept] = i;
                    kept += 1;
                } else {
                    overflow = true;
                }
                if let (Some((lo, hi)), Some((rlo, rhi))) = (
                    polyhedron_interval(p, ro, rd),
                    sphere_interval(p.center, self.puffs[i as usize].room, ro, rd),
                ) {
                    lower.push(lo.max(rlo).max(t0), hi.min(rhi).min(t1));
                    pushed = true;
                }
            }
            pushed && enough(lower.covered())
        });
        let mut out = Bounds {
            lower: lower.covered(),
            upper: f32::INFINITY,
        };
        if !stopped && !overflow {
            let mut upper = Intervals::new(&[]);
            let mut fits = true;
            for &i in &within[..kept] {
                let p = &self.live[i as usize];
                let grown = LivePuff {
                    radius: p.radius + k,
                    ..*p
                };
                if let Some((lo, hi)) = polyhedron_interval(&grown, ro, rd) {
                    fits &= upper.push_exact(lo.max(t0), hi.min(t1));
                }
            }
            if fits {
                out.upper = upper.covered();
            }
        }
        out
    }
}

/// Radius of a capsule around the hole's segment containing everything the
/// faceted picture cuts away (see [`crate::facets::hole_sdf`]), if it cuts
/// anything.
#[inline]
fn hole_bound(h: &HoleShape) -> Option<f32> {
    if h.strength <= 0.0 {
        return None;
    }
    let r = h.radius * (h.strength * 1.5).min(1.0);
    // Blast cavity: an icosahedron of inradius 0.8 r (corners 1.0067 r out);
    // tunnel: a hexagonal prism of apothem r (corners 1.1547 r out).
    Some(if h.a == h.b { r * 1.01 } else { r * 1.16 })
}

/// Where the ray `ro + rd·t` (`rd` a unit vector) is inside a capsule
/// around a hole (see [`hole_bound`]), from every hole, sorted and merged
/// in `buf`. When more separate stretches cross the ray than `buf` holds,
/// the two nearest are joined, so the gap between them counts as hole too:
/// the rules that subtract these count less cloud for it, never more.
#[inline(always)]
fn hole_spans<'b>(holes: &[HoleShape], ro: Vec3, rd: Vec3, buf: &'b mut [(f32, f32); HOLE_BUFFER]) -> &'b [(f32, f32)] {
    let mut n = 0;
    for (k, h) in holes.iter().enumerate() {
        if let Some(r) = hole_bound(h)
            && let Some(span) = capsule_interval(h.a, h.b, r, ro, rd)
        {
            if n == HOLE_BUFFER {
                return more_hole_spans(&holes[k..], ro, rd, buf);
            }
            buf[n] = span;
            n += 1;
        }
    }
    merge_sorted(&mut buf[..n])
}

/// [`hole_spans`] once `buf` is full: the spans of `holes` too, making room
/// for each by merging the buffer and, if its spans are all apart, joining
/// the two nearest (the first such pair, on a tie). Out of line and called
/// from outside the loop: handled inside it, inline or by a call, it made
/// the core rule's checks 9–11 % slower halfway through a fade on an Apple
/// M4, though it never ran.
#[cold]
#[inline(never)]
fn more_hole_spans<'b>(
    holes: &[HoleShape],
    ro: Vec3,
    rd: Vec3,
    buf: &'b mut [(f32, f32); HOLE_BUFFER],
) -> &'b [(f32, f32)] {
    let mut n = HOLE_BUFFER;
    for h in holes {
        let Some(span) = hole_bound(h).and_then(|r| capsule_interval(h.a, h.b, r, ro, rd)) else {
            continue;
        };
        if n == HOLE_BUFFER {
            n = merge_sorted(&mut buf[..]).len();
            if n == HOLE_BUFFER {
                // Sorted and disjoint: join the pair with the smallest gap.
                let mut k = 1;
                for j in 2..n {
                    if buf[j].0 - buf[j - 1].1 < buf[k].0 - buf[k - 1].1 {
                        k = j;
                    }
                }
                buf[k - 1].1 = buf[k].1;
                buf.copy_within(k + 1..n, k);
                n -= 1;
            }
        }
        buf[n] = span;
        n += 1;
    }
    merge_sorted(&mut buf[..n])
}

/// Sort intervals by start and merge overlaps; returns the merged prefix.
fn merge_sorted(v: &mut [(f32, f32)]) -> &[(f32, f32)] {
    for i in 1..v.len() {
        let x = v[i];
        let mut j = i;
        while j > 0 && v[j - 1].0 > x.0 {
            v[j] = v[j - 1];
            j -= 1;
        }
        v[j] = x;
    }
    let mut out = 0;
    for i in 0..v.len() {
        let (lo, hi) = v[i];
        if out > 0 && lo <= v[out - 1].1 {
            if hi > v[out - 1].1 {
                v[out - 1].1 = hi;
            }
        } else {
            v[out] = (lo, hi);
            out += 1;
        }
    }
    &v[..out]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facets::Polyhedron;
    use glam::Mat3;

    fn puff(center: Vec3, radius: f32, shape: Polyhedron, to_local: Mat3) -> LivePuff {
        LivePuff {
            center,
            radius,
            to_local,
            shape,
            ratio: shape.corner_ratio(),
            core: radius,
        }
    }

    #[test]
    fn polyhedron_chords_match_sampling() {
        let rot = crate::math::rotation(Vec3::new(0.3, 0.8, -0.5).normalize(), 0.7).transpose();
        for shape in [
            Polyhedron::Cube,
            Polyhedron::Octahedron,
            Polyhedron::Dodecahedron,
            Polyhedron::Icosahedron,
        ] {
            let p = puff(Vec3::new(0.2, -0.1, 0.4), 0.8, shape, rot);
            for k in 0..40 {
                let a = k as f32 * 0.61;
                let (s, c) = crate::math::sin_cos(a);
                let ro = Vec3::new(-3.0, 0.3 * s, 0.5 * c);
                let rd = (Vec3::new(3.0, 0.2 * c, -0.4 * s) - ro).normalize();
                let exact = polyhedron_interval(&p, ro, rd).map_or(0.0, |(lo, hi)| hi - lo);
                let steps = 20_000;
                let dt = 6.0 / steps as f32;
                let sampled = (0..steps)
                    .filter(|&i| inside_polyhedron(&p, ro + rd * ((i as f32 + 0.5) * dt)))
                    .count() as f32
                    * dt;
                assert!(
                    (exact - sampled).abs() < 2e-3,
                    "{shape:?} ray {k}: {exact} vs {sampled}"
                );
            }
        }
    }

    #[test]
    fn capsules_contain_their_segment_and_nothing_far() {
        let (a, b) = (Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 0.5, 0.0));
        let ro = Vec3::new(0.0, -3.0, 0.0);
        let iv = capsule_interval(a, b, 0.3, ro, Vec3::Y).unwrap();
        // Crosses the segment near y = 0.25: inside from about -0.05 to 0.55.
        assert!(iv.0 > 2.8 && iv.0 < 3.0 && iv.1 > 3.4 && iv.1 < 3.6, "{iv:?}");
        assert!(capsule_interval(a, b, 0.3, Vec3::new(5.0, -3.0, 0.0), Vec3::Y).is_none());
        // Along the axis: from cap to cap.
        let along = capsule_interval(a, b, 0.3, a - (b - a).normalize() * 2.0, (b - a).normalize()).unwrap();
        assert!((along.1 - along.0 - ((b - a).length() + 0.6)).abs() < 1e-4, "{along:?}");
    }

    #[test]
    fn hole_spans_cover_every_hole() {
        // Small blasts along the ray, unevenly spaced: more separate spans
        // than the buffer holds.
        let holes: Vec<HoleShape> = (0..41)
            .map(|i| {
                let c = Vec3::new(i as f32 * 0.25 + if i % 3 == 0 { 0.05 } else { 0.0 }, 0.0, 0.0);
                HoleShape {
                    a: c,
                    b: c,
                    radius: 0.08,
                    strength: 1.0,
                }
            })
            .collect();
        let (ro, rd) = (Vec3::new(-1.0, 0.0, 0.0), Vec3::X);
        let truth: Vec<(f32, f32)> = holes
            .iter()
            .filter_map(|h| hole_bound(h).and_then(|r| capsule_interval(h.a, h.b, r, ro, rd)))
            .collect();
        assert_eq!(truth.len(), 41);
        let mut buf = [(0.0, 0.0); HOLE_BUFFER];
        let spans = hole_spans(&holes, ro, rd, &mut buf);
        assert_eq!(spans.len(), HOLE_BUFFER);
        assert!(
            spans.windows(2).all(|w| w[0].1 < w[1].0),
            "not sorted and disjoint: {spans:?}"
        );
        // Every hole is covered, and every span starts and ends where a
        // hole does: joining only adds the gaps between them.
        for t in &truth {
            assert!(spans.iter().any(|s| s.0 <= t.0 && t.1 <= s.1), "{t:?} is left out");
        }
        for s in spans {
            assert!(
                truth.iter().any(|t| t.0 == s.0) && truth.iter().any(|t| t.1 == s.1),
                "{s:?}"
            );
        }
        // With room for them all, the spans are the holes themselves.
        let mut buf = [(0.0, 0.0); HOLE_BUFFER];
        assert_eq!(
            hole_spans(&holes[..HOLE_BUFFER], ro, rd, &mut buf),
            &truth[..HOLE_BUFFER]
        );
    }

    #[test]
    fn unions_count_overlaps_once() {
        let mut iv = Intervals::new(&[]);
        iv.push(0.0, 1.0);
        iv.push(0.5, 2.0);
        iv.push(3.0, 4.0);
        iv.push(3.5, 3.6);
        assert!((iv.covered() - 3.0).abs() < 1e-6);
        let holes = [(0.5, 1.0), (1.5, 5.0)];
        let mut iv = Intervals::new(&holes);
        iv.push(0.0, 2.0);
        assert!((iv.covered() - 1.0).abs() < 1e-6);
        // More pieces than fit: never more than the true union.
        let mut iv = Intervals::new(&[]);
        for i in 0..200 {
            iv.push(i as f32, i as f32 + 0.5);
        }
        let total = iv.covered();
        assert!(total <= 100.0 + 1e-3 && total > 90.0, "{total}");
    }
}
