//! Level geometry, seen through two questions.
//!
//! The simulation never touches meshes or a physics engine. It asks a
//! [`Level`] whether a box is solid (the flood fill) and whether a ray hits
//! anything (only for the optional lighting in [`crate::light`]). Both are
//! asked in the caller's units and frame (see [`crate::Units`]), so a level
//! is a thin wrapper over the game's own collision code.
//!
//! For shipping games, [`VoxelObstacles`] voxelises the static level once at
//! load time; every fill after that is bit lookups. [`BoxObstacles`] is fine
//! for blockouts and tests.

use std::sync::Arc;

use glam::{IVec3, UVec3, Vec3};

use crate::units::UpAxis;

/// Level geometry the smoke respects. Positions and lengths are in the
/// caller's units and frame.
///
/// For the simulation to be identical on every machine, the answers must be
/// too: implement it over the same deterministic collision data the game
/// simulation uses.
pub trait Level {
    /// Is the axis-aligned cube centred at `center` with half-size
    /// `half_extent` blocked by level geometry?
    fn is_blocked(&self, center: Vec3, half_extent: f32) -> bool;

    /// Is the point `point` inside level geometry? Asked about voxel centres
    /// next to volumetric smoke. Of blocked voxels, to find the ones a wall
    /// only clips (their centre is still open air): smoke fills those up to
    /// the wall. And of a few open voxels beside those that the smoke can't
    /// reach, which are past a wall unless they are the hollow inside of one
    /// (a level made of surfaces, like [`VoxelObstacles::from_triangles`],
    /// can say so); smoke never fills a clipped voxel next to open air past a
    /// wall.
    ///
    /// The default asks `is_blocked(point, 0.0)`; override it if a box of
    /// size zero isn't a point test in your collision code. Answering
    /// `is_blocked(point, half a voxel)` is always safe: smoke then stops at
    /// the middle of the first blocked voxel.
    fn is_inside(&self, point: Vec3) -> bool {
        self.is_blocked(point, 0.0)
    }

    /// Does a ray from `origin` along the unit vector `dir` hit level geometry
    /// within `max_dist`? Only used by [`crate::light`], which renderers use
    /// to shade smoke indoors; the simulation itself never casts rays.
    fn ray_hit(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> bool;
}

impl<T: Level + ?Sized> Level for &T {
    fn is_blocked(&self, center: Vec3, half_extent: f32) -> bool {
        (**self).is_blocked(center, half_extent)
    }
    fn is_inside(&self, point: Vec3) -> bool {
        (**self).is_inside(point)
    }
    fn ray_hit(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> bool {
        (**self).ray_hit(origin, dir, max_dist)
    }
}

impl<T: Level + ?Sized> Level for Box<T> {
    fn is_blocked(&self, center: Vec3, half_extent: f32) -> bool {
        (**self).is_blocked(center, half_extent)
    }
    fn is_inside(&self, point: Vec3) -> bool {
        (**self).is_inside(point)
    }
    fn ray_hit(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> bool {
        (**self).ray_hit(origin, dir, max_dist)
    }
}

impl<T: Level + ?Sized> Level for Arc<T> {
    fn is_blocked(&self, center: Vec3, half_extent: f32) -> bool {
        (**self).is_blocked(center, half_extent)
    }
    fn is_inside(&self, point: Vec3) -> bool {
        (**self).is_inside(point)
    }
    fn ray_hit(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> bool {
        (**self).ray_hit(origin, dir, max_dist)
    }
}

#[inline]
fn up_component(p: Vec3, up: UpAxis) -> f32 {
    match up {
        UpAxis::Y => p.y,
        UpAxis::Z => p.z,
    }
}

/// An infinite ground plane: everything below `height` along the up axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlatGround {
    /// The plane's height along the up axis, in your units.
    pub height: f32,
    /// Which axis is up.
    pub up: UpAxis,
}

impl Default for FlatGround {
    fn default() -> Self {
        Self {
            height: 0.0,
            up: UpAxis::Y,
        }
    }
}

impl FlatGround {
    /// Ground at `height` along the `up` axis.
    pub fn new(height: f32, up: UpAxis) -> Self {
        Self { height, up }
    }

    /// Distance along a ray to the plane, if it hits within `max_dist`.
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<f32> {
        let d = up_component(dir, self.up);
        if d >= 0.0 {
            return None;
        }
        let t = (self.height - up_component(origin, self.up)) / d;
        (0.0..=max_dist).contains(&t).then_some(t)
    }
}

impl Level for FlatGround {
    fn is_blocked(&self, center: Vec3, half_extent: f32) -> bool {
        // A voxel counts as solid once most of it is below the floor.
        up_component(center, self.up) < self.height + half_extent * 0.25
    }

    fn ray_hit(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> bool {
        if up_component(dir, self.up) >= 0.0 {
            return up_component(origin, self.up) < self.height;
        }
        self.raycast(origin, dir, max_dist).is_some()
    }
}

/// An axis-aligned box, stored as min/max corners.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObstacleBox {
    /// The corner with the smallest coordinates, in your units.
    pub min: Vec3,
    /// The corner with the largest coordinates.
    pub max: Vec3,
}

impl ObstacleBox {
    /// The box of size `size` centred on `center`.
    pub fn from_center_size(center: Vec3, size: Vec3) -> Self {
        Self {
            min: center - size * 0.5,
            max: center + size * 0.5,
        }
    }

    fn overlaps_cube(&self, center: Vec3, half: f32) -> bool {
        // Shrink the voxel a little so a voxel that only grazes a wall face
        // stays open; otherwise doorways lose a voxel on each side.
        let h = half * 0.8;
        (center + h).cmpgt(self.min).all() && (center - h).cmplt(self.max).all()
    }

    /// Slab test. Returns the entry distance if the ray hits within `max_dist`.
    pub fn ray_entry(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<f32> {
        let inv = dir.recip();
        let t0 = (self.min - origin) * inv;
        let t1 = (self.max - origin) * inv;
        let near = t0.min(t1).max_element();
        let far = t0.max(t1).min_element();
        if far >= near.max(0.0) && near <= max_dist {
            Some(near.max(0.0))
        } else {
            None
        }
    }
}

/// A list of boxes plus an optional ground plane. Fine for prototypes and
/// blockout levels with up to a few hundred boxes.
#[derive(Clone, Debug, Default)]
pub struct BoxObstacles {
    /// The boxes, all solid.
    pub boxes: Vec<ObstacleBox>,
    /// A ground plane under them, if any.
    pub ground: Option<FlatGround>,
}

impl BoxObstacles {
    /// Boxes on a ground plane at height 0, Y up.
    pub fn new(boxes: Vec<ObstacleBox>) -> Self {
        Self {
            boxes,
            ground: Some(FlatGround::default()),
        }
    }

    /// Closest hit distance along a ray, if any.
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<f32> {
        let mut best: Option<f32> = None;
        for b in &self.boxes {
            if let Some(t) = b.ray_entry(origin, dir, max_dist) {
                best = Some(best.map_or(t, |bt: f32| bt.min(t)));
            }
        }
        if let Some(t) = self.ground.and_then(|g| g.raycast(origin, dir, max_dist)) {
            best = Some(best.map_or(t, |bt: f32| bt.min(t)));
        }
        best
    }
}

impl Level for BoxObstacles {
    fn is_blocked(&self, center: Vec3, half_extent: f32) -> bool {
        if let Some(g) = self.ground
            && g.is_blocked(center, half_extent)
        {
            return true;
        }
        self.boxes.iter().any(|b| b.overlaps_cube(center, half_extent))
    }

    fn ray_hit(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> bool {
        self.raycast(origin, dir, max_dist).is_some()
    }
}

/// A pre-voxelised level stored as bits per voxel.
///
/// Build it once when the level loads; every flood fill afterwards is pure
/// bit lookups. A 256 × 32 × 256 m level at 0.3 m voxels takes about 10 MB,
/// plus as much again for each of two sets of voxels next to walls if it
/// has any (which solid voxels a wall only clips, and which open ones are
/// the hollow inside of a wall: see [`Level::is_inside`]). It holds at most
/// 2^32 - 1 voxels (512 MB of bits): building a bigger one panics, as does
/// a voxel size that isn't above 0.
///
/// Build it with the tuning's voxel size ([`crate::Tuning::voxel_size`], in
/// your units) and fills give exactly the clouds the source level would:
/// every cloud's grid lies on the same world lattice (multiples of the voxel
/// size from the origin), so each cloud voxel is one cell here, and that
/// cell holds the source level's answers to the very questions the fill asks
/// (whether the voxel is blocked, and whether its centre is inside). Both
/// work out a voxel's centre from its index on the lattice the same way, to
/// the last bit, so the answers agree even for a wall lying exactly on a
/// voxel's centre or edge. A different voxel size only approximates the
/// source: both questions here look up the cell holding the point.
#[derive(Clone, Debug)]
pub struct VoxelObstacles {
    origin: Vec3,
    voxel_size: f32,
    dims: UVec3,
    /// Blocked voxels.
    bits: Vec<u64>,
    /// Blocked voxels whose centre is open air: a wall only clips them.
    /// Empty if there are none.
    clipped: Vec<u64>,
    /// Open voxels whose centre is inside: the hollow inside of a wall made
    /// of surfaces. Empty if there are none.
    hollow: Vec<u64>,
    /// What to report for queries outside the voxelised region.
    pub outside_is_solid: bool,
}

/// Most voxels a [`VoxelObstacles`] holds: its lattice indexes them in
/// `u32`. That many take 512 MB per set of bits.
const MAX_VOXELS: u64 = u32::MAX as u64;

/// How many voxels a lattice of `dims` holds, if no more than
/// [`MAX_VOXELS`].
fn voxel_count(dims: UVec3) -> Option<usize> {
    let n = (dims.x as u64).checked_mul(dims.y as u64)?.checked_mul(dims.z as u64)?;
    (n <= MAX_VOXELS).then_some(n as usize)
}

/// Why [`VoxelObstacles::from_bytes`] couldn't read its bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum VoxelBytesError {
    /// They don't start the way [`VoxelObstacles::to_bytes`] starts: not
    /// voxels at all.
    NotVoxels,
    /// Voxels in a format this version can't read, most likely written by a
    /// newer version of this crate: the format's number (this version
    /// reads 1 and 2).
    UnsupportedVersion(u32),
    /// They end before the voxels do: cut short in a download or a write.
    Truncated,
    /// Their header makes no sense (no voxels, more than 2^32 - 1, a voxel
    /// size that isn't above 0, a corner that isn't finite), or more bytes
    /// follow the voxels.
    Invalid,
}

impl core::fmt::Display for VoxelBytesError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotVoxels => f.write_str("not smokescreen voxels"),
            Self::UnsupportedVersion(v) => {
                write!(
                    f,
                    "smokescreen voxels in format {v}, which this version can't read (it reads 1 and 2)"
                )
            }
            Self::Truncated => f.write_str("smokescreen voxels cut short"),
            Self::Invalid => f.write_str("smokescreen voxels whose header makes no sense, or with bytes after them"),
        }
    }
}

impl std::error::Error for VoxelBytesError {}

/// Bits over the voxels of a [`Lattice`].
fn set_bit(bits: &mut [u64], i: usize) {
    bits[i / 64] |= 1 << (i % 64);
}

fn bit(bits: &[u64], i: usize) -> bool {
    bits.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0)
}

/// The set bits, in order.
fn ones(bits: &[u64]) -> impl Iterator<Item = usize> + '_ {
    bits.iter()
        .enumerate()
        .flat_map(|(k, &w)| (0..64).filter(move |b| w & (1 << b) != 0).map(move |b| k * 64 + b))
}

/// No set bits: nothing to keep.
fn kept(bits: Vec<u64>) -> Vec<u64> {
    if bits.iter().any(|&w| w != 0) { bits } else { Vec::new() }
}

/// The voxels of a [`VoxelObstacles`], x fastest: those of the world
/// lattice from index `first`.
#[derive(Clone, Copy, Debug)]
struct Lattice {
    first: IVec3,
    voxel_size: f32,
    dims: UVec3,
}

impl Lattice {
    fn count(&self) -> usize {
        self.dims.x as usize * self.dims.y as usize * self.dims.z as usize
    }

    fn cell(&self, i: usize) -> IVec3 {
        let (x, y) = (self.dims.x as usize, self.dims.y as usize);
        IVec3::new((i % x) as i32, ((i / x) % y) as i32, (i / (x * y)) as i32)
    }

    fn index(&self, c: IVec3) -> Option<usize> {
        if c.cmplt(IVec3::ZERO).any() || c.cmpge(self.dims.as_ivec3()).any() {
            return None;
        }
        Some((c.x as u32 + self.dims.x * (c.y as u32 + self.dims.y * c.z as u32)) as usize)
    }

    /// A voxel's centre, computed from its lattice index exactly as a fill
    /// computes the points it asks the level about.
    fn centre(&self, i: usize) -> Vec3 {
        ((self.first + self.cell(i)).as_vec3() + 0.5) * self.voxel_size
    }

    /// Where voxel (0, 0, 0) starts.
    fn origin(&self) -> Vec3 {
        self.first.as_vec3() * self.voxel_size
    }

    /// The lattice of the voxels holding `[min, max]`.
    fn around(min: Vec3, max: Vec3, voxel_size: f32) -> Self {
        assert!(
            voxel_size.is_finite() && voxel_size > 0.0,
            "VoxelObstacles needs a voxel size above 0, not {voxel_size}"
        );
        assert!(
            min.is_finite() && max.is_finite(),
            "VoxelObstacles needs a finite region, not {min} to {max}"
        );
        let first = (min / voxel_size).floor();
        let dims = ((max - first * voxel_size) / voxel_size)
            .ceil()
            .max(Vec3::ONE)
            .as_uvec3();
        assert!(
            voxel_count(dims).is_some(),
            "VoxelObstacles of {dims} voxels of {voxel_size} is too large: it holds at most 2^32 - 1 voxels (512 MB of bits). Voxelise a smaller region, or use larger voxels."
        );
        Self {
            first: first.as_ivec3(),
            voxel_size,
            dims,
        }
    }

    /// The voxels of the 3×3×3 block around `i` (`i` too) in the region.
    fn block(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        let c = self.cell(i);
        (0..27).filter_map(move |n| self.index(c + IVec3::new(n % 3, (n / 3) % 3, n / 9) - 1))
    }
}

/// Winding numbers of triangles at voxel centres, counted down the up axis
/// from the top: +1 for each surface above a centre that faces up, -1 for
/// each that faces down. 1 inside a closed mesh facing outwards, 2 where
/// two overlap, 0 outside it, by a vertical sheet or a sheet facing both
/// ways, and negative inside a mesh facing inwards; 1 under a single sheet
/// facing up (terrain, a ground plane), which is what lies under the ground.
struct Windings {
    lattice: Lattice,
    /// The up axis (1 or 2), and the two across it, in cyclic order (so a
    /// triangle's winding seen from above is the sign of its normal's up
    /// component).
    axes: [usize; 3],
    /// Per row of voxel centres along the up axis: row `r`'s crossings are
    /// `crossings[start[r]..start[r + 1]]`, as (where up the row, which way
    /// the surface faces), in the order found.
    start: Vec<u32>,
    crossings: Vec<(f32, i8)>,
}

impl Windings {
    fn new(triangles: &[[Vec3; 3]], lattice: Lattice, up: UpAxis) -> Self {
        let (origin, v, dims) = (lattice.origin(), lattice.voxel_size, lattice.dims);
        let axes = match up {
            UpAxis::Y => [1, 2, 0],
            UpAxis::Z => [2, 0, 1],
        };
        let [axis, u, w] = axes;
        let mut found: Vec<(u32, f32, i8)> = Vec::new();
        for t in triangles {
            let normal = (t[1] - t[0]).cross(t[2] - t[0]);
            if normal[axis] == 0.0 {
                continue;
            }
            let flat = |p: Vec3| [p[u], p[w]];
            let (a, b, c) = (flat(t[0]), flat(t[1]), flat(t[2]));
            // Seen from above: counter-clockwise if it faces up.
            let facing: i8 = if normal[axis] > 0.0 { 1 } else { -1 };
            let ccw = if facing > 0 { [a, b, c] } else { [a, c, b] };
            let range = |k: usize| {
                let lo = a[k].min(b[k]).min(c[k]);
                let hi = a[k].max(b[k]).max(c[k]);
                let axis = [u, w][k];
                let first = ((lo - origin[axis]) / v - 0.5).floor().max(0.0) as u32;
                let last = (((hi - origin[axis]) / v - 0.5).ceil().max(0.0) as u32).min(dims[axis] - 1);
                first..=last
            };
            for jw in range(1) {
                for ju in range(0) {
                    let q = [origin[u] + (ju as f32 + 0.5) * v, origin[w] + (jw as f32 + 0.5) * v];
                    if !covers(&ccw, q) {
                        continue;
                    }
                    // Where the row meets the triangle's plane.
                    let along =
                        t[0][axis] - (normal[u] * (q[0] - t[0][u]) + normal[w] * (q[1] - t[0][w])) / normal[axis];
                    found.push((ju + dims[u] * jw, along, facing));
                }
            }
        }
        // Bucket by row, keeping the order they were found in.
        let count = (dims[u] * dims[w]) as usize;
        let mut start = vec![0u32; count + 1];
        for &(row, ..) in &found {
            start[row as usize + 1] += 1;
        }
        for r in 0..count {
            start[r + 1] += start[r];
        }
        let mut next = start.clone();
        let mut crossings = vec![(0.0, 0); found.len()];
        for (row, along, facing) in found {
            crossings[next[row as usize] as usize] = (along, facing);
            next[row as usize] += 1;
        }
        Self {
            lattice,
            axes,
            start,
            crossings,
        }
    }

    /// The winding number at voxel `i`'s centre.
    fn at(&self, i: usize) -> i32 {
        let (c, p) = (self.lattice.cell(i), self.lattice.centre(i));
        let [axis, u, w] = self.axes;
        let row = (c[u] as u32 + self.lattice.dims[u] * c[w] as u32) as usize;
        let here = &self.crossings[self.start[row] as usize..self.start[row + 1] as usize];
        here.iter()
            .filter(|&&(along, _)| along > p[axis])
            .map(|&(_, facing)| facing as i32)
            .sum()
    }
}

/// Is the point `q` inside the counter-clockwise triangle `t`? A point on
/// an edge belongs to just one of the two triangles sharing it (each edge
/// is evaluated from its endpoints in a fixed order, so both get exactly
/// opposite values), so a row through a mesh's edge crosses it once.
fn covers(t: &[[f32; 2]; 3], q: [f32; 2]) -> bool {
    (0..3).all(|k| {
        let (e0, e1) = (t[k], t[(k + 1) % 3]);
        let flip = (e1[0], e1[1]) < (e0[0], e0[1]);
        let (s, e) = if flip { (e1, e0) } else { (e0, e1) };
        let f = (e[0] - s[0]) * (q[1] - s[1]) - (e[1] - s[1]) * (q[0] - s[0]);
        let f = if flip { -f } else { f };
        let d = [e1[0] - e0[0], e1[1] - e0[1]];
        f > 0.0 || (f == 0.0 && (d[1] < 0.0 || (d[1] == 0.0 && d[0] > 0.0)))
    })
}

impl VoxelObstacles {
    /// Voxelise the region `[min, max]` by calling `solid(center, half_extent)`
    /// for every voxel. Plug your collision's box-overlap query in here.
    ///
    /// It is also called with a `half_extent` of 0, as a point test (see
    /// [`Level::is_inside`]): for the centre of every solid voxel, and of
    /// the open voxels around those whose centre is open. If your query
    /// can't test a point, answer those as you did for the whole voxel.
    pub fn build(min: Vec3, max: Vec3, voxel_size: f32, mut solid: impl FnMut(Vec3, f32) -> bool) -> Self {
        Self::voxelise(min, max, voxel_size, |c, half| solid(c, half.unwrap_or(0.0)))
    }

    /// Voxelise any other [`Level`] (for example a [`BoxObstacles`]), with
    /// its [`Level::is_blocked`] and [`Level::is_inside`]. With the tuning's
    /// voxel size the clouds come out identical to the source's (see the
    /// type's docs).
    pub fn from_level(min: Vec3, max: Vec3, voxel_size: f32, source: &dyn Level) -> Self {
        Self::voxelise(min, max, voxel_size, |c, half| match half {
            Some(half) => source.is_blocked(c, half),
            None => source.is_inside(c),
        })
    }

    /// `ask(center, Some(half_extent))`: is the voxel blocked?
    /// `ask(center, None)`: is its centre inside? Asked of every blocked
    /// voxel, and of the open voxels around those whose centre isn't, which
    /// are all the fill ever asks about.
    fn voxelise(min: Vec3, max: Vec3, voxel_size: f32, mut ask: impl FnMut(Vec3, Option<f32>) -> bool) -> Self {
        let lattice = Lattice::around(min, max, voxel_size);
        let words = lattice.count().div_ceil(64);
        let (mut bits, mut clipped) = (vec![0u64; words], vec![0u64; words]);
        let half = voxel_size * 0.5;
        for i in 0..lattice.count() {
            let c = lattice.centre(i);
            if ask(c, Some(half)) {
                set_bit(&mut bits, i);
                if !ask(c, None) {
                    set_bit(&mut clipped, i);
                }
            }
        }
        let (mut hollow, mut asked) = (vec![0u64; words], vec![0u64; words]);
        for i in ones(&clipped) {
            for n in lattice.block(i) {
                if !bit(&bits, n) && !bit(&asked, n) {
                    set_bit(&mut asked, n);
                    if ask(lattice.centre(n), None) {
                        set_bit(&mut hollow, n);
                    }
                }
            }
        }
        Self::new(lattice, bits, clipped, hollow)
    }

    fn new(lattice: Lattice, bits: Vec<u64>, clipped: Vec<u64>, hollow: Vec<u64>) -> Self {
        Self {
            origin: lattice.origin(),
            voxel_size: lattice.voxel_size,
            dims: lattice.dims,
            bits,
            clipped: kept(clipped),
            hollow: kept(hollow),
            outside_is_solid: false,
        }
    }

    /// Voxelise triangles (a level's render or collision mesh, in the
    /// caller's units): every voxel a triangle passes through is solid. The
    /// region covers the triangles plus `margin` all round.
    ///
    /// Each voxel is tested a hundredth of a voxel up each axis from where it
    /// is, so a floor lying exactly on a voxel boundary blocks the voxel
    /// below it and leaves the one above open, instead of lifting the smoke a
    /// voxel off the floor. Uses only exact arithmetic, so every machine gets
    /// the same voxels from the same triangles.
    ///
    /// Only surfaces are voxelised (a closed mesh stays hollow inside), so
    /// which side of a surface is solid comes from the way its triangles
    /// face, counted down the `up` axis ([`Level::is_inside`]): inside
    /// closed meshes facing outwards (counter-clockwise seen from outside,
    /// as Bevy and most engines draw them), and under a single sheet facing
    /// up, such as terrain or a ground plane. Smoke fills right up to those
    /// surfaces wherever they sit on the voxel grid. A vertical sheet with
    /// nothing behind it (a partition made of a plane) has no inside, and
    /// neither has a mesh facing inwards: smoke stops up to half a voxel
    /// short of them, as against any wall thinner than a voxel.
    pub fn from_triangles(triangles: &[[Vec3; 3]], voxel_size: f32, margin: f32, up: UpAxis) -> Self {
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for t in triangles {
            for v in t {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
        if triangles.is_empty() {
            lo = Vec3::ZERO;
            hi = Vec3::ZERO;
        }
        let v = voxel_size;
        let lattice = Lattice::around(lo - margin, hi + margin, v);
        let (origin, dims) = (lattice.origin(), lattice.dims);
        let words = lattice.count().div_ceil(64);
        let mut bits = vec![0u64; words];
        let shift = Vec3::splat(v * 0.01);
        let half = Vec3::splat(v * 0.5);
        let max_cell = dims.as_ivec3() - 1;
        for t in triangles {
            let tmin = t[0].min(t[1]).min(t[2]);
            let tmax = t[0].max(t[1]).max(t[2]);
            let c0 = (((tmin - origin - shift) / v).floor().as_ivec3() - 1).clamp(IVec3::ZERO, max_cell);
            let c1 = ((tmax - origin - shift) / v)
                .floor()
                .as_ivec3()
                .clamp(IVec3::ZERO, max_cell);
            for z in c0.z..=c1.z {
                for y in c0.y..=c1.y {
                    for x in c0.x..=c1.x {
                        let i = (x as u32 + dims.x * (y as u32 + dims.y * z as u32)) as usize;
                        if triangle_overlaps_box(t, lattice.centre(i) + shift, half) {
                            set_bit(&mut bits, i);
                        }
                    }
                }
            }
        }
        // Which voxels the surfaces only clip (their centre is outside),
        // and which open voxels around those are the hollow inside of one.
        let windings = Windings::new(triangles, lattice, up);
        let (mut clipped, mut hollow, mut asked) = (vec![0u64; words], vec![0u64; words], vec![0u64; words]);
        for i in ones(&bits) {
            if windings.at(i) < 1 {
                set_bit(&mut clipped, i);
            }
        }
        for i in ones(&clipped) {
            for n in lattice.block(i) {
                if !bit(&bits, n) && !bit(&asked, n) {
                    set_bit(&mut asked, n);
                    if windings.at(n) >= 1 {
                        set_bit(&mut hollow, n);
                    }
                }
            }
        }
        Self::new(lattice, bits, clipped, hollow)
    }

    /// The voxels as bytes, to save or send (a server can voxelise once and
    /// ship the result, so every machine's level is the same).
    pub fn to_bytes(&self) -> Vec<u8> {
        let words = self.bits.len() + self.clipped.len() + self.hollow.len();
        let mut out = Vec::with_capacity(38 + words * 8);
        out.extend_from_slice(b"SMKV");
        out.extend_from_slice(&2u32.to_le_bytes());
        for f in [self.origin.x, self.origin.y, self.origin.z, self.voxel_size] {
            out.extend_from_slice(&f.to_le_bytes());
        }
        for d in [self.dims.x, self.dims.y, self.dims.z] {
            out.extend_from_slice(&d.to_le_bytes());
        }
        out.push(self.outside_is_solid as u8);
        out.push(!self.clipped.is_empty() as u8 | (!self.hollow.is_empty() as u8) << 1);
        for w in self.bits.iter().chain(&self.clipped).chain(&self.hollow) {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out
    }

    /// Read what [`Self::to_bytes`] wrote. Also reads the first version's
    /// bytes, which don't say which voxels a wall only clips: with those,
    /// smoke stops at the middle of the first solid voxel, as it did then.
    ///
    /// The bytes may come from anywhere (another machine, a file on disk):
    /// whatever they hold, the answer is the voxels or an error, never a
    /// panic.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, VoxelBytesError> {
        use VoxelBytesError::{Invalid, NotVoxels, Truncated, UnsupportedVersion};
        if bytes.get(..4) != Some(b"SMKV".as_slice()) {
            return Err(NotVoxels);
        }
        let word = |at: usize| -> Result<[u8; 4], VoxelBytesError> {
            bytes.get(at..at + 4).and_then(|w| w.try_into().ok()).ok_or(Truncated)
        };
        let version = u32::from_le_bytes(word(4)?);
        if !(1..=2).contains(&version) {
            return Err(UnsupportedVersion(version));
        }
        let f = |i: usize| word(8 + i * 4).map(f32::from_le_bytes);
        let u = |i: usize| word(24 + i * 4).map(u32::from_le_bytes);
        let origin = Vec3::new(f(0)?, f(1)?, f(2)?);
        let voxel_size = f(3)?;
        let dims = UVec3::new(u(0)?, u(1)?, u(2)?);
        let outside_is_solid = *bytes.get(36).ok_or(Truncated)? != 0;
        let sets = if version >= 2 {
            *bytes.get(37).ok_or(Truncated)?
        } else {
            0
        };
        let count = voxel_count(dims).filter(|&n| n > 0).ok_or(Invalid)?;
        if !origin.is_finite() || !voxel_size.is_finite() || voxel_size <= 0.0 || sets & !3 != 0 {
            return Err(Invalid);
        }
        // Fewer than 2^32 voxels: at most 512 MB per set, so no sum below
        // overflows, even with 32-bit sizes.
        let len = count.div_ceil(64) * 8;
        let mut at = if version >= 2 { 38 } else { 37 };
        let mut read = |present: bool| -> Result<Vec<u64>, VoxelBytesError> {
            if !present {
                return Ok(Vec::new());
            }
            let (chunks, _) = bytes.get(at..at + len).ok_or(Truncated)?.as_chunks::<8>();
            at += len;
            Ok(chunks.iter().map(|c| u64::from_le_bytes(*c)).collect())
        };
        let bits = read(true)?;
        let clipped = read(sets & 1 != 0)?;
        let hollow = read(sets & 2 != 0)?;
        if at != bytes.len() {
            return Err(Invalid);
        }
        Ok(Self {
            origin,
            voxel_size,
            dims,
            bits,
            clipped,
            hollow,
            outside_is_solid,
        })
    }

    /// The size of its voxels, in the units it was built in.
    pub fn voxel_size(&self) -> f32 {
        self.voxel_size
    }

    fn lattice(&self) -> Lattice {
        Lattice {
            first: (self.origin / self.voxel_size).round().as_ivec3(),
            voxel_size: self.voxel_size,
            dims: self.dims,
        }
    }

    fn cell(&self, p: Vec3) -> IVec3 {
        ((p - self.origin) / self.voxel_size).floor().as_ivec3()
    }

    fn solid_cell(&self, c: IVec3) -> bool {
        match self.lattice().index(c) {
            Some(i) => bit(&self.bits, i),
            None => self.outside_is_solid,
        }
    }
}

/// Does the triangle touch the axis-aligned box? Separating axis test
/// (Akenine-Möller): the box's three axes, the triangle's normal, and the
/// nine cross products of their edges.
pub(crate) fn triangle_overlaps_box(t: &[Vec3; 3], center: Vec3, half: Vec3) -> bool {
    let v = [t[0] - center, t[1] - center, t[2] - center];
    let edges = [v[1] - v[0], v[2] - v[1], v[0] - v[2]];
    let separated = |axis: Vec3| {
        let p = [v[0].dot(axis), v[1].dot(axis), v[2].dot(axis)];
        let r = half.x * axis.x.abs() + half.y * axis.y.abs() + half.z * axis.z.abs();
        p[0].min(p[1]).min(p[2]) > r || p[0].max(p[1]).max(p[2]) < -r
    };
    for e in edges {
        for a in [Vec3::X, Vec3::Y, Vec3::Z] {
            let axis = a.cross(e);
            if axis.length_squared() > 1e-20 && separated(axis) {
                return false;
            }
        }
    }
    let lo = v[0].min(v[1]).min(v[2]);
    let hi = v[0].max(v[1]).max(v[2]);
    if lo.cmpgt(half).any() || hi.cmplt(-half).any() {
        return false;
    }
    let n = edges[0].cross(edges[1]);
    !(n.length_squared() > 1e-20 && separated(n))
}

impl Level for VoxelObstacles {
    fn is_blocked(&self, center: Vec3, _half_extent: f32) -> bool {
        self.solid_cell(self.cell(center))
    }

    fn is_inside(&self, point: Vec3) -> bool {
        match self.lattice().index(self.cell(point)) {
            Some(i) if bit(&self.bits, i) => !bit(&self.clipped, i),
            Some(i) => bit(&self.hollow, i),
            None => self.outside_is_solid,
        }
    }

    fn ray_hit(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> bool {
        // Amanatides & Woo voxel traversal.
        let mut cell = self.cell(origin);
        let step = dir.signum().as_ivec3();
        let inv = dir.abs().max(Vec3::splat(1e-9)).recip();
        let next_boundary = (cell.as_vec3() + step.max(IVec3::ZERO).as_vec3()) * self.voxel_size + self.origin;
        let mut t_max = (next_boundary - origin).abs() * inv;
        let t_delta = Vec3::splat(self.voxel_size) * inv;
        let mut t = 0.0;
        let max_steps = (max_dist / self.voxel_size) as u32 * 3 + 3;
        let dims = self.dims.as_ivec3();
        for _ in 0..max_steps {
            if t > max_dist {
                return false;
            }
            let outside = cell.cmplt(IVec3::ZERO).any() || cell.cmpge(dims).any();
            if outside {
                // Once a ray has left the voxelised region it never comes back.
                let leaving =
                    (cell.cmplt(IVec3::ZERO) & step.cmple(IVec3::ZERO)) | (cell.cmpge(dims) & step.cmpge(IVec3::ZERO));
                if leaving.any() {
                    return self.outside_is_solid;
                }
            } else if self.solid_cell(cell) {
                return true;
            }
            if t_max.x < t_max.y && t_max.x < t_max.z {
                t = t_max.x;
                t_max.x += t_delta.x;
                cell.x += step.x;
            } else if t_max.y < t_max.z {
                t = t_max.y;
                t_max.y += t_delta.y;
                cell.y += step.y;
            } else {
                t = t_max.z;
                t_max.z += t_delta.z;
                cell.z += step.z;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_ray_and_overlap() {
        let b = ObstacleBox::from_center_size(Vec3::new(0.0, 1.0, 5.0), Vec3::new(4.0, 2.0, 0.2));
        assert!(b.ray_entry(Vec3::new(0.0, 1.0, 0.0), Vec3::Z, 10.0).is_some());
        assert!(b.ray_entry(Vec3::new(0.0, 1.0, 0.0), -Vec3::Z, 10.0).is_none());
        assert!(b.overlaps_cube(Vec3::new(0.0, 1.0, 5.0), 0.15));
        assert!(!b.overlaps_cube(Vec3::new(0.0, 1.0, 5.4), 0.15));
    }

    #[test]
    fn voxel_obstacles_match_boxes() {
        let boxes = BoxObstacles::new(vec![ObstacleBox::from_center_size(
            Vec3::new(0.0, 1.5, 0.0),
            Vec3::new(1.0, 3.0, 1.0),
        )]);
        let vox = VoxelObstacles::from_level(Vec3::new(-4.0, -1.0, -4.0), Vec3::new(4.0, 4.0, 4.0), 0.25, &boxes);
        assert!(vox.is_blocked(Vec3::new(0.0, 1.5, 0.0), 0.125));
        assert!(!vox.is_blocked(Vec3::new(2.0, 1.5, 0.0), 0.125));
        assert!(vox.is_blocked(Vec3::new(2.0, -0.5, 0.0), 0.125)); // ground
        assert!(vox.ray_hit(Vec3::new(-3.0, 1.5, 0.0), Vec3::X, 10.0));
        assert!(!vox.ray_hit(Vec3::new(-3.0, 1.5, 0.0), Vec3::Y, 2.0));
        assert!(vox.ray_hit(Vec3::new(2.0, 1.0, 0.0), -Vec3::Y, 2.0)); // hits ground
    }

    #[test]
    fn triangles_voxelise_like_boxes() {
        // A floor exactly on a voxel boundary, facing up, a ground sheet
        // 5 cm above the lattice beside it, and a wall sheet facing +x, 5 cm
        // into the voxels at 0 ≤ x < 0.3.
        let quad = |a: Vec3, b: Vec3, c: Vec3, d: Vec3| [[a, b, c], [a, c, d]];
        let mut tris = Vec::new();
        tris.extend(quad(
            Vec3::new(-3.0, 0.0, -3.0),
            Vec3::new(-3.0, 0.0, 3.0),
            Vec3::new(3.0, 0.0, 3.0),
            Vec3::new(3.0, 0.0, -3.0),
        ));
        tris.extend(quad(
            Vec3::new(3.0, 0.05, -3.0),
            Vec3::new(3.0, 0.05, 3.0),
            Vec3::new(6.0, 0.05, 3.0),
            Vec3::new(6.0, 0.05, -3.0),
        ));
        tris.extend(quad(
            Vec3::new(0.05, 0.0, -2.0),
            Vec3::new(0.05, 2.0, -2.0),
            Vec3::new(0.05, 2.0, 2.0),
            Vec3::new(0.05, 0.0, 2.0),
        ));
        let vox = VoxelObstacles::from_triangles(&tris, 0.3, 0.6, UpAxis::Y);
        // The voxel just above the floor is open, the one below solid, its
        // centre under the floor.
        let (above, below) = (Vec3::new(-1.95, 0.15, 0.15), Vec3::new(-1.95, -0.15, 0.15));
        assert!(!vox.is_blocked(above, 0.15) && !vox.is_inside(above));
        assert!(vox.is_blocked(below, 0.15) && vox.is_inside(below));
        // The raised ground blocks the voxel it passes through, whose centre
        // is above it; the open voxel under it counts as inside (the ground).
        let (on, under) = (Vec3::new(4.05, 0.15, 0.15), Vec3::new(4.05, -0.15, 0.15));
        assert!(vox.is_blocked(on, 0.15) && !vox.is_inside(on));
        assert!(!vox.is_blocked(under, 0.15) && vox.is_inside(under));
        // The wall blocks; beside it is open.
        assert!(vox.is_blocked(Vec3::new(0.15, 1.05, 0.15), 0.15));
        assert!(!vox.is_blocked(Vec3::new(0.75, 1.05, 0.15), 0.15));
        assert!(!vox.is_blocked(Vec3::new(-0.45, 1.05, 0.15), 0.15));
        // The voxel the wall passes through has its centre in front of it,
        // and a vertical sheet has no inside behind it.
        assert!(!vox.is_inside(Vec3::new(0.15, 1.05, 0.15)));
        assert!(!vox.is_inside(Vec3::new(-0.15, 1.05, 0.15)));
        // A round trip through bytes.
        let back = VoxelObstacles::from_bytes(&vox.to_bytes()).expect("parses");
        assert_eq!(
            (&back.bits, &back.clipped, &back.hollow),
            (&vox.bits, &vox.clipped, &vox.hollow)
        );
        assert!(!vox.clipped.is_empty() && !vox.hollow.is_empty());
        assert_eq!(
            (back.origin, back.dims, back.voxel_size),
            (vox.origin, vox.dims, vox.voxel_size)
        );
        assert_eq!(
            VoxelObstacles::from_bytes(b"nope").err(),
            Some(VoxelBytesError::NotVoxels)
        );
    }

    #[test]
    fn bytes_that_are_not_voxels_are_errors_not_panics() {
        use VoxelBytesError::{Invalid, NotVoxels, Truncated, UnsupportedVersion};
        let vox = VoxelObstacles::build(Vec3::splat(-1.0), Vec3::splat(1.0), 0.3, |c, _| c.y < 0.0);
        let good = vox.to_bytes();
        let read = |bytes: &[u8]| VoxelObstacles::from_bytes(bytes).err();
        assert_eq!(read(&good), None);
        // A header like `good`'s with one field set: a word at `at`.
        let with = |at: usize, word: [u8; 4]| {
            let mut b = good.clone();
            b[at..at + 4].copy_from_slice(&word);
            b
        };
        assert_eq!(read(b""), Some(NotVoxels));
        assert_eq!(read(b"SMK"), Some(NotVoxels));
        assert_eq!(read(&with(0, *b"SMKW")), Some(NotVoxels));
        assert_eq!(read(&good[..6]), Some(Truncated));
        assert_eq!(read(&with(4, 3u32.to_le_bytes())), Some(UnsupportedVersion(3)));
        assert_eq!(read(&with(4, 0u32.to_le_bytes())), Some(UnsupportedVersion(0)));
        // Cut anywhere in the header or the voxels.
        for len in [10, 30, 36, 37, 38, good.len() - 1] {
            assert_eq!(read(&good[..len]), Some(Truncated), "{len}");
        }
        // More bytes after the voxels.
        let mut long = good.clone();
        long.push(0);
        assert_eq!(read(&long), Some(Invalid));
        // Sizes that don't add up: none of these may panic or allocate what
        // the bytes don't hold.
        for (at, word) in [
            (20, 0.0f32.to_le_bytes()),
            (20, (-0.3f32).to_le_bytes()),
            (20, f32::NAN.to_le_bytes()),
            (8, f32::INFINITY.to_le_bytes()),
            (24, 0u32.to_le_bytes()),
            (24, u32::MAX.to_le_bytes()),
        ] {
            assert_eq!(read(&with(at, word)), Some(Invalid), "{at}");
        }
        let mut huge = with(24, u32::MAX.to_le_bytes());
        huge[28..36].copy_from_slice(&[0xff; 8]);
        assert_eq!(read(&huge), Some(Invalid));
        // Dimensions bigger than the voxels sent, but a count that fits.
        assert_eq!(read(&with(24, 1000u32.to_le_bytes())), Some(Truncated));
        // Unknown sets of voxels.
        let mut sets = good.clone();
        sets[37] |= 4;
        assert_eq!(read(&sets), Some(Invalid));
        assert_eq!(
            VoxelBytesError::UnsupportedVersion(3).to_string(),
            "smokescreen voxels in format 3, which this version can't read (it reads 1 and 2)"
        );
    }

    #[test]
    #[should_panic(expected = "voxel size above 0")]
    fn voxels_of_no_size_are_refused() {
        VoxelObstacles::build(Vec3::ZERO, Vec3::ONE, 0.0, |_, _| false);
    }

    #[test]
    #[should_panic(expected = "too large")]
    fn levels_of_more_voxels_than_the_lattice_counts_are_refused() {
        // 10 km × 1 km × 10 km at 1 cm: 10^15 voxels.
        VoxelObstacles::build(Vec3::ZERO, Vec3::new(1.0e4, 1.0e3, 1.0e4), 0.01, |_, _| false);
    }

    #[test]
    fn triangles_facing_both_ways_or_inwards_are_not_hollow() {
        let quad = |a: Vec3, b: Vec3, c: Vec3, d: Vec3| [[a, b, c], [a, c, d]];
        // A two-sided sheet: the same wall facing +x and -x.
        let (a, b, c, d) = (
            Vec3::new(0.05, 0.0, -2.0),
            Vec3::new(0.05, 2.0, -2.0),
            Vec3::new(0.05, 2.0, 2.0),
            Vec3::new(0.05, 0.0, 2.0),
        );
        let mut tris = quad(a, b, c, d).to_vec();
        tris.extend(quad(a, d, c, b));
        let vox = VoxelObstacles::from_triangles(&tris, 0.3, 0.6, UpAxis::Y);
        assert!(!vox.is_inside(Vec3::new(0.15, 1.05, 0.15)));
        assert!(!vox.is_inside(Vec3::new(-0.15, 1.05, 0.15)));
        assert!(vox.hollow.is_empty());
        // A box 1.2 m across facing inwards: nothing is inside it. Facing
        // outwards, it is hollow inside next to the voxels its far faces
        // clip (all the fill ever asks about).
        let (lo, hi) = (Vec3::new(0.05, 0.05, 0.05), Vec3::new(1.25, 1.25, 1.25));
        let corner = |i: usize| {
            Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            )
        };
        // Faces as corner indices, counter-clockwise seen from outside.
        let faces = [
            [0, 4, 6, 2],
            [1, 3, 7, 5],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 2, 3, 1],
            [4, 5, 7, 6],
        ];
        let outwards: Vec<[Vec3; 3]> = faces
            .iter()
            .flat_map(|f| quad(corner(f[0]), corner(f[1]), corner(f[2]), corner(f[3])))
            .collect();
        let inwards: Vec<[Vec3; 3]> = outwards.iter().map(|t| [t[0], t[2], t[1]]).collect();
        let middle = Vec3::new(1.05, 0.45, 0.45);
        let clipped = Vec3::new(1.35, 0.45, 0.45);
        let out = VoxelObstacles::from_triangles(&outwards, 0.3, 0.6, UpAxis::Y);
        assert!(!out.is_blocked(middle, 0.15) && out.is_inside(middle));
        assert!(out.is_blocked(clipped, 0.15) && !out.is_inside(clipped));
        let inside_out = VoxelObstacles::from_triangles(&inwards, 0.3, 0.6, UpAxis::Y);
        assert!(!inside_out.is_blocked(middle, 0.15) && !inside_out.is_inside(middle));
        assert!(inside_out.hollow.is_empty());
    }

    #[test]
    fn voxel_obstacles_know_which_voxels_a_wall_only_clips() {
        // A wall reaching 6 cm into the voxels at 1.8 ≤ x < 2.1.
        let wall = ObstacleBox {
            min: Vec3::new(2.04, 0.0, -3.0),
            max: Vec3::new(3.0, 3.0, 3.0),
        };
        let boxes = BoxObstacles::new(vec![wall]);
        let vox = VoxelObstacles::from_level(Vec3::new(-3.0, -1.0, -3.0), Vec3::new(4.0, 4.0, 4.0), 0.3, &boxes);
        let clipped = Vec3::new(1.95, 1.05, 0.15);
        assert!(vox.is_blocked(clipped, 0.15) && !vox.is_inside(clipped));
        let deep = Vec3::new(2.25, 1.05, 0.15);
        assert!(vox.is_blocked(deep, 0.15) && vox.is_inside(deep));
        // The same answers as the boxes at every voxel's centre.
        for z in 0..vox.dims.z {
            for y in 0..vox.dims.y {
                for x in 0..vox.dims.x {
                    let i = (x + vox.dims.x * (y + vox.dims.y * z)) as usize;
                    let c = vox.lattice().centre(i);
                    assert_eq!(vox.is_blocked(c, 0.15), boxes.is_blocked(c, 0.15), "{c}");
                    if boxes.is_blocked(c, 0.15) {
                        assert_eq!(vox.is_inside(c), boxes.is_inside(c), "{c}");
                    }
                }
            }
        }
        // Kept through bytes.
        let bytes = vox.to_bytes();
        let back = VoxelObstacles::from_bytes(&bytes).expect("parses");
        assert_eq!((&back.bits, &back.clipped), (&vox.bits, &vox.clipped));
        // The first version's bytes (no clipped voxels) still read.
        let words = vox.bits.len() * 8;
        let mut v1 = bytes[..37].to_vec();
        v1[4..8].copy_from_slice(&1u32.to_le_bytes());
        v1.extend_from_slice(&bytes[38..38 + words]);
        let old = VoxelObstacles::from_bytes(&v1).expect("parses");
        assert_eq!(old.bits, vox.bits);
        assert!(old.clipped.is_empty() && old.is_inside(clipped));
    }

    #[test]
    fn ground_follows_the_up_axis() {
        let g = FlatGround::new(0.0, UpAxis::Z);
        assert!(g.is_blocked(Vec3::new(0.0, 5.0, -1.0), 0.1));
        assert!(!g.is_blocked(Vec3::new(0.0, -5.0, 1.0), 0.1));
        assert!(g.ray_hit(Vec3::new(0.0, 0.0, 2.0), -Vec3::Z, 3.0));
    }
}
