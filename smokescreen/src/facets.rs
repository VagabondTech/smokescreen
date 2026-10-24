//! The faceted and crystal styles: hard-edged, tumbling polyhedral puffs
//! grown on the flood fill. Faceted puffs merge like metaballs; crystal puffs
//! don't merge at all (see the `chords` module).
//!
//! The fill decides where smoke goes and when it gets there. Instead of a
//! soft density, the cloud is a signed distance field (SDF) built from puffs:
//!
//! - one puff per `spacing`-sized cell of filled voxels, each an icosahedron,
//!   dodecahedron, octahedron or cube that tumbles about its own axis;
//! - merged with a smooth minimum (`blend`): 0 gives hard creases, a few
//!   tenths of a metre gives metaball-like necks while each puff keeps flat
//!   facets;
//! - clipped to the fill's halo region, so puffs bulge into open air but never
//!   poke through a wall;
//! - minus bullet tunnels (hexagonal prisms) and blast cavities (icosahedra).
//!
//! The cloud is the set where the SDF is negative. The amount of cloud along
//! a segment is the length of the segment inside that set.

use glam::{IVec3, Mat3, Vec3};

use crate::grid::Grid;
use crate::holes::HoleShape;
use crate::math::{rotation, safe_recip, slab};
use crate::timeline::Timeline;

// Golden ratio and friends, for the face normals below.
const A: f32 = 0.577_350_3; // 1/√3
const B: f32 = 0.525_731_1; // 1/√(1+φ²)
const C: f32 = 0.850_650_8; // φ/√(1+φ²)
const D: f32 = 0.356_822_1; // (1/φ)/√3
const E: f32 = 0.934_172_4; // φ/√3

const CUBE: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];
const OCTAHEDRON: [Vec3; 4] = [
    Vec3::new(A, A, A),
    Vec3::new(A, A, -A),
    Vec3::new(A, -A, A),
    Vec3::new(-A, A, A),
];
const DODECAHEDRON: [Vec3; 6] = [
    Vec3::new(0.0, B, C),
    Vec3::new(0.0, B, -C),
    Vec3::new(B, C, 0.0),
    Vec3::new(B, -C, 0.0),
    Vec3::new(C, 0.0, B),
    Vec3::new(-C, 0.0, B),
];
const ICOSAHEDRON: [Vec3; 10] = [
    Vec3::new(A, A, A),
    Vec3::new(A, A, -A),
    Vec3::new(A, -A, A),
    Vec3::new(-A, A, A),
    Vec3::new(0.0, D, E),
    Vec3::new(0.0, D, -E),
    Vec3::new(D, E, 0.0),
    Vec3::new(D, -E, 0.0),
    Vec3::new(E, 0.0, D),
    Vec3::new(-E, 0.0, D),
];

/// Puff shapes. All are centrally symmetric, so each needs one normal per
/// pair of opposite faces. The discriminants are the ids renderers use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Polyhedron {
    /// Six square faces.
    Cube = 0,
    /// Eight triangular faces.
    Octahedron = 1,
    /// Twelve pentagonal faces.
    Dodecahedron = 2,
    /// Twenty triangular faces.
    Icosahedron = 3,
}

impl Polyhedron {
    #[doc(hidden)]
    pub fn face_normals(self) -> &'static [Vec3] {
        match self {
            Self::Cube => &CUBE,
            Self::Octahedron => &OCTAHEDRON,
            Self::Dodecahedron => &DODECAHEDRON,
            Self::Icosahedron => &ICOSAHEDRON,
        }
    }

    /// Circumradius divided by inradius (how far corners stick out).
    #[doc(hidden)]
    pub fn corner_ratio(self) -> f32 {
        match self {
            Self::Cube | Self::Octahedron => 1.732_051,
            Self::Dodecahedron | Self::Icosahedron => 1.258_409,
        }
    }

    /// Signed distance (exact inside, a tight lower bound near edges and
    /// corners) from local point `q` to the polyhedron with inradius `r`.
    #[inline]
    pub(crate) fn sdf(self, q: Vec3, r: f32) -> f32 {
        let mut g = 0.0_f32;
        for n in self.face_normals() {
            g = g.max(n.dot(q).abs());
        }
        g - r
    }
}

/// Which polyhedra a cloud is built from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShapeMix {
    /// Every puff has the same shape.
    Single(Polyhedron),
    /// Icosahedra and dodecahedra with the occasional octahedron.
    Mixed,
}

/// Capacity per cloud. Part of the rules: a cloud that would need more puffs
/// is built from fewer, bigger ones. Renderers size their buffers from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FacetLimits {
    /// Puffs per cloud.
    pub max_puffs: usize,
    /// Cells of the grid that lists which puffs are near each point; the
    /// cells grow if a cloud would need more.
    pub max_cells: usize,
    /// Entries in those lists, every cell's added up.
    pub max_indices: usize,
}

impl Default for FacetLimits {
    fn default() -> Self {
        Self {
            max_puffs: 1024,
            max_cells: 16384,
            max_indices: 32768,
        }
    }
}

/// Shape and motion of faceted clouds. Lengths in the caller's units, times
/// in seconds, angles in radians.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FacetSettings {
    /// Distance between puff centres. Rounded to whole voxels.
    pub spacing: f32,
    /// Typical puff inradius.
    pub size: f32,
    /// Random size variation, ± fraction.
    pub size_jitter: f32,
    /// How much smaller puffs at the outside of the cloud are (0..1).
    pub taper: f32,
    /// Smooth-union radius. 0 = hard creases, ~0.3 m = metaballs.
    pub blend: f32,
    /// Which polyhedra the puffs are.
    pub shapes: ShapeMix,
    /// Typical tumbling speed, radians per second.
    pub spin: f32,
    /// Seconds a puff takes to pop in once the growth front reaches it.
    pub pop_time: f32,
    /// How far puffs may bulge past the filled voxels into open air. Keep it
    /// small: it is also how far smoke could reach around a door jamb.
    pub overhang: f32,
    /// How many puffs a cloud may have, and how big their lookup may grow.
    pub limits: FacetLimits,
}

/// What counts as cloud for the faceted style's rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FacetRule {
    /// The drawn surface, exactly. Costs about ten times a volumetric check.
    #[default]
    Exact,
    /// Only each puff's core: its inscribed sphere, kept clear of walls, minus
    /// a generous bound on every hole. About as cheap as a volumetric check.
    /// Everything it counts lies inside the drawn surface, so it never blocks
    /// a line the picture leaves clear; thin edges and corners count as
    /// see-through.
    Core,
    /// Between the two: each puff's hard polyhedron, kept clear of walls,
    /// minus a generous bound on every hole. Exact chords, with no tracing,
    /// at about the core rule's cost. Everything it counts lies inside the
    /// drawn surface, so it never blocks a line the picture leaves clear.
    /// Along a line through a grown cloud it counts about two thirds of what
    /// the exact rule counts (the core rule about three fifths); most of the
    /// rest is the blend between puffs.
    Chords,
}

impl FacetSettings {
    /// Crystal defaults, in metres: the faceted ones without the blend.
    pub fn crystal() -> Self {
        Self {
            blend: 0.0,
            ..Self::metric()
        }
    }

    /// Defaults, in metres.
    pub fn metric() -> Self {
        Self {
            spacing: 0.9,
            size: 0.72,
            size_jitter: 0.3,
            taper: 0.15,
            blend: 0.3,
            shapes: ShapeMix::Mixed,
            spin: 0.35,
            pop_time: 0.35,
            overhang: 0.45,
            limits: FacetLimits::default(),
        }
    }

    /// The same settings with every length passed through `f` (for unit
    /// conversions).
    pub(crate) fn map_lengths(&self, f: impl Fn(f32) -> f32) -> Self {
        Self {
            spacing: f(self.spacing),
            size: f(self.size),
            blend: f(self.blend),
            overhang: f(self.overhang),
            ..*self
        }
    }

    /// Furthest any part of a puff can reach from its centre.
    pub(crate) fn max_reach(&self) -> f32 {
        let corner = match self.shapes {
            ShapeMix::Single(p) => p.corner_ratio(),
            // Octahedra in a mix are shrunk to the same reach as the others.
            ShapeMix::Mixed => Polyhedron::Icosahedron.corner_ratio(),
        };
        self.size * (1.0 + self.size_jitter) * corner * POP_OVERSHOOT + self.blend
    }

    /// Halo, in metres, the flood fill keeps around the cloud, for settings in
    /// metres: the overhang plus a margin, so puffs in open air are never
    /// clipped by it.
    pub(crate) fn halo_metres(&self) -> f32 {
        self.overhang + 0.2
    }
}

/// Peak scale of the pop-in animation.
#[doc(hidden)]
pub const POP_OVERSHOOT: f32 = 1.12;

/// One puff, fixed for the cloud's lifetime. Internal frame (metres, Y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Puff {
    pub center: Vec3,
    /// Full-grown inradius.
    pub radius: f32,
    /// Distance from the centre to the edge of the space the cloud may fill
    /// (walls are outside it), less a margin. Crystal puffs fit inside it;
    /// the faceted core rule never counts past it.
    pub room: f32,
    /// When the growth front reaches it, 0 (centre) .. 1 (outermost).
    pub arrival: f32,
    pub shape: Polyhedron,
    /// Unit rotation axis.
    pub axis: Vec3,
    /// Radians per second (signed).
    pub spin: f32,
    pub phase: f32,
}

fn ease_out_back(x: f32) -> f32 {
    // Overshoots to about POP_OVERSHOOT before settling at 1.
    let c1 = 1.1;
    let c3 = c1 + 1.0;
    let t = x - 1.0;
    1.0 + c3 * t * t * t + c1 * t * t
}

impl Puff {
    /// Seconds after the pop when this puff starts to pop in: when the
    /// volumetric style's growth front reaches it.
    pub(crate) fn arrival_time(&self, timeline: &Timeline) -> f32 {
        let a = (self.arrival * crate::grid::FILLED_MAX / 1.2).clamp(0.0, 0.999);
        timeline.start + timeline.grow_time * (1.0 - crate::math::cbrt(1.0 - a))
    }

    /// Inradius `age` seconds after the pop, given [`Self::arrival_time`]:
    /// pops in, then pops out (outer puffs first) during the fade. 0 when
    /// absent.
    pub(crate) fn radius_at(&self, t_in: f32, age: f32, timeline: &Timeline, settings: &FacetSettings) -> f32 {
        let pop = settings.pop_time.max(1e-3);
        let x = (age - t_in) / pop;
        if x <= 0.0 {
            return 0.0;
        }
        let grow = if x >= 1.0 { 1.0 } else { ease_out_back(x) };
        let fade_start = timeline.lifetime - timeline.fade_time;
        let t_out = fade_start + (timeline.fade_time - pop) * (1.0 - self.arrival);
        let y = ((age - t_out) / pop).clamp(0.0, 1.0);
        let shrink = 1.0 - y * y;
        self.radius * grow * shrink
    }

    /// Orientation `age` seconds after the pop, as a world-to-local rotation.
    pub(crate) fn to_local_at(self, age: f32) -> Mat3 {
        rotation(self.axis, self.phase + self.spin * age).transpose()
    }

    /// The puff at one moment.
    pub(crate) fn live(&self, t_in: f32, age: f32, timeline: &Timeline, settings: &FacetSettings) -> LivePuff {
        let radius = self.radius_at(t_in, age, timeline, settings);
        LivePuff {
            center: self.center,
            radius,
            to_local: self.to_local_at(age),
            shape: self.shape,
            ratio: self.shape.corner_ratio(),
            core: radius.min(self.room),
        }
    }
}

/// A puff as it is at one tick: what the SDF evaluates.
#[derive(Clone, Copy, Debug, PartialEq)]
#[doc(hidden)]
pub struct LivePuff {
    pub center: Vec3,
    /// Inradius; 0 when absent. Crystal puffs also shrink out of the way of
    /// holes.
    pub radius: f32,
    /// World-to-local rotation.
    pub to_local: Mat3,
    pub shape: Polyhedron,
    /// [`Polyhedron::corner_ratio`] of `shape`.
    pub ratio: f32,
    /// Radius of the core the faceted core rule counts: the inscribed sphere,
    /// kept clear of walls.
    pub(crate) core: f32,
}

/// Uniform grid of puff lists, so an SDF evaluation only visits nearby puffs.
#[derive(Clone, Debug, PartialEq)]
pub struct Bins {
    pub min: Vec3,
    pub cell: f32,
    pub dims: IVec3,
    /// Per cell: (first index into `indices`, count).
    pub ranges: Vec<(u32, u32)>,
    pub indices: Vec<u32>,
}

impl Bins {
    /// Bin puffs by their maximum reach (so bins stay valid while puffs grow,
    /// spin and shrink). Lists are in puff order, which keeps the smooth
    /// union identical across cell boundaries.
    pub(crate) fn build(puffs: &[Puff], blend: f32, cell: f32) -> Self {
        let mut builder = BinBuilder::new(cell);
        while !builder.step(puffs, blend, usize::MAX) {}
        builder.finish()
    }

    pub(crate) fn max(&self) -> Vec3 {
        self.min + self.dims.as_vec3() * self.cell
    }

    #[inline]
    pub(crate) fn cell_of(&self, p: Vec3) -> Option<IVec3> {
        let g = (p - self.min) / self.cell;
        if g.cmplt(Vec3::ZERO).any() {
            return None;
        }
        // Non-negative, so truncation is floor.
        let c = g.as_ivec3();
        c.cmplt(self.dims).all().then_some(c)
    }

    #[inline]
    pub(crate) fn list(&self, c: IVec3) -> &[u32] {
        let (start, count) = self.ranges[(c.x + self.dims.x * (c.y + self.dims.y * c.z)) as usize];
        &self.indices[start as usize..(start + count) as usize]
    }
}

/// Builds [`Bins`] a few puffs at a time, so a fill can spread the work over
/// ticks. Gives exactly what [`Bins::build`] gives.
#[derive(Clone, Debug)]
pub(crate) struct BinBuilder {
    cell: f32,
    lo: Vec3,
    dims: IVec3,
    counts: Vec<u32>,
    ranges: Vec<(u32, u32)>,
    indices: Vec<u32>,
    /// 0: bounds, 1: count, 2: fill, 3: done.
    pass: u8,
    next: usize,
}

impl BinBuilder {
    pub fn new(cell: f32) -> Self {
        Self {
            cell,
            lo: Vec3::ZERO,
            dims: IVec3::ONE,
            counts: Vec::new(),
            ranges: Vec::new(),
            indices: Vec::new(),
            pass: 0,
            next: 0,
        }
    }

    fn reach(p: &Puff, blend: f32) -> f32 {
        p.radius * p.shape.corner_ratio() * POP_OVERSHOOT + blend
    }

    /// Every cell the puff's bounding sphere touches, in a fixed order.
    fn visit(&self, p: &Puff, blend: f32, mut f: impl FnMut(usize)) {
        let (lo, cell, dims) = (self.lo, self.cell, self.dims);
        let r = Self::reach(p, blend);
        let c0 = ((p.center - r - lo) / cell).floor().as_ivec3().max(IVec3::ZERO);
        let c1 = ((p.center + r - lo) / cell).floor().as_ivec3().min(dims - 1);
        for z in c0.z..=c1.z {
            for y in c0.y..=c1.y {
                for x in c0.x..=c1.x {
                    // Only cells the bounding sphere actually touches.
                    let cmin = lo + IVec3::new(x, y, z).as_vec3() * cell;
                    let closest = p.center.clamp(cmin, cmin + cell);
                    if closest.distance_squared(p.center) <= r * r {
                        f((x + dims.x * (y + dims.y * z)) as usize);
                    }
                }
            }
        }
    }

    /// Process up to `budget` puffs. Returns `true` once done.
    pub fn step(&mut self, puffs: &[Puff], blend: f32, budget: usize) -> bool {
        let mut left = budget;
        while left > 0 && self.pass < 3 {
            match self.pass {
                0 => {
                    let mut lo = Vec3::splat(f32::MAX);
                    let mut hi = Vec3::splat(f32::MIN);
                    for p in puffs {
                        lo = lo.min(p.center - Self::reach(p, blend));
                        hi = hi.max(p.center + Self::reach(p, blend));
                    }
                    if puffs.is_empty() {
                        lo = Vec3::ZERO;
                        hi = Vec3::ONE;
                    }
                    self.lo = lo;
                    self.dims = ((hi - lo) / self.cell).ceil().as_ivec3().max(IVec3::ONE);
                    self.counts = vec![0; (self.dims.x * self.dims.y * self.dims.z) as usize];
                    self.pass = 1;
                    self.next = 0;
                }
                1 => {
                    while left > 0 && self.next < puffs.len() {
                        let mut counts = std::mem::take(&mut self.counts);
                        self.visit(&puffs[self.next], blend, |c| counts[c] += 1);
                        self.counts = counts;
                        self.next += 1;
                        left -= 1;
                    }
                    if self.next == puffs.len() {
                        // Prefix sums; the counts become each list's cursor.
                        let mut start = 0u32;
                        self.ranges = Vec::with_capacity(self.counts.len());
                        for count in self.counts.iter_mut() {
                            self.ranges.push((start, *count));
                            let c = *count;
                            *count = start;
                            start += c;
                        }
                        self.indices = vec![0; start as usize];
                        self.pass = 2;
                        self.next = 0;
                    }
                }
                _ => {
                    while left > 0 && self.next < puffs.len() {
                        let i = self.next as u32;
                        let (mut cursor, mut indices) =
                            (std::mem::take(&mut self.counts), std::mem::take(&mut self.indices));
                        self.visit(&puffs[self.next], blend, |c| {
                            indices[cursor[c] as usize] = i;
                            cursor[c] += 1;
                        });
                        self.counts = cursor;
                        self.indices = indices;
                        self.next += 1;
                        left -= 1;
                    }
                    if self.next == puffs.len() {
                        self.pass = 3;
                    }
                }
            }
        }
        self.pass == 3
    }

    pub fn finish(self) -> Bins {
        Bins {
            min: self.lo,
            cell: self.cell,
            dims: self.dims,
            ranges: self.ranges,
            indices: self.indices,
        }
    }
}

/// Polynomial smooth minimum (exactly `min` when `a` and `b` differ by more
/// than `k`). 1-Lipschitz, so the union stays a valid distance bound.
#[inline]
pub(crate) fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

/// Hexagonal prism around the segment `a..b`, open at the ends.
pub(crate) fn tunnel_sdf(p: Vec3, a: Vec3, b: Vec3, radius: f32) -> f32 {
    let ab = b - a;
    let len = ab.length().max(1e-6);
    let u = ab / len;
    let q = p - a;
    let s = q.dot(u);
    let radial = q - u * s;
    let (e1, e2) = u.any_orthonormal_pair();
    let x = radial.dot(e1);
    let y = radial.dot(e2);
    let hex = x
        .abs()
        .max((0.5 * x + 0.866_025_4 * y).abs())
        .max((-0.5 * x + 0.866_025_4 * y).abs());
    (hex - radius).max((-s).max(s - len))
}

/// The hole's own SDF (negative inside the hole).
#[inline]
pub(crate) fn hole_sdf(p: Vec3, h: &HoleShape) -> f32 {
    let r = h.radius * (h.strength * 1.5).min(1.0);
    if h.a == h.b {
        // Explosion: an icosahedral cavity.
        Polyhedron::Icosahedron.sdf(p - h.a, r * 0.8)
    } else {
        tunnel_sdf(p, h.a, h.b, r)
    }
}

/// Value returned for points with no puffs nearby.
pub(crate) const FAR: f32 = 1e3;

/// Below this distance (metres) from the surface the integrator evaluates real
/// polyhedra; further away bounding spheres decide.
const NEAR_SURFACE: f32 = 0.2;
/// Inside, deeper than this the inscribed spheres decide.
const DEEP_INSIDE: f32 = 0.1;
/// Smallest step of the integrator (metres): a boundary crossing is located
/// to about this much.
pub(crate) const MIN_STEP: f32 = 5e-3;
/// Iterations after which a segment is given up on and counted as cloud.
const MAX_ITERATIONS: u32 = 4096;

/// A faceted cloud at one tick, ready for SDF queries. Internal frame.
#[derive(Clone, Copy, Debug)]
pub struct FacetField<'a> {
    pub(crate) grid: &'a Grid,
    pub(crate) bins: &'a Bins,
    pub(crate) live: &'a [LivePuff],
    pub(crate) blend: f32,
    pub(crate) holes: &'a [HoleShape],
}

impl FacetField<'_> {
    /// Signed distance to the cloud's surface (negative inside).
    pub fn sdf(&self, p: Vec3) -> f32 {
        let Some(c) = self.bins.cell_of(p) else {
            return FAR;
        };
        self.sdf_in_cell(p, self.bins.list(c))
    }

    fn sdf_in_cell(&self, p: Vec3, list: &[u32]) -> f32 {
        let mut d = FAR;
        for &i in list {
            let puff = &self.live[i as usize];
            if puff.radius <= 0.0 {
                continue;
            }
            // A puff whose bounding sphere is further than the blend radius
            // past the current surface can't change it.
            let to_center = p.distance(puff.center);
            if (to_center - puff.radius * puff.ratio) / puff.ratio >= d + self.blend {
                continue;
            }
            let q = puff.to_local * (p - puff.center);
            d = smin(d, puff.shape.sdf(q, puff.radius), self.blend);
        }
        if d >= FAR * 0.5 {
            return d;
        }
        // Never past the fill's halo: no poking through walls. The voxelised
        // boundary sits up to half a voxel in front of a wall, so it is
        // pushed out by that much; the wall itself hides the overlap.
        d = d.max(self.grid.sample_region(p) - self.grid.voxel_size * 0.5);
        for h in self.holes {
            if h.strength > 0.0 {
                d = d.max(-hole_sdf(p, h));
            }
        }
        d
    }

    /// One cheap pass over the listed puffs' spheres. Returns a lower bound
    /// on the distance from `p` to the cloud (from bounding spheres), and how
    /// far along `rd` the ray certainly stays inside some puff (from the
    /// inscribed spheres containing `p`; 0 if none does).
    #[inline]
    fn sphere_pass(&self, p: Vec3, rd: Vec3, list: &[u32]) -> (f32, f32) {
        let k = self.blend;
        // A chain of smooth minima lies at most `k` below the plain minimum,
        // so the cloud is inside the puffs grown by `k`.
        let mut value = FAR;
        let mut euclid = FAR;
        let mut hop = 0.0_f32;
        for &i in list {
            let puff = &self.live[i as usize];
            if puff.radius <= 0.0 {
                continue;
            }
            let q = p - puff.center;
            let rho2 = q.length_squared();
            let rho = rho2.sqrt();
            value = value.min((rho - puff.radius * puff.ratio) / puff.ratio);
            euclid = euclid.min(rho - puff.ratio * (puff.radius + k));
            let r2 = puff.radius * puff.radius;
            if rho2 < r2 {
                let b = q.dot(rd);
                hop = hop.max(-b + (b * b + (r2 - rho2)).sqrt());
            }
        }
        ((value - k).max(euclid), hop)
    }

    /// Radius of a ball around `p` that is inside the wall clip and outside
    /// every hole (negative if `p` itself isn't).
    #[inline]
    fn clip_radius(&self, p: Vec3) -> f32 {
        let mut r = self.grid.voxel_size * 0.5 - self.grid.sample_region(p);
        for h in self.holes {
            if h.strength > 0.0 {
                r = r.min(hole_sdf(p, h));
            }
        }
        r
    }

    /// How far along `rd` the ray stays inside a puff's polyhedron that
    /// contains `p` (the longest such exit; 0 if none contains it).
    #[inline]
    fn polyhedron_hop(&self, p: Vec3, rd: Vec3, list: &[u32]) -> f32 {
        let mut hop = 0.0_f32;
        for &i in list {
            let puff = &self.live[i as usize];
            if puff.radius <= 0.0 {
                continue;
            }
            let rel = p - puff.center;
            let reach = puff.radius * puff.ratio;
            if rel.length_squared() >= reach * reach {
                continue;
            }
            let q = puff.to_local * rel;
            let u = puff.to_local * rd;
            let mut exit = FAR;
            let mut inside = true;
            for n in puff.shape.face_normals() {
                let nq = n.dot(q);
                if nq.abs() >= puff.radius {
                    inside = false;
                    break;
                }
                let nu = n.dot(u);
                if nu.abs() > 1e-9 {
                    // Leaves the slab |n·x| ≤ r through the face it heads for.
                    exit = exit.min((puff.radius - nq * nu.signum()) / nu.abs());
                }
            }
            if inside {
                hop = hop.max(exit);
            }
        }
        hop
    }

    /// Add the length of the ray `ro + rd·t`, `t0 ≤ t ≤ t1`, that lies inside
    /// the cloud to `acc`, one interval at a time. Stops (and returns `true`)
    /// as soon as `stop(*acc)` holds. `rd` must be a unit vector.
    ///
    /// Each step is certified by the SDF (it is 1-Lipschitz), so no part of
    /// the cloud is skipped; boundaries are located to about [`MIN_STEP`].
    pub(crate) fn amount(
        &self,
        ro: Vec3,
        rd: Vec3,
        t0: f32,
        t1: f32,
        acc: &mut f32,
        stop: &dyn Fn(f32) -> bool,
    ) -> bool {
        let inv = safe_recip(rd);
        let (b0, b1) = slab(ro, inv, self.bins.min, self.bins.max());
        let mut t = t0.max(b0);
        let end = t1.min(b1);
        let cell = self.bins.cell;
        let mut iterations = 0;
        while t < end {
            iterations += 1;
            if iterations > MAX_ITERATIONS {
                // Pathological grazing: fail closed, like a renderer that runs
                // out of steps and draws smoke.
                *acc += end - t;
                return stop(*acc);
            }
            let p = ro + rd * t;
            let c = ((p - self.bins.min) / cell)
                .max(Vec3::ZERO)
                .as_ivec3()
                .min(self.bins.dims - 1);
            let cmin = self.bins.min + c.as_vec3() * cell;
            let (_, cell_exit) = slab(ro, inv, cmin, cmin + cell);
            // Just past the cell, so the next step lands in the next one.
            let to_exit = (cell_exit - t).max(0.0) + 1e-4;
            let list = self.bins.list(c);
            if list.is_empty() {
                t += to_exit;
                continue;
            }
            let (outer, hop) = self.sphere_pass(p, rd, list);
            // Far outside: bounding spheres only.
            if outer > NEAR_SURFACE {
                t += outer.min(to_exit);
                continue;
            }
            // Inside a puff's inscribed sphere: on to where the ray leaves it
            // (or the wall clip, or a hole), without touching polyhedra.
            if hop > DEEP_INSIDE {
                let step = hop.min(self.clip_radius(p));
                if step > DEEP_INSIDE {
                    let step = step.min(end - t);
                    *acc += step;
                    if stop(*acc) {
                        return true;
                    }
                    t += step;
                    continue;
                }
            }
            let d = self.sdf_in_cell(p, list);
            if d < 0.0 {
                // Inside: the whole ball of radius -d is cloud, whichever
                // cells it spans; so is the rest of any polyhedron the ray is
                // in, as far as the wall clip and holes allow.
                let mut step = -d;
                let poly = self.polyhedron_hop(p, rd, list);
                if poly > step {
                    step = poly.min(self.clip_radius(p)).max(step);
                }
                let step = step.max(MIN_STEP).min(end - t);
                *acc += step;
                if stop(*acc) {
                    return true;
                }
                t += step;
            } else {
                // Outside: puffs not listed in this cell may lie beyond it.
                t += d.max(MIN_STEP).min(to_exit);
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polyhedra_are_exact_on_faces() {
        for shape in [
            Polyhedron::Cube,
            Polyhedron::Octahedron,
            Polyhedron::Dodecahedron,
            Polyhedron::Icosahedron,
        ] {
            let n = shape.face_normals()[0];
            assert!((n.length() - 1.0).abs() < 1e-5, "{shape:?} normal not unit");
            assert!((shape.sdf(n * 2.0, 1.0) - 1.0).abs() < 1e-5);
            assert!((shape.sdf(Vec3::ZERO, 1.0) + 1.0).abs() < 1e-6);
            // A corner is the furthest point: corner ratio × inradius.
            let r = shape.corner_ratio();
            let mut max_corner = 0.0_f32;
            for i in 0..2000 {
                let a = i as f32 * 2.399_963;
                let z = 1.0 - 2.0 * (i as f32 + 0.5) / 2000.0;
                let (s, c) = crate::math::sin_cos(a);
                let dir = Vec3::new((1.0 - z * z).sqrt() * c, z, (1.0 - z * z).sqrt() * s);
                let g = shape.sdf(dir, 0.0);
                max_corner = max_corner.max(1.0 / g);
            }
            assert!((max_corner - r).abs() < 0.03, "{shape:?}: {max_corner} vs {r}");
        }
    }

    #[test]
    fn smin_is_one_lipschitz() {
        for (a, b) in [(0.0, 0.1), (0.3, -0.2), (1.0, 1.05)] {
            let k = 0.3;
            let e = 1e-3;
            let da = (smin(a + e, b, k) - smin(a, b, k)) / e;
            let db = (smin(a, b + e, k) - smin(a, b, k)) / e;
            assert!(da >= -1e-3 && db >= -1e-3 && da + db <= 1.0 + 1e-2, "{da} {db}");
        }
    }
}
