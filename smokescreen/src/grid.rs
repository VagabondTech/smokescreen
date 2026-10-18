//! The voxel grid a flood fill produces.

use glam::{IVec3, UVec3, Vec3};

use crate::math::BYTE_TO_UNIT;

/// Distance value of a voxel the smoke never reached.
pub const EMPTY_VOXEL: u8 = 255;

/// Filled voxels store their normalised fill distance in `0..=FILLED_MAX` (as
/// a fraction of 255). The gap up to 1 (empty) gives trilinear filtering room
/// to fade the boundary out.
pub const FILLED_MAX: f32 = 0.8;

/// Region distances are stored in ±this many metres.
pub const REGION_RANGE: f32 = 1.6;

/// Encode a signed distance (metres, negative inside) for the region channel.
#[inline]
pub fn encode_region(sdf: f32) -> u8 {
    ((sdf / REGION_RANGE).clamp(-1.0, 1.0) * 127.5 + 127.5).round() as u8
}

/// Decode a region value (0..1, as a renderer samples it) to metres.
#[inline]
pub fn decode_region(v: f32) -> f32 {
    (v * 2.0 - 1.0) * REGION_RANGE
}

/// What the fill learned about the level, per voxel.
pub mod occupancy {
    pub const UNKNOWN: u8 = 0;
    pub const OPEN: u8 = 1;
    /// The level blocks it. Also an open voxel beside a padded one whose
    /// centre is inside the level: the hollow inside of a wall made of
    /// surfaces, which samples skip like the wall.
    pub const SOLID: u8 = 2;
    /// Solid, because a wall reaches into it, but its centre is open air in
    /// front of a wall that runs on behind it: it holds the smoke in front
    /// of it, so the smoke reaches the wall instead of stopping at the
    /// voxel's centre. Samples treat it as open.
    pub const PADDED: u8 = 3;
}

/// Result of a flood fill, in the internal frame (metres, Y up). Immutable
/// once built; shared by the simulation, its queries and renderers.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    /// Minimum corner of the grid.
    pub min: Vec3,
    pub voxel_size: f32,
    pub dims: UVec3,
    /// Per voxel, x fastest then y then z: normalised fill distance × 255
    /// (see [`FILLED_MAX`]), or [`EMPTY_VOXEL`].
    pub distance: Vec<u8>,
    /// Per voxel: signed distance to the fill plus its halo, encoded with
    /// [`encode_region`]; 255 ("far outside") when no halo was requested.
    /// Faceted puffs are clipped to it so they never poke through walls.
    pub region: Vec<u8>,
    /// Per voxel: [`occupancy`] values. Known for every voxel next to a
    /// filled or padded one; samples skip the solid ones (see
    /// [`Grid::sample_distance`]). May be empty (everything open).
    pub occupancy: Vec<u8>,
    /// Number of filled voxels.
    pub filled: u32,
    /// Bounds of the filled voxels, padded by one voxel.
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    /// Per brick of [`BRICK`]³ voxels: the smallest distance value in the
    /// brick and one voxel around it, which is everything a trilinear sample
    /// inside the brick reads. [`EMPTY_VOXEL`] means the brick is empty, so
    /// queries skip it. Built by [`Grid::build_bricks`].
    pub bricks: Vec<u8>,
    /// Per distance value `v` (0..=254): a box holding every point whose
    /// trilinear sample reads a voxel with distance ≤ `v`, or `None` if no
    /// voxel has. Queries use it to skip the parts of a cloud the growth
    /// front hasn't reached yet or the fade has already eaten. Built by
    /// [`Grid::build_front_bounds`].
    pub front_bounds: Vec<Option<(Vec3, Vec3)>>,
}

/// Edge length of a brick, in voxels.
pub const BRICK: u32 = 4;

/// Trilinearly filtered value (0..1) of a byte channel of a voxel grid, like a
/// GPU's linear sampler with clamp-to-edge addressing.
#[inline]
pub(crate) fn sample_channel(channel: &[u8], min: Vec3, voxel_size: f32, dims: UVec3, p: Vec3) -> f32 {
    let g = (p - min) * (1.0 / voxel_size) - 0.5;
    let max = (dims - 1).as_vec3();
    let g = g.clamp(Vec3::ZERO, max);
    // Non-negative, so truncation is floor (and much cheaper on x86-64,
    // where floor is a function call without SSE4.1).
    let c0 = g.as_uvec3();
    let f = g - c0.as_vec3();
    // Neighbour offsets, zero at the far edge (clamp-to-edge).
    let (dx, dy) = (dims.x as usize, (dims.x * dims.y) as usize);
    let ox = (c0.x + 1 < dims.x) as usize;
    let oy = (c0.y + 1 < dims.y) as usize * dx;
    let oz = (c0.z + 1 < dims.z) as usize * dy;
    let i = (c0.x + dims.x * (c0.y + dims.y * c0.z)) as usize;
    let d = |k: usize| BYTE_TO_UNIT[channel[k] as usize];
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(d(i), d(i + ox), f.x);
    let x10 = lerp(d(i + oy), d(i + oy + ox), f.x);
    let x01 = lerp(d(i + oz), d(i + oz + ox), f.x);
    let x11 = lerp(d(i + oz + oy), d(i + oz + oy + ox), f.x);
    lerp(lerp(x00, x10, f.y), lerp(x01, x11, f.y), f.z)
}

/// Like [`sample_channel`], but solid voxels (see [`occupancy`]) don't take
/// part, and each voxel's byte is first turned into a value by `values`
/// (`byte / 255` for a distance, a [`crate::timeline::DensityTable`]'s
/// values for a density): the result is the trilinear average of the open
/// voxels' values around `p`, and the value of [`EMPTY_VOXEL`] when all
/// eight are solid. Next to a wall or floor a sample therefore reads the
/// smoke in front of it instead of fading towards the empty solid voxels,
/// right up to the middle of the first solid voxel. Where a wall only clips
/// that voxel, so that its middle is still in front of the wall, the fill
/// pads it ([`occupancy::PADDED`]: open to samples, holding the smoke in
/// front of it), and the smoke reaches into the wall. Renderers do the same
/// by dividing a filtered value by the filtered open mask
/// ([`Grid::open_texels`]).
#[inline]
pub(crate) fn sample_open(
    channel: &[u8],
    values: &[f32; 256],
    occ: &[u8],
    min: Vec3,
    voxel_size: f32,
    dims: UVec3,
    p: Vec3,
) -> f32 {
    let solid = |k: usize| occ.get(k) == Some(&occupancy::SOLID);
    sample_open_by(channel, values, occ, min, voxel_size, dims, p, |_, corners| {
        corners.iter().any(|&k| solid(k))
    })
}

/// [`sample_open`] with the grid's near-solid flags (see
/// [`near_solid_cells`]): the same value, with one byte tested instead of
/// eight to find the common case, away from walls.
#[allow(clippy::too_many_arguments)]
#[inline]
pub(crate) fn sample_open_near(
    channel: &[u8],
    values: &[f32; 256],
    occ: &[u8],
    near: &[u8],
    min: Vec3,
    voxel_size: f32,
    dims: UVec3,
    p: Vec3,
) -> f32 {
    sample_open_by(channel, values, occ, min, voxel_size, dims, p, |i, _| {
        near.get(i).is_some_and(|&f| f != 0)
    })
}

/// Per voxel, for the cell of eight voxels starting at it (the ones a
/// trilinear sample between them reads): 1 if any of the eight is solid,
/// else 0. Empty if `occ` is. Built once per cloud, so a sample can tell
/// the common case, away from walls, from one byte.
pub(crate) fn near_solid_cells(occ: &[u8], dims: UVec3) -> Vec<u8> {
    if occ.is_empty() {
        return Vec::new();
    }
    let mut near = vec![0u8; occ.len()];
    let (dx, dxy) = (dims.x as usize, (dims.x * dims.y) as usize);
    for (i, _) in occ.iter().enumerate().filter(|&(_, &o)| o == occupancy::SOLID) {
        let c = IVec3::new((i % dx) as i32, ((i % dxy) / dx) as i32, (i / dxy) as i32);
        // The cells with this voxel among their corners: the one starting
        // here and the ones starting a voxel back along any axes. (At the
        // far edge a cell reads its own voxel twice, so the voxel is a
        // corner of its own cell either way.)
        for n in 0..8 {
            let k = c - IVec3::new(n & 1, (n >> 1) & 1, n >> 2);
            if k.cmpge(IVec3::ZERO).all() {
                near[k.x as usize + dx * (k.y as usize + dims.y as usize * k.z as usize)] = 1;
            }
        }
    }
    near
}

/// The open-voxel average of [`sample_open`], with `any_solid(i, corners)`
/// deciding whether a solid voxel is among the eight corners (voxel `i`
/// and its neighbours): if not, plain trilinear filtering.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn sample_open_by(
    channel: &[u8],
    values: &[f32; 256],
    occ: &[u8],
    min: Vec3,
    voxel_size: f32,
    dims: UVec3,
    p: Vec3,
    any_solid: impl FnOnce(usize, &[usize; 8]) -> bool,
) -> f32 {
    let g = (p - min) * (1.0 / voxel_size) - 0.5;
    let max = (dims - 1).as_vec3();
    let g = g.clamp(Vec3::ZERO, max);
    // Non-negative, so truncation is floor.
    let c0 = g.as_uvec3();
    let f = g - c0.as_vec3();
    // Neighbour offsets, zero at the far edge (clamp-to-edge).
    let (dx, dy) = (dims.x as usize, (dims.x * dims.y) as usize);
    let ox = (c0.x + 1 < dims.x) as usize;
    let oy = (c0.y + 1 < dims.y) as usize * dx;
    let oz = (c0.z + 1 < dims.z) as usize * dy;
    let i = (c0.x + dims.x * (c0.y + dims.y * c0.z)) as usize;
    // The eight corners, x fastest.
    let corners = [
        i,
        i + ox,
        i + oy,
        i + oy + ox,
        i + oz,
        i + oz + ox,
        i + oz + oy,
        i + oz + oy + ox,
    ];
    let d = |k: usize| values[channel[k] as usize];
    let solid = |k: usize| occ.get(k) == Some(&occupancy::SOLID);
    if !any_solid(i, &corners) {
        // The common case, away from walls: plain trilinear filtering.
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let x00 = lerp(d(corners[0]), d(corners[1]), f.x);
        let x10 = lerp(d(corners[2]), d(corners[3]), f.x);
        let x01 = lerp(d(corners[4]), d(corners[5]), f.x);
        let x11 = lerp(d(corners[6]), d(corners[7]), f.x);
        return lerp(lerp(x00, x10, f.y), lerp(x01, x11, f.y), f.z);
    }
    let wx = [1.0 - f.x, f.x];
    let wy = [1.0 - f.y, f.y];
    let wz = [1.0 - f.z, f.z];
    let (mut sum, mut weight) = (0.0f32, 0.0f32);
    for (n, &k) in corners.iter().enumerate() {
        if !solid(k) {
            let w = wx[n & 1] * wy[(n >> 1) & 1] * wz[n >> 2];
            sum += w * d(k);
            weight += w;
        }
    }
    if weight > 0.0 {
        sum / weight
    } else {
        values[EMPTY_VOXEL as usize]
    }
}

/// See [`Grid::front_bounds`]: from `(voxel index, distance)` pairs.
pub(crate) fn front_bounds(
    min: Vec3,
    voxel: f32,
    dims: UVec3,
    voxels: impl Iterator<Item = (usize, u8)>,
) -> Vec<Option<(Vec3, Vec3)>> {
    let mut lo = [IVec3::MAX; 255];
    let mut hi = [IVec3::MIN; 255];
    let (dx, dxy) = (dims.x as usize, (dims.x * dims.y) as usize);
    for (i, b) in voxels {
        if b == EMPTY_VOXEL {
            continue;
        }
        let c = IVec3::new((i % dx) as i32, ((i % dxy) / dx) as i32, (i / dxy) as i32);
        lo[b as usize] = lo[b as usize].min(c);
        hi[b as usize] = hi[b as usize].max(c);
    }
    let (mut l, mut h) = (IVec3::MAX, IVec3::MIN);
    // A trilinear sample at p reads voxels within one voxel of it, so pad
    // the voxels' cells by one; and a little more, so rounding never leaves
    // out a point that reads one.
    let pad = Vec3::splat(1.0 + 1e-3);
    (0..255)
        .map(|b| {
            l = l.min(lo[b]);
            h = h.max(hi[b]);
            l.cmple(h).all().then(|| {
                (
                    min + (l.as_vec3() + 0.5 - pad) * voxel,
                    min + (h.as_vec3() + 0.5 + pad) * voxel,
                )
            })
        })
        .collect()
}

/// Compute one z-layer of bricks (see [`Grid::bricks`]).
pub(crate) fn brick_layer(distance: &[u8], dims: UVec3, bz: u32, bricks: &mut [u8]) {
    let bd = (dims + BRICK - 1) / BRICK;
    let d = dims.as_ivec3();
    let b = BRICK as i32;
    for by in 0..bd.y as i32 {
        for bx in 0..bd.x as i32 {
            let corner = IVec3::new(bx, by, bz as i32) * b;
            let lo = (corner - 1).max(IVec3::ZERO);
            let hi = (corner + b).min(d - 1);
            let mut m = EMPTY_VOXEL;
            for z in lo.z..=hi.z {
                for y in lo.y..=hi.y {
                    let row = (d.x * (y + d.y * z)) as usize;
                    for x in lo.x..=hi.x {
                        m = m.min(distance[row + x as usize]);
                    }
                }
            }
            bricks[(bx as u32 + bd.x * (by as u32 + bd.y * bz)) as usize] = m;
        }
    }
}

impl Grid {
    pub fn is_empty(&self) -> bool {
        self.filled == 0
    }

    pub fn len(&self) -> usize {
        self.distance.len()
    }

    #[inline]
    pub fn index(&self, c: UVec3) -> usize {
        (c.x + self.dims.x * (c.y + self.dims.y * c.z)) as usize
    }

    /// Normalised distance at a voxel (1 = empty).
    #[inline]
    pub fn distance_at(&self, c: UVec3) -> f32 {
        BYTE_TO_UNIT[self.distance[self.index(c)] as usize]
    }

    /// Trilinearly filtered normalised distance, like a GPU's linear sampler
    /// with clamp-to-edge addressing, over the open voxels only: solid
    /// voxels don't take part, so smoke pressed against a wall or floor stays
    /// as dense as the voxels in front of it, right up to the wall (see
    /// `sample_open`). 1 where all eight voxels are solid.
    #[inline]
    pub fn sample_distance(&self, p: Vec3) -> f32 {
        sample_open(
            &self.distance,
            &BYTE_TO_UNIT,
            &self.occupancy,
            self.min,
            self.voxel_size,
            self.dims,
            p,
        )
    }

    /// Per voxel, 255 where the voxel is open (or unknown, or padded) and 0
    /// where the level is solid: for a renderer's open-mask texture. Filtered like the
    /// density, it is the open fraction to divide by (see
    /// [`Self::sample_distance`]).
    pub fn open_texels(&self) -> Vec<u8> {
        (0..self.len())
            .map(|i| {
                if self.occupancy.get(i) == Some(&occupancy::SOLID) {
                    0
                } else {
                    255
                }
            })
            .collect()
    }

    /// Signed distance (metres) to the fill plus its halo, trilinearly
    /// filtered. Only meaningful when the fill had a halo.
    #[inline]
    pub fn sample_region(&self, p: Vec3) -> f32 {
        decode_region(self.sample(&self.region, p))
    }

    #[inline]
    fn sample(&self, channel: &[u8], p: Vec3) -> f32 {
        sample_channel(channel, self.min, self.voxel_size, self.dims, p)
    }

    /// Bricks per axis.
    pub fn brick_dims(&self) -> UVec3 {
        (self.dims + BRICK - 1) / BRICK
    }

    /// Fill [`Self::front_bounds`] from the distance channel.
    pub fn build_front_bounds(&mut self) {
        self.front_bounds = front_bounds(
            self.min,
            self.voxel_size,
            self.dims,
            self.distance.iter().copied().enumerate(),
        );
    }

    /// Fill [`Self::bricks`] from the distance channel.
    pub fn build_bricks(&mut self) {
        let bd = self.brick_dims();
        let mut bricks = vec![EMPTY_VOXEL; (bd.x * bd.y * bd.z) as usize];
        for bz in 0..bd.z {
            brick_layer(&self.distance, self.dims, bz, &mut bricks);
        }
        self.bricks = bricks;
    }

    /// The brick a point falls in (clamped to the grid), with its bounds.
    #[inline]
    pub fn brick_at(&self, p: Vec3) -> (usize, Vec3, Vec3) {
        let size = self.voxel_size * BRICK as f32;
        let bd = self.brick_dims();
        let c = ((p - self.min) * (1.0 / size)).max(Vec3::ZERO).as_uvec3().min(bd - 1);
        let lo = self.min + c.as_vec3() * size;
        ((c.x + bd.x * (c.y + bd.y * c.z)) as usize, lo, lo + size)
    }

    /// Interleaved RGBA8 texels for a GPU 3D texture: distance, sun
    /// visibility, sky visibility, region. Without lighting both visibility
    /// channels are 255 (fully lit).
    pub fn rgba_texels(&self, lighting: Option<&[[u8; 2]]>) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len() * 4);
        for i in 0..self.len() {
            let [sun, sky] = lighting.map_or([255, 255], |l| l[i]);
            out.extend_from_slice(&[self.distance[i], sun, sky, self.region[i]]);
        }
        out
    }
}

/// Approximate Euclidean distance (in voxels) from every voxel in the box
/// `lo..=hi` to the nearest voxel of a mask, with a two-pass 3×3×3 chamfer
/// transform. Runs a z-slice at a time so a fill can spread it over ticks.
#[derive(Clone, Debug)]
pub(crate) struct Chamfer {
    dims: IVec3,
    lo: IVec3,
    hi: IVec3,
    size: IVec3,
    /// Box-local distances with a one-voxel border of [`CHAMFER_FAR`], so
    /// relaxing never needs a bounds check. x fastest. Allocated zeroed and
    /// first written by the seeding pass, a slice at a time, so its pages
    /// aren't all touched in one go.
    d: Vec<f32>,
    /// Strides of `d` along y and z.
    sy: usize,
    sz: usize,
    /// [`BEFORE`] as offsets into `d`.
    offsets: [usize; 13],
    /// 0: seeding from the mask, 1: forward pass, 2: backward pass, 3: done.
    pass: u8,
    /// Next slice of the current pass, box-local.
    z: i32,
}

/// Distance reported for voxels nothing reached (and outside the box).
pub(crate) const CHAMFER_FAR: f32 = 1e6;

impl Chamfer {
    pub fn new(dims: IVec3, lo: IVec3, hi: IVec3) -> Self {
        let size = (hi - lo + 1).max(IVec3::ZERO);
        let padded = size + 2;
        let sy = padded.x as usize;
        let sz = (padded.x * padded.y) as usize;
        let empty = size.cmple(IVec3::ZERO).any();
        let mut offsets = [0usize; 13];
        for (o, (v, _)) in offsets.iter_mut().zip(BEFORE.iter()) {
            // Negative offsets, as a distance back from the voxel.
            *o = (-(v.x as isize) - v.y as isize * sy as isize - v.z as isize * sz as isize) as usize;
        }
        Self {
            dims,
            lo,
            hi,
            size,
            d: if empty {
                Vec::new()
            } else {
                vec![0.0; (padded.x * padded.y * padded.z) as usize]
            },
            sy,
            sz,
            offsets,
            pass: 0,
            z: 0,
        }
    }

    pub fn is_done(&self) -> bool {
        self.pass == 3 || self.d.is_empty()
    }

    /// Voxels per slice, for work accounting.
    pub fn slice_len(&self) -> u64 {
        (self.size.x * self.size.y) as u64
    }

    /// Seeding from the mask (much cheaper per slice than the two passes
    /// after it)?
    pub fn is_seeding(&self) -> bool {
        self.pass == 0
    }

    #[inline]
    fn local(&self, c: IVec3) -> usize {
        let l = c - self.lo + 1;
        l.x as usize + self.sy * l.y as usize + self.sz * l.z as usize
    }

    /// Distance at a voxel of the full grid.
    #[inline]
    pub fn get(&self, c: IVec3) -> f32 {
        if self.d.is_empty() || c.cmplt(self.lo).any() || c.cmpgt(self.hi).any() {
            return CHAMFER_FAR;
        }
        self.d[self.local(c)]
    }

    /// Run one slice. `mask` takes a full-grid voxel index.
    pub fn step_slice(&mut self, mask: impl Fn(usize) -> bool) {
        if self.is_done() {
            self.pass = 3;
            return;
        }
        let dims = self.dims;
        match self.pass {
            0 => {
                // This slice and the border around it start far, the slices
                // of border before the first and after the last too.
                let slice = |k: i32| k as usize * self.sz..(k as usize + 1) * self.sz;
                if self.z == 0 {
                    self.d[slice(0)].fill(CHAMFER_FAR);
                }
                self.d[slice(self.z + 1)].fill(CHAMFER_FAR);
                if self.z == self.size.z - 1 {
                    self.d[slice(self.z + 2)].fill(CHAMFER_FAR);
                }
                let z = self.lo.z + self.z;
                for y in self.lo.y..=self.hi.y {
                    for x in self.lo.x..=self.hi.x {
                        let c = IVec3::new(x, y, z);
                        if mask((c.x + dims.x * (c.y + dims.y * c.z)) as usize) {
                            let i = self.local(c);
                            self.d[i] = 0.0;
                        }
                    }
                }
            }
            1 => {
                let z = self.lo.z + self.z;
                let n = self.size.x as usize;
                for y in self.lo.y..=self.hi.y {
                    let row = self.local(IVec3::new(self.lo.x, y, z));
                    // Every neighbour but x-1 lies in an earlier row, already
                    // final: relax against those first (vectorisable), then
                    // run along the row for x-1. Same result as relaxing
                    // against all 13 at once, since min is exact.
                    let (before, rest) = self.d.split_at_mut(row);
                    let current = &mut rest[..n];
                    for (k, (o, (_, w))) in self.offsets.iter().zip(BEFORE.iter()).enumerate() {
                        if k == 12 {
                            break;
                        }
                        let src = &before[row - o..row - o + n];
                        for (c, s) in current.iter_mut().zip(src) {
                            *c = c.min(*s + w);
                        }
                    }
                    let mut prev = before[row - 1];
                    for c in current.iter_mut() {
                        *c = c.min(prev + 1.0);
                        prev = *c;
                    }
                }
            }
            _ => {
                let z = self.hi.z - self.z;
                let n = self.size.x as usize;
                for y in (self.lo.y..=self.hi.y).rev() {
                    let row = self.local(IVec3::new(self.lo.x, y, z));
                    let (head, after) = self.d.split_at_mut(row + n);
                    let current = &mut head[row..];
                    for (k, (o, (_, w))) in self.offsets.iter().zip(BEFORE.iter()).enumerate() {
                        if k == 12 {
                            break;
                        }
                        // Offsets mirror: row + o lies in `after` at o - n.
                        let src = &after[o - n..*o];
                        for (c, s) in current.iter_mut().zip(src) {
                            *c = c.min(*s + w);
                        }
                    }
                    let mut prev = after[0];
                    for c in current.iter_mut().rev() {
                        *c = c.min(prev + 1.0);
                        prev = *c;
                    }
                }
            }
        }
        self.z += 1;
        if self.z >= self.size.z {
            self.z = 0;
            self.pass += 1;
        }
    }
}

/// The 13 neighbours that come before a voxel in x-fastest scan order, with
/// their distances.
static BEFORE: [(IVec3, f32); 13] = {
    const R2: f32 = core::f32::consts::SQRT_2;
    const R3: f32 = 1.732_050_8;
    [
        (IVec3::new(-1, -1, -1), R3),
        (IVec3::new(0, -1, -1), R2),
        (IVec3::new(1, -1, -1), R3),
        (IVec3::new(-1, 0, -1), R2),
        (IVec3::new(0, 0, -1), 1.0),
        (IVec3::new(1, 0, -1), R2),
        (IVec3::new(-1, 1, -1), R3),
        (IVec3::new(0, 1, -1), R2),
        (IVec3::new(1, 1, -1), R3),
        (IVec3::new(-1, -1, 0), R2),
        (IVec3::new(0, -1, 0), 1.0),
        (IVec3::new(1, -1, 0), R2),
        (IVec3::new(-1, 0, 0), 1.0),
    ]
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chamfer_weights_are_lengths() {
        for (o, w) in BEFORE.iter() {
            assert_eq!(o.as_vec3().length(), *w);
            // Before the voxel in x-fastest order.
            assert!((o.z, o.y, o.x) < (0, 0, 0));
        }
    }

    /// The near-solid flags find exactly the cells with a solid corner,
    /// and the flagged sample gives the very bits of the plain one, for
    /// distances and densities alike.
    #[test]
    fn near_solid_flags_give_the_same_samples() {
        let dims = UVec3::new(12, 8, 8);
        let n = (dims.x * dims.y * dims.z) as usize;
        let hash = crate::math::hash_u32;
        let distance: Vec<u8> = (0..n).map(|i| (hash(i as u32) % 300).min(255) as u8).collect();
        let occ: Vec<u8> = (0..n)
            .map(|i| match hash(i as u32 ^ 0x5bd1_e995) % 11 {
                0 => occupancy::SOLID,
                1 => occupancy::PADDED,
                2 => occupancy::UNKNOWN,
                _ => occupancy::OPEN,
            })
            .collect();
        let near = near_solid_cells(&occ, dims);
        let (dx, dxy) = (dims.x as usize, (dims.x * dims.y) as usize);
        for i in 0..n {
            let c = UVec3::new((i % dx) as u32, ((i % dxy) / dx) as u32, (i / dxy) as u32);
            let ox = (c.x + 1 < dims.x) as usize;
            let oy = (c.y + 1 < dims.y) as usize * dx;
            let oz = (c.z + 1 < dims.z) as usize * dxy;
            let solid = [0, ox, oy, oy + ox, oz, oz + ox, oz + oy, oz + oy + ox]
                .iter()
                .any(|&o| occ[i + o] == occupancy::SOLID);
            assert_eq!(near[i] != 0, solid, "cell {c:?}");
        }
        assert!(near.contains(&1) && near.contains(&0));
        let (min, v) = (Vec3::new(-1.0, 0.5, 2.0), 0.3);
        let extent = dims.as_vec3() * v;
        let density = crate::timeline::DensityTable::new(1.2, 0.5);
        for values in [&BYTE_TO_UNIT, density.values()] {
            for k in 0..4000u32 {
                let r = |s: u32| (hash(k.wrapping_mul(3) ^ s) >> 8) as f32 / (1u32 << 24) as f32;
                // Inside the grid and a little around it (clamp-to-edge).
                let p = min + Vec3::new(r(1), r(2), r(3)) * (extent + v) - v * 0.5;
                let plain = sample_open(&distance, values, &occ, min, v, dims, p);
                let flagged = sample_open_near(&distance, values, &occ, &near, min, v, dims, p);
                assert_eq!(plain.to_bits(), flagged.to_bits(), "at {p:?}");
            }
        }
        assert!(near_solid_cells(&[], dims).is_empty());
    }

    #[test]
    fn chamfer_measures_distance() {
        let dims = IVec3::new(9, 9, 9);
        let mut ch = Chamfer::new(dims, IVec3::ZERO, dims - 1);
        let centre = (4 + 9 * (4 + 9 * 4)) as usize;
        while !ch.is_done() {
            ch.step_slice(|i| i == centre);
        }
        assert_eq!(ch.get(IVec3::splat(4)), 0.0);
        assert_eq!(ch.get(IVec3::new(7, 4, 4)), 3.0);
        assert!((ch.get(IVec3::new(6, 6, 4)) - 2.0 * core::f32::consts::SQRT_2).abs() < 1e-5);
    }
}
