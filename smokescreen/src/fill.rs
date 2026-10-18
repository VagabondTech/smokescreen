//! The flood fill, run a little at a time.
//!
//! A fill is a Dijkstra expansion over a voxel grid around the grenade,
//! followed (for faceted clouds) by a halo, two distance transforms, puff
//! seeding and binning. A fill job runs those stages as a state machine that
//! can stop after any unit of work and pick up where it left off. Stopping
//! never changes what the next unit does, so the finished cloud is the same
//! however the work was split across ticks.

use std::{cmp::Reverse, collections::BinaryHeap, sync::OnceLock};

use glam::{IVec3, UVec3, Vec3};

use crate::{
    facets::{BinBuilder, Bins, FacetSettings, POP_OVERSHOOT, Polyhedron, Puff, ShapeMix},
    grid::{BRICK, Chamfer, EMPTY_VOXEL, FILLED_MAX, Grid, REGION_RANGE, encode_region, occupancy},
    level::Level,
    math::{hash01, sin_cos, value_noise_corners, value_noise_lerp},
    units::Units,
};

/// Where a fill starts spreading from, besides its origin. Metres, internal
/// frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Source {
    /// From the origin.
    Point,
    /// From every point of the segment origin..end.
    Line { end: Vec3 },
    /// From every open voxel in the box centred on the origin.
    Box { half: Vec3 },
}

/// Parameters of one flood fill, in metres (internal frame).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FillSpec {
    pub origin: Vec3,
    pub source: Source,
    pub dims: UVec3,
    pub voxel: f32,
    /// The voxel size in the caller's units. The level is asked about the
    /// voxels of the caller's lattice of this size, at exactly the points
    /// and sizes [`crate::VoxelObstacles`] asks about, so one built at this
    /// size answers exactly as its source would.
    pub level_voxel: f32,
    pub volume: f32,
    pub up_cost: f32,
    pub down_cost: f32,
    pub lumpiness: f32,
    pub lump_size: f32,
    pub floor_fraction: f32,
    pub seed: u32,
    /// Faceted and crystal clouds: puff settings in metres.
    pub facets: Option<FacetSettings>,
    /// Crystal puffs: unblended, and kept entirely inside the cloud's space.
    pub crystal: bool,
}

/// A finished fill: everything about a cloud that doesn't change over its
/// life. Internal frame (metres, Y up). Immutable and cheap to share.
#[derive(Clone, Debug, PartialEq)]
pub struct CloudShape {
    pub grid: Grid,
    /// Faceted clouds only.
    pub facets: Option<Facets>,
}

/// The puffs of a faceted cloud.
#[derive(Clone, Debug, PartialEq)]
pub struct Facets {
    pub puffs: Vec<Puff>,
    pub bins: Bins,
    /// The settings the puffs were built with, in metres. Spacing and size
    /// grow if a big cloud would otherwise need more puffs than allowed.
    pub settings: FacetSettings,
}

impl CloudShape {
    /// Box containing every point the cloud can ever occupy.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        match &self.facets {
            Some(f) => (f.bins.min, f.bins.max()),
            None => (self.grid.bounds_min, self.grid.bounds_max),
        }
    }

    /// No smoke at all: the grenade found no open space (or, faceted, no
    /// puff fitted).
    pub fn is_empty(&self) -> bool {
        self.grid.is_empty() || self.facets.as_ref().is_some_and(|f| f.puffs.is_empty())
    }
}

/// Work units, calibrated to about 10 ns each on a desktop CPU (plus
/// whatever the level costs per query, see
/// [`crate::Config::level_query_units`]).
pub(crate) mod cost {
    /// Allocating and clearing a grid, per 64 voxels.
    pub const ALLOC_PER_64: i64 = 4;
    /// Scanning the grid, per 64 voxels.
    pub const SCAN_PER_64: i64 = 8;
    pub const SETTLE: i64 = 48;
    /// Per voxel the halo expands.
    pub const HALO_EXPAND: i64 = 15;
    /// Per eight voxels of a chamfer slice being seeded, and per four of
    /// one being relaxed.
    pub const CHAMFER_SEED_PER_8: i64 = 1;
    pub const CHAMFER_RELAX_PER_4: i64 = 5;
    /// Per two voxels.
    pub const ENCODE_PAIR: i64 = 3;
    pub const SEED_VOXEL: i64 = 1;
    /// Per puff, per pass of binning.
    pub const BIN_PUFF: i64 = 80;
    /// Per filled voxel folded into the bricks (up to eight of them).
    pub const BRICK_VOXEL: i64 = 8;
    /// Padding: per filled voxel next to a wall, per clipped voxel found
    /// (asking about its neighbours), per clipped voxel checked for whether
    /// it is safe to pad, and per clipped voxel per pass of deciding.
    pub const PAD_SCAN: i64 = 4;
    pub const PAD_FOUND: i64 = 12;
    pub const PAD_SAFE: i64 = 12;
    pub const PAD_SOLVE: i64 = 2;
    /// Per padded voxel, marking it and the hollow voxels around it.
    pub const PAD_FINISH: i64 = 30;

    /// The next slice of a distance transform.
    pub fn chamfer(c: &crate::grid::Chamfer) -> i64 {
        let n = c.slice_len() as i64;
        if c.is_seeding() {
            n * CHAMFER_SEED_PER_8 / 8
        } else {
            n * CHAMFER_RELAX_PER_4 / 4
        }
    }

    /// Rough totals, for scheduling only ([`super::FillJob::remaining`]),
    /// measured with the default tuning: level queries per voxel the spread
    /// settles, and the work after the spread per ten grid voxels.
    pub const QUERIES_PER_SETTLE: i64 = 2;
    pub const REST_PER_10_VOXELS: i64 = 14;
    pub const FACETED_REST_PER_10_VOXELS: i64 = 100;
}

/// Puffs binned per unit of work.
const BIN_CHUNK: usize = 16;

/// Filled voxels folded into the bricks per unit of work.
const BRICK_CHUNK: usize = 512;

/// Voxels per piece of work in the padding's later stages.
const PAD_CHUNK: usize = 256;

/// Per-voxel flags of the padding (see `FillJob::pad_scan`).
mod pad {
    /// The level was asked whether the voxel's centre is inside it.
    pub const ASKED: u8 = 1;
    /// It is.
    pub const INSIDE: u8 = 2;
    /// Solid, its centre open air, and next to a filled voxel: it may
    /// hold smoke.
    pub const CLIPPED: u8 = 4;
    /// Bits 3 and 4: the round that padded it (1 to 3), or 0.
    pub const ROUND_SHIFT: u8 = 3;
    pub const ROUND_MASK: u8 = 3 << ROUND_SHIFT;
    /// Smoke in it could show past a wall: never padded.
    pub const UNSAFE: u8 = 32;
    /// Clipped, and beside open air the smoke can't reach (past a wall):
    /// its centre may be past the wall too, so nothing next to it is padded.
    pub const FAR: u8 = 64;
}

/// The six face neighbours.
const FACES: [IVec3; 6] = [IVec3::X, IVec3::NEG_X, IVec3::Y, IVec3::NEG_Y, IVec3::Z, IVec3::NEG_Z];

/// The 3×3×3 block of voxels around one, as the bits `x + 3y + 9z` (each
/// from 0 to 2; the voxel itself is bit 13).
mod block {
    /// The voxels with `axis` (0 to 2: x, y, z) equal to `value`.
    const fn plane(axis: usize, value: u32) -> u32 {
        let mut mask = 0;
        let mut n = 0;
        while n < 27 {
            if [n % 3, (n / 3) % 3, n / 9][axis] == value {
                mask |= 1 << n;
            }
            n += 1;
        }
        mask
    }

    const ALL: u32 = (1 << 27) - 1;
    const X0: u32 = plane(0, 0);
    const X2: u32 = plane(0, 2);
    const Y0: u32 = plane(1, 0);
    const Y2: u32 = plane(1, 2);

    /// `m` and its face neighbours within the block.
    pub const fn spread(m: u32) -> u32 {
        m | ((m << 1) & !X0) | ((m >> 1) & !X2) | ((m << 3) & !Y0) | ((m >> 3) & !Y2) | ((m << 9) & ALL) | (m >> 9)
    }

    /// The voxels of `open` reached from `from` (a subset of it) by face
    /// steps through `open`.
    pub fn reach(from: u32, open: u32) -> u32 {
        let mut near = from;
        loop {
            let next = spread(near) & open;
            if next == near {
                return near;
            }
            near = next;
        }
    }

    /// The offset of bit `n` from the block's centre.
    pub fn offset(n: i32) -> glam::IVec3 {
        glam::IVec3::new(n % 3, (n / 3) % 3, n / 9) - 1
    }

    /// The centre's six face neighbours.
    pub const FACES: u32 = 1 << 4 | 1 << 10 | 1 << 12 | 1 << 14 | 1 << 16 | 1 << 22;
}

/// A budget of work units.
pub(crate) struct Work {
    left: i64,
    full: i64,
    spent: i64,
    level_query: i64,
}

impl Work {
    pub fn new(units: i64, level_query: i64) -> Self {
        Self {
            left: units,
            full: units,
            spent: 0,
            level_query,
        }
    }

    pub fn unlimited() -> Self {
        Self::new(i64::MAX / 2, 0)
    }

    #[inline]
    fn spend(&mut self, n: i64) {
        self.left -= n;
        self.spent += n;
    }

    /// Spend `n` units on a piece of work that can't be split, if it fits
    /// in what is left or nothing has been spent yet (a piece bigger than
    /// the whole budget has to run some time). Otherwise give up the rest of
    /// the budget, so the piece runs first thing next time: a budget is
    /// never overrun by more than one small step.
    #[must_use]
    fn begin(&mut self, n: i64) -> bool {
        if n <= self.left || self.left >= self.full {
            self.spend(n);
            true
        } else {
            self.left = 0;
            false
        }
    }

    #[inline]
    fn exhausted(&self) -> bool {
        self.left <= 0
    }

    pub fn left(&self) -> i64 {
        self.left
    }
}

/// One of the 26 neighbour steps, with the cells that must be open so the
/// fill can't squeeze diagonally between two walls.
struct Step {
    offset: IVec3,
    /// Bits (indices into the 26 steps) of the sub-steps that must be open.
    guard_bits: u32,
    /// Euclidean length of `offset`.
    length: f32,
}

fn steps() -> &'static [Step; 26] {
    static STEPS: OnceLock<[Step; 26]> = OnceLock::new();
    STEPS.get_or_init(|| {
        let mut out: Vec<Step> = Vec::with_capacity(26);
        let mut guard_lists: Vec<Vec<IVec3>> = Vec::with_capacity(26);
        let mut offsets: Vec<IVec3> = Vec::with_capacity(26);
        for z in -1..=1 {
            for y in -1..=1 {
                for x in -1..=1 {
                    if (x, y, z) != (0, 0, 0) {
                        offsets.push(IVec3::new(x, y, z));
                    }
                }
            }
        }
        for z in -1..=1 {
            for y in -1..=1 {
                for x in -1..=1 {
                    let offset = IVec3::new(x, y, z);
                    if offset == IVec3::ZERO {
                        continue;
                    }
                    // Every proper sub-step of a diagonal move must be open.
                    let mut guards = [IVec3::ZERO; 6];
                    let mut guard_count = 0;
                    let axes = [IVec3::new(x, 0, 0), IVec3::new(0, y, 0), IVec3::new(0, 0, z)];
                    for mask in 1u8..7 {
                        let sub = (0..3)
                            .filter(|i| mask & (1 << i) != 0)
                            .fold(IVec3::ZERO, |acc, i| acc + axes[i]);
                        if sub != IVec3::ZERO && sub != offset && !guards[..guard_count].contains(&sub) {
                            guards[guard_count] = sub;
                            guard_count += 1;
                        }
                    }
                    out.push(Step {
                        offset,
                        guard_bits: 0,
                        length: offset.as_vec3().length(),
                    });
                    guard_lists.push(guards[..guard_count].to_vec());
                }
            }
        }
        // Every guard is itself one of the 26 steps.
        for (step, guards) in out.iter_mut().zip(&guard_lists) {
            for g in guards {
                let k = offsets.iter().position(|o| o == g).unwrap_or(0);
                step.guard_bits |= 1 << k;
            }
        }
        match out.try_into() {
            Ok(steps) => steps,
            Err(_) => unreachable!("26 neighbours"),
        }
    })
}

/// Heap keys: integer distances keep the expansion order (and so the shape)
/// identical on every machine, ties broken by voxel index.
#[inline]
fn key(d: f32) -> u32 {
    (d * 4096.0) as u32
}

type Heap = BinaryHeap<Reverse<(u32, u32)>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Start,
    Spread,
    Finish,
    PadStart,
    PadScan,
    PadSolve,
    PadFinish,
    Bricks,
    HaloStart,
    Halo,
    RegionStart,
    RegionInside,
    RegionOutside,
    RegionEncode,
    AirStart,
    Air,
    Seed,
    Bin,
    Done,
}

/// The level, seen in the internal frame.
struct Probe<'a, L: Level + ?Sized> {
    level: &'a L,
    units: &'a Units,
}

/// A flood fill in progress.
pub(crate) struct FillJob {
    spec: FillSpec,
    stage: Stage,
    dims: IVec3,
    min: Vec3,
    /// The lattice index of voxel (0, 0, 0): `min` is this times `v`.
    first: IVec3,
    v: f32,
    count: usize,
    occupancy: Vec<u8>,
    distance: Vec<u8>,
    region: Vec<u8>,
    // Spread.
    dist: Vec<f32>,
    /// Where `dist` (or, during the halo, `halo_dist`) holds a distance.
    reached: Vec<bool>,
    settled: Vec<bool>,
    lumps: Vec<f32>,
    heap: Heap,
    order: Vec<u32>,
    target: usize,
    lengths: [f32; 26],
    /// Index delta of each of the 26 steps (wrapping, for negative ones).
    deltas: [usize; 26],
    /// Corner hashes of the lump noise's last few lattice cells.
    noise_cache: [(IVec3, [f32; 8]); 4],
    noise_next: usize,
    /// Units spent so far, for [`Self::remaining`].
    spent: i64,
    /// Every voxel the level was asked about lies in this box.
    asked: (IVec3, IVec3),
    /// Volumetric fills: filled voxels with a solid neighbour, and which of
    /// the 26 neighbours are solid (one bit per step), in the order the
    /// spread settled them.
    walled: Vec<(u32, u32)>,
    // Padding (volumetric fills, see `pad_scan`).
    /// Per voxel: [`pad`] flags.
    pad: Vec<u8>,
    /// Solid voxels next to the smoke whose centres are open air, in the
    /// order they were found.
    clipped: Vec<u32>,
    /// The ones that hold smoke, in the same order.
    padded: Vec<u32>,
    /// Where the current padding stage is up to: in `walled`, `clipped`
    /// or `padded`.
    pad_cursor: usize,
    /// 0: which clipped voxels are safe to pad; 1: padding them from the
    /// voxels the spread filled; 2 and 3: into concave edges, then corners.
    pad_round: u8,
    /// What the current round pads, once it is done.
    pad_found: Vec<(u32, u8)>,
    // Results of the spread (and the padding).
    filled_min: IVec3,
    filled_max: IVec3,
    bounds: (Vec3, Vec3),
    // Halo.
    in_region: Vec<bool>,
    halo_dist: Vec<f32>,
    /// Voxels waiting to be expanded, in the order they were queued, and the
    /// next one to take.
    halo_queue: Vec<u32>,
    halo_next: usize,
    /// Is the voxel queued and not yet taken?
    halo_queued: Vec<bool>,
    halo_limit: f32,
    /// Every voxel in the halo (the fill included) lies in this box.
    halo_box: (IVec3, IVec3),
    // Region distance transform.
    region_box: (IVec3, IVec3),
    to_inside: Option<Chamfer>,
    to_outside: Option<Chamfer>,
    encode_z: i32,
    // Puffs.
    to_air: Option<Chamfer>,
    settings: FacetSettings,
    attempt: u32,
    cell_cursor: IVec3,
    candidates: Vec<(IVec3, u8, f32)>,
    puffs: Vec<Puff>,
    bin_cell: f32,
    bin_attempt: u32,
    binner: Option<BinBuilder>,
    bins: Option<Bins>,
    bricks_done: Vec<u8>,
    /// How many filled (then padded) voxels the bricks stage has folded in.
    brick_cursor: usize,
    front: Vec<Option<(Vec3, Vec3)>>,
}

impl FillJob {
    pub fn new(spec: FillSpec) -> Self {
        let dims = spec.dims.max(UVec3::splat(4)).as_ivec3();
        let count = (dims.x * dims.y * dims.z) as usize;
        let v = spec.voxel;
        // Align the grid to the world voxel lattice.
        let offset = Vec3::new(
            dims.x as f32 * 0.5,
            dims.y as f32 * spec.floor_fraction,
            dims.z as f32 * 0.5,
        ) * v;
        // Centred on what the cloud spreads from.
        let center = match spec.source {
            Source::Line { end } => (spec.origin + end) * 0.5,
            Source::Point | Source::Box { .. } => spec.origin,
        };
        let first = ((center - offset) / v).floor();
        let min = first * v;
        let settings = spec.facets.unwrap_or_else(FacetSettings::metric);
        let target = ((spec.volume / (v * v * v)).round() as usize).clamp(1, count);
        Self {
            stage: Stage::Start,
            dims,
            min,
            first: first.as_ivec3(),
            v,
            count,
            occupancy: Vec::new(),
            distance: Vec::new(),
            region: Vec::new(),
            dist: Vec::new(),
            reached: Vec::new(),
            settled: Vec::new(),
            lumps: Vec::new(),
            heap: Heap::new(),
            order: Vec::new(),
            target,
            lengths: [0.0; 26],
            deltas: std::array::from_fn(|k| {
                let o = steps()[k].offset;
                (o.x as isize + (dims.x as isize) * (o.y as isize + (dims.y as isize) * o.z as isize)) as usize
            }),
            noise_cache: [(IVec3::MAX, [0.0; 8]); 4],
            noise_next: 0,
            spent: 0,
            asked: (IVec3::MAX, IVec3::MIN),
            walled: Vec::new(),
            pad: Vec::new(),
            clipped: Vec::new(),
            padded: Vec::new(),
            pad_cursor: 0,
            pad_round: 0,
            pad_found: Vec::new(),
            filled_min: IVec3::MAX,
            filled_max: IVec3::MIN,
            bounds: (spec.origin, spec.origin),
            in_region: Vec::new(),
            halo_dist: Vec::new(),
            halo_queue: Vec::new(),
            halo_next: 0,
            halo_queued: Vec::new(),
            halo_limit: 0.0,
            halo_box: (IVec3::MAX, IVec3::MIN),
            region_box: (IVec3::ZERO, IVec3::ZERO),
            to_inside: None,
            to_outside: None,
            encode_z: 0,
            to_air: None,
            settings,
            attempt: 0,
            cell_cursor: IVec3::ZERO,
            candidates: Vec::new(),
            puffs: Vec::new(),
            bin_cell: 0.0,
            bin_attempt: 0,
            binner: None,
            bins: None,
            bricks_done: Vec::new(),
            brick_cursor: 0,
            front: Vec::new(),
            spec,
        }
    }

    pub fn is_done(&self) -> bool {
        self.stage == Stage::Done
    }

    /// Roughly how many units are left, for scheduling only: it never
    /// changes the result.
    pub(crate) fn remaining(&self, level_query: i64) -> i64 {
        let spread = self.target as i64 * (cost::SETTLE + cost::QUERIES_PER_SETTLE * level_query);
        let rest = if self.spec.facets.is_some() {
            cost::FACETED_REST_PER_10_VOXELS
        } else {
            cost::REST_PER_10_VOXELS
        };
        (spread + self.count as i64 * rest / 10 - self.spent).max(0)
    }

    #[inline]
    fn index(&self, c: IVec3) -> usize {
        (c.x + self.dims.x * (c.y + self.dims.y * c.z)) as usize
    }

    #[inline]
    fn decode(&self, i: usize) -> IVec3 {
        let i = i as i32;
        let plane = self.dims.x * self.dims.y;
        IVec3::new(i % self.dims.x, (i % plane) / self.dims.x, i / plane)
    }

    #[inline]
    fn center(&self, c: IVec3) -> Vec3 {
        self.min + (c.as_vec3() + 0.5) * self.v
    }

    /// A voxel's centre in the caller's units and frame, as the level is
    /// asked about it: from its index on the world lattice, so it is the
    /// very point [`crate::VoxelObstacles`] asks its source about.
    #[inline]
    fn level_center<L: Level + ?Sized>(&self, c: IVec3, probe: &Probe<L>) -> Vec3 {
        let lattice = ((self.first + c).as_vec3() + 0.5) * self.spec.level_voxel;
        probe.units.dir_from_internal(lattice)
    }

    /// Is a voxel solid? The outermost layer always is, so trilinear
    /// filtering in a texture atlas never bleeds into a neighbouring slot.
    fn solid<L: Level + ?Sized>(&mut self, c: IVec3, probe: &Probe<L>, work: &mut Work) -> bool {
        if !(c.cmpge(IVec3::ONE).all() && c.cmple(self.dims - 2).all()) {
            return true;
        }
        let i = self.index(c);
        match self.occupancy[i] {
            occupancy::OPEN => false,
            occupancy::SOLID | occupancy::PADDED => true,
            _ => {
                work.spend(work.level_query);
                let center = self.level_center(c, probe);
                let solid = probe.level.is_blocked(center, self.spec.level_voxel * 0.5);
                self.occupancy[i] = if solid { occupancy::SOLID } else { occupancy::OPEN };
                self.asked = (self.asked.0.min(c), self.asked.1.max(c));
                solid
            }
        }
    }

    /// Are all 26 neighbours of `c` inside the interior (never in the
    /// outermost layer)? Then they can be reached by index deltas alone.
    #[inline]
    fn neighbours_interior(&self, c: IVec3) -> bool {
        c.cmpge(IVec3::splat(2)).all() && c.cmple(self.dims - 3).all()
    }

    /// Which of the 26 neighbours of `c` are solid, one bit per step.
    fn solid_mask<L: Level + ?Sized>(&mut self, c: IVec3, probe: &Probe<L>, work: &mut Work) -> u32 {
        let mut mask = 0;
        if self.neighbours_interior(c) {
            // The common case: every neighbour is a known byte at a fixed
            // offset, no bounds checks; only unknown ones ask the level.
            let i = self.index(c);
            let deltas = self.deltas;
            for (k, (s, &delta)) in steps().iter().zip(&deltas).enumerate() {
                let ni = i.wrapping_add(delta);
                let solid = match self.occupancy[ni] {
                    occupancy::OPEN => false,
                    occupancy::SOLID | occupancy::PADDED => true,
                    _ => self.solid(c + s.offset, probe, work),
                };
                mask |= (solid as u32) << k;
            }
            return mask;
        }
        for (k, s) in steps().iter().enumerate() {
            if self.solid(c + s.offset, probe, work) {
                mask |= 1 << k;
            }
        }
        mask
    }

    /// Do some work. Returns `true` once the fill is finished.
    pub fn step<L: Level + ?Sized>(&mut self, level: &L, units: &Units, work: &mut Work) -> bool {
        let probe = Probe { level, units };
        let spent = work.spent;
        while !work.exhausted() && self.stage != Stage::Done {
            match self.stage {
                Stage::Start => self.start(&probe, work),
                Stage::Spread => self.spread(&probe, work),
                Stage::Finish => self.finish(work),
                Stage::PadStart => self.pad_start(work),
                Stage::PadScan => self.pad_scan(&probe, work),
                Stage::PadSolve => self.pad_solve(work),
                Stage::PadFinish => self.pad_finish(work),
                Stage::Bricks => self.bricks(work),
                Stage::HaloStart => self.halo_start(work),
                Stage::Halo => self.halo(&probe, work),
                Stage::RegionStart => self.region_start(work),
                Stage::RegionInside | Stage::RegionOutside => self.region_chamfer(work),
                Stage::RegionEncode => self.region_encode(work),
                Stage::AirStart => self.air_start(work),
                Stage::Air => self.air(work),
                Stage::Seed => self.seed(work),
                Stage::Bin => self.bin(work),
                Stage::Done => {}
            }
        }
        self.spent += work.spent - spent;
        self.stage == Stage::Done
    }

    fn start<L: Level + ?Sized>(&mut self, probe: &Probe<L>, work: &mut Work) {
        let count = self.count;
        // The spread's five grids.
        if !work.begin(count as i64 / 64 * cost::ALLOC_PER_64 * 5) {
            return;
        }
        // Zeroed allocations: pages are only touched as the fill reaches them.
        self.occupancy = vec![occupancy::UNKNOWN; count];

        let seeds = self.seeds(probe, work);
        if seeds.is_empty() {
            // Nowhere to go: an empty cloud.
            self.stage = Stage::Done;
            return;
        }

        for (len, s) in self.lengths.iter_mut().zip(steps()) {
            // Step lengths with the vertical anisotropy baked in.
            let o = s.offset.as_vec3();
            let vertical = if o.y > 0.0 {
                self.spec.up_cost
            } else if o.y < 0.0 {
                self.spec.down_cost
            } else {
                1.0
            };
            *len = Vec3::new(o.x, o.y * vertical, o.z).length();
        }
        // `dist` is only meaningful where `reached`; a lump of 0 means not
        // computed yet (real lumps are at least 0.05).
        self.dist = vec![0.0; count];
        self.reached = vec![false; count];
        self.settled = vec![false; count];
        self.lumps = vec![0.0; count];
        self.order = Vec::with_capacity(self.target);
        for (c, d) in seeds {
            let si = self.index(c);
            self.dist[si] = d;
            self.reached[si] = true;
            self.heap.push(Reverse((key(d), si as u32)));
        }
        self.stage = Stage::Spread;
    }

    /// The open voxels the fill spreads from, with their starting distance
    /// (in voxels), in a fixed order, no repeats.
    fn seeds<L: Level + ?Sized>(&mut self, probe: &Probe<L>, work: &mut Work) -> Vec<(IVec3, f32)> {
        let interior = |c: IVec3, dims: IVec3| c.clamp(IVec3::ONE, dims - 2);
        let voxel_of = |p: Vec3, min: Vec3, v: f32| ((p - min) / v).floor().as_ivec3();
        let origin = self.spec.origin;
        let mut out = Vec::new();
        match self.spec.source {
            Source::Point => {
                // The grenade usually rests on the floor, so the voxel
                // containing it may be solid. Try upwards, then any neighbour.
                let start = interior(voxel_of(origin, self.min, self.v), self.dims);
                for dy in 0..4 {
                    let c = start + IVec3::new(0, dy, 0);
                    if !self.solid(c, probe, work) {
                        out.push((c, 0.0));
                        return out;
                    }
                }
                for s in steps() {
                    let c = start + s.offset;
                    if !self.solid(c, probe, work) {
                        out.push((c, 0.0));
                        return out;
                    }
                }
            }
            Source::Line { end } => {
                let n = ((end - origin).length() / (self.v * 0.5)).ceil().max(1.0) as u32;
                for i in 0..=n {
                    let p = origin + (end - origin) * (i as f32 / n as f32);
                    let c = interior(voxel_of(p, self.min, self.v), self.dims);
                    if !out.iter().any(|(o, _)| *o == c) && !self.solid(c, probe, work) {
                        out.push((c, 0.0));
                    }
                }
            }
            Source::Box { half } => {
                // Rounded: each voxel starts as far out as it lies across the
                // box (ellipsoidally), so the box fills from the middle and
                // its edges feather like any other cloud's.
                let half = half.max(Vec3::splat(self.v * 0.5));
                let scale = half.min_element() / self.v;
                let lo = interior(voxel_of(origin - half, self.min, self.v), self.dims);
                let hi = interior(voxel_of(origin + half, self.min, self.v), self.dims);
                for z in lo.z..=hi.z {
                    for y in lo.y..=hi.y {
                        for x in lo.x..=hi.x {
                            let c = IVec3::new(x, y, z);
                            if !self.solid(c, probe, work) {
                                let across = (self.center(c) - origin) / half;
                                out.push((c, across.length() * scale));
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Signed distance (metres) to the edge of the fill's region (fill plus
    /// halo), trilinearly sampled. Valid once the region is encoded.
    fn region_at(&self, p: Vec3) -> f32 {
        crate::grid::decode_region(crate::grid::sample_channel(
            &self.region,
            self.min,
            self.v,
            self.dims.as_uvec3(),
            p,
        ))
    }

    /// [`value_noise`] of the lumps, with the eight corner hashes of the last
    /// few lattice cells kept: neighbouring voxels nearly always share a
    /// cell, and the lerps are the same whoever computed the corners.
    #[inline]
    fn lump_noise(&mut self, p: Vec3) -> f32 {
        let i = p.floor();
        let cell = i.as_ivec3();
        let f = p - i;
        let slot = match self.noise_cache.iter().position(|(c, _)| *c == cell) {
            Some(slot) => slot,
            None => {
                let slot = self.noise_next;
                self.noise_next = (slot + 1) % self.noise_cache.len();
                self.noise_cache[slot] = (cell, value_noise_corners(cell, self.spec.seed));
                slot
            }
        };
        value_noise_lerp(&self.noise_cache[slot].1, f)
    }

    fn spread<L: Level + ?Sized>(&mut self, probe: &Probe<L>, work: &mut Work) {
        let all = steps();
        while !work.exhausted() {
            let Some(Reverse((_, idx))) = self.heap.pop() else {
                break;
            };
            let i = idx as usize;
            if self.settled[i] {
                continue;
            }
            self.settled[i] = true;
            self.order.push(idx);
            work.spend(cost::SETTLE);
            let c = self.decode(i);
            // Solid neighbours, and diagonal moves squeezing between two
            // walls, are out. Asked for the last voxel too, so every solid
            // voxel next to the smoke is known: samples skip them (see
            // `Grid::sample_distance`).
            let solid = self.solid_mask(c, probe, work);
            if solid != 0 && self.spec.facets.is_none() {
                self.walled.push((idx, solid));
            }
            if self.order.len() >= self.target {
                break;
            }
            let d = self.dist[i];
            for (k, s) in all.iter().enumerate() {
                // Solid neighbours are never relaxed, and the outermost
                // layer is always solid, so a step taken stays inside the
                // grid: the index delta is exact.
                if solid & (1 << k | s.guard_bits) != 0 {
                    continue;
                }
                let ni = i.wrapping_add(self.deltas[k]);
                if self.settled[ni] {
                    continue;
                }
                // The lumpiness noise is evaluated once per voxel, not per edge.
                if self.lumps[ni] == 0.0 {
                    let n = c + s.offset;
                    let noise = self.lump_noise(self.center(n) / self.spec.lump_size.max(0.01));
                    self.lumps[ni] = (1.0 + self.spec.lumpiness * (2.0 * noise - 1.0)).max(0.05);
                }
                let nd = d + self.lengths[k] * self.lumps[ni];
                if !self.reached[ni] || nd < self.dist[ni] {
                    self.dist[ni] = nd;
                    self.reached[ni] = true;
                    self.heap.push(Reverse((key(nd), ni as u32)));
                }
            }
        }
        if work.exhausted() && !self.heap.is_empty() && self.order.len() < self.target {
            return;
        }
        self.heap.clear();
        self.stage = Stage::Finish;
    }

    fn finish(&mut self, work: &mut Work) {
        if !work.begin(self.order.len() as i64 * 3 + self.count as i64 / 64 * cost::SCAN_PER_64) {
            return;
        }
        self.distance = vec![EMPTY_VOXEL; self.count];
        let d_max = self
            .order
            .iter()
            .map(|&i| self.dist[i as usize])
            .fold(0.0_f32, f32::max)
            .max(1e-3);
        let (mut cmin, mut cmax) = (IVec3::MAX, IVec3::MIN);
        for k in 0..self.order.len() {
            let i = self.order[k] as usize;
            let c = self.decode(i);
            cmin = cmin.min(c);
            cmax = cmax.max(c);
            let norm = (self.dist[i] / d_max).clamp(0.0, 1.0);
            self.distance[i] = (norm * FILLED_MAX * 255.0).round() as u8;
        }
        self.filled_min = cmin;
        self.filled_max = cmax;
        self.dist = Vec::new();
        self.reached = Vec::new();
        self.lumps = Vec::new();
        if self.walled.is_empty() {
            // Faceted (their puffs are clipped by the region instead), or
            // nowhere near a wall.
            self.close();
        } else {
            self.stage = Stage::PadStart;
        }
    }

    /// Bounds, front bounds and bricks of the filled (and padded) voxels.
    fn close(&mut self) {
        let voxels = self.order.iter().chain(&self.padded);
        self.front = crate::grid::front_bounds(
            self.min,
            self.v,
            self.dims.as_uvec3(),
            voxels.map(|&i| (i as usize, self.distance[i as usize])),
        );
        let v = self.v;
        let margin = Vec3::splat(v);
        let lo = self.min + self.filled_min.as_vec3() * v - margin;
        let hi = self.min + (self.filled_max + 1).as_vec3() * v + margin;
        let grid_max = self.min + self.dims.as_vec3() * v;
        self.bounds = (lo.max(self.min), hi.min(grid_max));
        let bd = (self.dims.as_uvec3() + BRICK - 1) / BRICK;
        self.bricks_done = vec![EMPTY_VOXEL; (bd.x * bd.y * bd.z) as usize];
        self.brick_cursor = 0;
        self.stage = Stage::Bricks;
    }

    /// Is a voxel's centre inside the level? Asked once per voxel, of
    /// voxels not in the outermost layer: solid ones, and open ones beside a
    /// clipped voxel that the smoke can't reach.
    fn inside<L: Level + ?Sized>(&mut self, c: IVec3, probe: &Probe<L>, work: &mut Work) -> bool {
        let i = self.index(c);
        let flags = self.pad[i];
        if flags & pad::ASKED != 0 {
            return flags & pad::INSIDE != 0;
        }
        work.spend(work.level_query);
        let inside = probe.level.is_inside(self.level_center(c, probe));
        self.pad[i] |= pad::ASKED | if inside { pad::INSIDE } else { 0 };
        inside
    }

    #[inline]
    fn is_interior(&self, c: IVec3) -> bool {
        c.cmpge(IVec3::ONE).all() && c.cmple(self.dims - 2).all()
    }

    /// Solid (for the fill) with its centre inside the level: a voxel deep
    /// enough in a wall to hold smoke in front of it. Valid once scanned.
    #[inline]
    fn is_deep(&self, c: IVec3) -> bool {
        self.is_interior(c) && self.pad[self.index(c)] & pad::INSIDE != 0
    }

    /// The round that padded a voxel, or 0.
    #[inline]
    fn round_of(&self, c: IVec3) -> u8 {
        if !self.is_interior(c) {
            return 0;
        }
        (self.pad[self.index(c)] & pad::ROUND_MASK) >> pad::ROUND_SHIFT
    }

    fn pad_start(&mut self, work: &mut Work) {
        if !work.begin(self.count as i64 / 64 * cost::ALLOC_PER_64) {
            return;
        }
        self.pad = vec![0; self.count];
        self.pad_cursor = 0;
        self.stage = Stage::PadScan;
    }

    /// Smoke against a wall stops at the middle of the first solid voxel
    /// (samples skip solid voxels). Where the wall only clips that voxel,
    /// its middle is open air in front of the wall, and the smoke would stop
    /// short of the wall, leaving a see-through layer along it. Such
    /// *clipped* voxels take the smoke in front of them, so it reaches into
    /// the wall, unless smoke there could show past the wall (a wall thinner
    /// than a voxel): then the smoke stops at their middle, as before.
    ///
    /// This stage finds the clipped voxels around filled ones, asking the
    /// level about each one's centre, and about all their neighbours: the
    /// cube and centre of each solid one, and the centre of each open one
    /// the smoke can't reach. `pad_solve` decides from those answers, and
    /// samples near a padded voxel read them.
    fn pad_scan<L: Level + ?Sized>(&mut self, probe: &Probe<L>, work: &mut Work) {
        let all = steps();
        while !work.exhausted() && self.pad_cursor < self.walled.len() {
            let (idx, mask) = self.walled[self.pad_cursor];
            self.pad_cursor += 1;
            work.spend(cost::PAD_SCAN);
            let a = self.decode(idx as usize);
            for (k, s) in all.iter().enumerate() {
                if mask & (1 << k) == 0 {
                    continue;
                }
                // Solid (the mask says so). The outermost layer never holds
                // smoke.
                let c = a + s.offset;
                if !self.is_interior(c) || self.pad[self.index(c)] & pad::CLIPPED != 0 || self.inside(c, probe, work) {
                    continue;
                }
                let i = self.index(c);
                self.pad[i] |= pad::CLIPPED;
                self.clipped.push(i as u32);
                work.spend(cost::PAD_FOUND);
                for t in all {
                    let b = c + t.offset;
                    if self.is_interior(b) && self.solid(b, probe, work) {
                        self.inside(b, probe, work);
                    }
                }
                // Open air beside it that the smoke can't reach: past a
                // wall, or the hollow inside of one. Ask which.
                let [open, filled, ..] = self.block_masks(c);
                let far = open & !block::reach(filled, open);
                for n in 0..27 {
                    let b = c + block::offset(n);
                    if far & (1 << n) != 0 && self.is_interior(b) {
                        self.inside(b, probe, work);
                    }
                }
                let [_, _, hollow, ..] = self.block_masks(c);
                if far & !hollow & block::FACES != 0 {
                    self.pad[i] |= pad::FAR;
                }
            }
        }
        if self.pad_cursor >= self.walled.len() {
            self.walled = Vec::new();
            self.pad_cursor = 0;
            self.pad_round = 0;
            self.stage = Stage::PadSolve;
        }
    }

    /// The 3×3×3 block around a clipped voxel, as masks of [`block`] bits:
    /// open voxels, filled ones, open ones whose centre the level called
    /// inside (the hollow inside of a wall made of surfaces), stray clipped
    /// ones (solid, centre open air, touching no filled voxel), and clipped
    /// ones beside air the smoke can't reach ([`pad::FAR`]).
    fn block_masks(&self, c: IVec3) -> [u32; 5] {
        let (mut open, mut filled, mut hollow, mut stray, mut far_clipped) = (0u32, 0u32, 0u32, 0u32, 0u32);
        for n in 0..27 {
            let o = block::offset(n);
            if o == IVec3::ZERO {
                continue;
            }
            let b = c + o;
            let bit = 1u32 << n;
            if !self.is_interior(b) {
                // The outermost layer samples as open air.
                open |= bit;
                continue;
            }
            let i = self.index(b);
            let flags = self.pad[i];
            match self.occupancy[i] {
                occupancy::OPEN => {
                    open |= bit;
                    if self.settled[i] {
                        filled |= bit;
                    } else if flags & pad::INSIDE != 0 {
                        hollow |= bit;
                    }
                }
                occupancy::SOLID if flags & pad::INSIDE != 0 => {}
                occupancy::SOLID if flags & pad::CLIPPED != 0 && flags & pad::FAR == 0 => {}
                occupancy::SOLID if flags & pad::CLIPPED != 0 => far_clipped |= bit,
                _ => stray |= bit,
            }
        }
        [open, filled, hollow, stray, far_clipped]
    }

    /// Could smoke in a clipped voxel show past a wall? Samples near it read
    /// its 26 neighbours, so they must all be on the smoke's side: a voxel
    /// inside the wall (solid, or the hollow inside of a wall made of
    /// surfaces), a filled or clipped one, or open air the smoke could reach
    /// without crossing the wall (by face steps through open voxels in the
    /// block, from the filled ones: around the edge of a door frame, say,
    /// but not through a wall thinner than a voxel). A solid voxel whose
    /// centre is open air and that no filled voxel touches must be next to
    /// such air too; otherwise it is the far face of a thin wall. And none
    /// may be a clipped voxel beside air the smoke can't reach: a filled
    /// voxel can touch one along an edge across a thin wall that leans.
    fn unsafe_to_pad(&self, c: IVec3) -> bool {
        let [open, filled, hollow, stray, far_clipped] = self.block_masks(c);
        let near = block::reach(filled, open);
        open & !near & !hollow != 0 || stray & !block::spread(near) != 0 || far_clipped != 0
    }

    /// Decide which clipped voxels hold smoke, and how much (the smallest
    /// fill distance among the neighbours it comes from), in rounds, never
    /// one where smoke could show past a wall (`unsafe_to_pad`, round 0):
    ///
    /// 1. Those next to filled voxels.
    /// 2. Concave edges (a wall meeting the floor), whose voxels touch the
    ///    filled ones only along an edge: from round-1 voxels, if each one's
    ///    other side is inside the wall (so padding never creeps along a
    ///    wall's face past the smoke).
    /// 3. Concave corners, from round-2 voxels, the same way.
    ///
    /// Each round decides from the rounds before it only, so the order of
    /// the voxels doesn't matter. Only reads what `pad_scan` asked, in
    /// pieces of [`PAD_CHUNK`] voxels.
    fn pad_solve(&mut self, work: &mut Work) {
        while !work.exhausted() && self.stage == Stage::PadSolve {
            let round = self.pad_round;
            let end = (self.pad_cursor + PAD_CHUNK).min(self.clipped.len());
            let unit = if round == 0 { cost::PAD_SAFE } else { cost::PAD_SOLVE };
            if !work.begin((end - self.pad_cursor) as i64 * unit) {
                return;
            }
            for n in self.pad_cursor..end {
                let i = self.clipped[n] as usize;
                let c = self.decode(i);
                if round == 0 {
                    if self.unsafe_to_pad(c) {
                        self.pad[i] |= pad::UNSAFE;
                    }
                } else if let Some(value) = self.pad_value(c, round) {
                    self.pad_found.push((i as u32, value));
                }
            }
            self.pad_cursor = end;
            if end < self.clipped.len() {
                continue;
            }
            // The round is done.
            self.pad_cursor = 0;
            for k in 0..self.pad_found.len() {
                let (i, value) = self.pad_found[k];
                self.pad[i as usize] |= round << pad::ROUND_SHIFT;
                self.distance[i as usize] = value;
            }
            self.pad_found.clear();
            if round < 3 {
                self.pad_round += 1;
            } else {
                self.padded = self
                    .clipped
                    .iter()
                    .copied()
                    .filter(|&i| self.pad[i as usize] & pad::ROUND_MASK != 0)
                    .collect();
                self.stage = Stage::PadFinish;
            }
        }
    }

    /// The smoke a clipped voxel takes in a round of `pad_solve`, if any.
    fn pad_value(&self, c: IVec3, round: u8) -> Option<u8> {
        if self.pad[self.index(c)] & (pad::ROUND_MASK | pad::UNSAFE) != 0 {
            return None;
        }
        let mut value: Option<u8> = None;
        for d in FACES {
            let from = c - d;
            // Filled voxels are never in the outermost layer.
            let fi = self.index(from);
            let filled = self.settled[fi];
            let source = if round == 1 {
                filled
            } else {
                if filled || (1..round - 1).contains(&self.round_of(from)) {
                    // It had its chance in an earlier round.
                    return None;
                }
                self.round_of(from) == round - 1
            };
            if !source {
                continue;
            }
            if round > 1 && !self.is_deep(c + d) {
                return None;
            }
            let d = self.distance[fi];
            value = Some(value.map_or(d, |v| v.min(d)));
        }
        value
    }

    fn pad_finish(&mut self, work: &mut Work) {
        while !work.exhausted() && self.pad_cursor < self.padded.len() {
            let end = (self.pad_cursor + PAD_CHUNK).min(self.padded.len());
            if !work.begin((end - self.pad_cursor) as i64 * cost::PAD_FINISH) {
                return;
            }
            for k in self.pad_cursor..end {
                let i = self.padded[k] as usize;
                self.occupancy[i] = occupancy::PADDED;
                let c = self.decode(i);
                self.filled_min = self.filled_min.min(c);
                self.filled_max = self.filled_max.max(c);
                // Samples skip the hollow inside of a wall beside it, as
                // they skip a solid one.
                for s in steps() {
                    let b = c + s.offset;
                    if self.is_interior(b) {
                        let j = self.index(b);
                        if self.occupancy[j] == occupancy::OPEN && !self.settled[j] && self.pad[j] & pad::INSIDE != 0 {
                            self.occupancy[j] = occupancy::SOLID;
                        }
                    }
                }
            }
            self.pad_cursor = end;
        }
        if self.pad_cursor < self.padded.len()
            || !work.begin((self.order.len() + self.padded.len()) as i64 * 2 + self.count as i64 / 64)
        {
            return;
        }
        self.pad = Vec::new();
        self.clipped = Vec::new();
        // `padded` is still read by the bricks stage.
        self.close();
    }

    /// Bricks for empty-space skipping in queries. A brick's value is the
    /// smallest distance in it and one voxel around it, and only filled (and
    /// padded) voxels are below [`EMPTY_VOXEL`], so each of those lowers the
    /// up to eight bricks whose padded box holds it, a chunk of them at a
    /// time; the same minimum as scanning every brick's box.
    fn bricks(&mut self, work: &mut Work) {
        let bd = (self.dims.as_uvec3() + BRICK - 1) / BRICK;
        let total = self.order.len() + self.padded.len();
        while !work.exhausted() && self.brick_cursor < total {
            let start = self.brick_cursor;
            let end = (start + BRICK_CHUNK).min(total);
            if !work.begin((end - start) as i64 * cost::BRICK_VOXEL) {
                return;
            }
            let b = BRICK as i32;
            for k in start..end {
                let i = if k < self.order.len() {
                    self.order[k]
                } else {
                    self.padded[k - self.order.len()]
                } as usize;
                let d = self.distance[i];
                let c = self.decode(i);
                // Bricks whose padded box [b·BRICK − 1, b·BRICK + BRICK]
                // holds c: ceil((c − BRICK) / BRICK) ..= floor((c + 1) / BRICK).
                let lo = (c - 1).div_euclid(IVec3::splat(b)).max(IVec3::ZERO);
                let hi = (c + 1).div_euclid(IVec3::splat(b)).min(bd.as_ivec3() - 1);
                for z in lo.z..=hi.z {
                    for y in lo.y..=hi.y {
                        for x in lo.x..=hi.x {
                            let m = &mut self.bricks_done[(x as u32 + bd.x * (y as u32 + bd.y * z as u32)) as usize];
                            *m = (*m).min(d);
                        }
                    }
                }
            }
            self.brick_cursor = end;
        }
        if self.brick_cursor >= total {
            self.padded = Vec::new();
            self.stage = if self.spec.facets.is_some() {
                Stage::HaloStart
            } else {
                Stage::Done
            };
        }
    }

    /// Keep expanding past the fill through free space, by a plain Euclidean
    /// distance: puffs may bulge into that halo but never past it.
    fn halo_start(&mut self, work: &mut Work) {
        if !work.begin(self.count as i64 / 64 * cost::ALLOC_PER_64 + self.order.len() as i64 * 2) {
            return;
        }
        self.halo_box = (self.filled_min, self.filled_max);
        self.halo_limit = self.settings.halo_metres() / self.v;
        self.in_region = self.settled.clone();
        // Every filled voxel is at distance 0.
        self.halo_dist = vec![0.0; self.count];
        self.reached = self.settled.clone();
        self.halo_queued = vec![false; self.count];
        self.halo_queue.clear();
        self.halo_next = 0;
        // The halo grows from the filled voxels with an open face. One whose
        // six face neighbours are all filled reaches nothing sooner than they
        // do: whatever it reaches with a diagonal step, one of them reaches
        // with a shorter step that needs no voxel open that the diagonal
        // didn't. And the spread has asked the level about all its
        // neighbours already.
        let d = self.dims;
        let faces =
            FACES.map(|o| (o.x as isize + (d.x as isize) * (o.y as isize + (d.y as isize) * o.z as isize)) as usize);
        for k in 0..self.order.len() {
            let i = self.order[k] as usize;
            // Filled voxels are never in the outermost layer.
            if faces.iter().any(|&f| !self.settled[i.wrapping_add(f)]) {
                self.halo_queued[i] = true;
                self.halo_queue.push(i as u32);
            }
        }
        self.stage = Stage::Halo;
    }

    /// Expands the queued voxels in turn, and queues a voxel again whenever
    /// a shorter way to it turns up, until nothing within reach is left.
    /// Every voxel ends at the distance a nearest-first search gives it, and
    /// the same voxels are expanded, so the level is asked about the same
    /// ones; only the order differs.
    fn halo<L: Level + ?Sized>(&mut self, probe: &Probe<L>, work: &mut Work) {
        let all = steps();
        while !work.exhausted() {
            let Some(&idx) = self.halo_queue.get(self.halo_next) else {
                self.halo_dist = Vec::new();
                self.reached = Vec::new();
                self.halo_queued = Vec::new();
                self.halo_queue = Vec::new();
                self.stage = Stage::RegionStart;
                return;
            };
            self.halo_next += 1;
            let i = idx as usize;
            self.halo_queued[i] = false;
            work.spend(cost::HALO_EXPAND);
            let c = self.decode(i);
            if !self.in_region[i] {
                self.in_region[i] = true;
                self.halo_box = (self.halo_box.0.min(c), self.halo_box.1.max(c));
            }
            let solid = self.solid_mask(c, probe, work);
            let d = self.halo_dist[i];
            if d + 1.0 > self.halo_limit {
                // Even a straight step would leave the halo: asking the level
                // about the neighbours, which the rest of the fill reads, was
                // all there was to do here.
                continue;
            }
            for (k, s) in all.iter().enumerate() {
                if solid & (1 << k | s.guard_bits) != 0 {
                    continue;
                }
                let ni = i.wrapping_add(self.deltas[k]);
                let nd = d + s.length;
                if nd <= self.halo_limit && (!self.reached[ni] || nd < self.halo_dist[ni]) {
                    self.halo_dist[ni] = nd;
                    self.reached[ni] = true;
                    if !self.halo_queued[ni] {
                        self.halo_queued[ni] = true;
                        self.halo_queue.push(ni as u32);
                    }
                }
            }
        }
    }

    fn region_start(&mut self, work: &mut Work) {
        if !work.begin(self.count as i64 / 64 * cost::ALLOC_PER_64 * 3) {
            return;
        }
        self.region = vec![255; self.count];
        // The region is the fill plus the halo, all of it settled by the halo.
        // A voxel outside the box is at least `margin + 1` voxels from it, so
        // its centre is at least `(margin + 0.5) × v ≥ REGION_RANGE` outside
        // and it encodes as 255, which the region already holds. At least
        // one voxel, so every voxel of the region has its nearest outside
        // voxel in the box.
        let (rmin, rmax) = self.halo_box;
        let margin = (REGION_RANGE / self.v - 0.5).ceil().max(1.0) as i32;
        let lo = (rmin - margin).max(IVec3::ZERO);
        let hi = (rmax + margin).min(self.dims - 1);
        self.region_box = (lo, hi);
        self.to_inside = Some(Chamfer::new(self.dims, lo, hi));
        self.to_outside = Some(Chamfer::new(self.dims, lo, hi));
        self.encode_z = lo.z;
        self.stage = Stage::RegionInside;
    }

    fn region_chamfer(&mut self, work: &mut Work) {
        let inside = self.stage == Stage::RegionInside;
        let chamfer = if inside {
            self.to_inside.as_mut()
        } else {
            self.to_outside.as_mut()
        };
        let Some(chamfer) = chamfer else {
            self.stage = Stage::RegionEncode;
            return;
        };
        let in_region = &self.in_region;
        while !work.exhausted() && !chamfer.is_done() {
            work.spend(cost::chamfer(chamfer));
            if inside {
                chamfer.step_slice(|i| in_region[i]);
            } else {
                chamfer.step_slice(|i| !in_region[i]);
            }
        }
        if chamfer.is_done() {
            self.stage = if inside {
                Stage::RegionOutside
            } else {
                Stage::RegionEncode
            };
        }
    }

    fn region_encode(&mut self, work: &mut Work) {
        let (lo, hi) = self.region_box;
        let (Some(to_inside), Some(to_outside)) = (&self.to_inside, &self.to_outside) else {
            self.stage = Stage::AirStart;
            return;
        };
        let v = self.v;
        while !work.exhausted() && self.encode_z <= hi.z {
            let z = self.encode_z;
            work.spend(((hi.x - lo.x + 1) * (hi.y - lo.y + 1)) as i64 * cost::ENCODE_PAIR / 2);
            for y in lo.y..=hi.y {
                for x in lo.x..=hi.x {
                    let c = IVec3::new(x, y, z);
                    let i = (c.x + self.dims.x * (c.y + self.dims.y * c.z)) as usize;
                    // Voxel centres sit half a voxel from the boundary.
                    let sdf = if self.in_region[i] {
                        -(to_outside.get(c) - 0.5) * v
                    } else {
                        (to_inside.get(c) - 0.5) * v
                    };
                    self.region[i] = encode_region(sdf);
                }
            }
            self.encode_z += 1;
        }
        if self.encode_z > hi.z {
            self.to_inside = None;
            self.to_outside = None;
            self.in_region = Vec::new();
            self.stage = Stage::AirStart;
        }
    }

    /// Distance from each voxel to open air: voxels the fill looked at and
    /// found free but didn't fill. Puffs are sized so they reach at most
    /// `overhang` past the fill into it.
    fn air_start(&mut self, work: &mut Work) {
        work.spend(cost::ALLOC_PER_64);
        if self.order.is_empty() {
            self.stage = Stage::Done;
            return;
        }
        // Over every voxel the fill ever asked the level about.
        self.to_air = Some(Chamfer::new(self.dims, self.asked.0, self.asked.1));
        self.stage = Stage::Air;
    }

    fn air(&mut self, work: &mut Work) {
        let Some(chamfer) = self.to_air.as_mut() else {
            self.stage = Stage::Seed;
            return;
        };
        let (distance, occ) = (&self.distance, &self.occupancy);
        while !work.exhausted() && !chamfer.is_done() {
            work.spend(cost::chamfer(chamfer));
            chamfer.step_slice(|i| distance[i] == EMPTY_VOXEL && occ[i] == occupancy::OPEN);
        }
        if chamfer.is_done() {
            self.attempt = 0;
            self.cell_cursor = IVec3::ZERO;
            self.puffs.clear();
            self.stage = Stage::Seed;
        }
    }

    /// Seed one puff per `spacing`-sized cell of filled voxels. If a big
    /// cloud needs more puffs than allowed, start over with them spaced out
    /// (and grown to match).
    fn seed(&mut self, work: &mut Work) {
        let max_puffs = self.settings.limits.max_puffs;
        let cell = (self.settings.spacing / self.v).round().max(1.0) as i32;
        while !work.exhausted() {
            let c = self.cell_cursor;
            if c.z >= self.dims.z {
                // A pass is complete.
                if self.puffs.len() > max_puffs && self.attempt < 12 {
                    self.attempt += 1;
                    self.settings.spacing *= 1.15;
                    self.settings.size *= 1.15;
                    self.puffs.clear();
                    self.cell_cursor = IVec3::ZERO;
                    return;
                }
                self.puffs.truncate(max_puffs);
                self.to_air = None;
                self.bin_cell = (self.settings.spacing * 0.67).max(self.v);
                self.bin_attempt = 0;
                self.stage = Stage::Bin;
                return;
            }
            work.spend((cell * cell * cell) as i64 * cost::SEED_VOXEL);
            self.seed_cell(c, cell);
            // Next cell: x fastest, then y, then z.
            let mut next = c + IVec3::new(cell, 0, 0);
            if next.x >= self.dims.x {
                next.x = 0;
                next.y += cell;
                if next.y >= self.dims.y {
                    next.y = 0;
                    next.z += cell;
                }
            }
            self.cell_cursor = next;
        }
    }

    fn seed_cell(&mut self, corner: IVec3, cell: i32) {
        let Some(to_air) = &self.to_air else {
            return;
        };
        let s = self.settings;
        let v = self.v;
        self.candidates.clear();
        for z in corner.z..(corner.z + cell).min(self.dims.z) {
            for y in corner.y..(corner.y + cell).min(self.dims.y) {
                for x in corner.x..(corner.x + cell).min(self.dims.x) {
                    let c = IVec3::new(x, y, z);
                    let d = self.distance[(x + self.dims.x * (y + self.dims.y * z)) as usize];
                    if d != EMPTY_VOXEL {
                        self.candidates.push((c, d, to_air.get(c)));
                    }
                }
            }
        }
        if self.candidates.is_empty() {
            return;
        }
        let key = (corner.x as u32).wrapping_mul(0x9e37_79b9)
            ^ (corner.y as u32).wrapping_mul(0x85eb_ca6b)
            ^ (corner.z as u32).wrapping_mul(0xc2b2_ae35)
            ^ self.spec.seed;
        let h = |k: u32| hash01(key ^ k.wrapping_mul(0x27d4_eb2f));
        // Centre the puff on the cell's deepest voxel (furthest from open
        // air), so edge puffs can still be big; ties go to a hashed pick.
        let deepest = self.candidates.iter().map(|c| c.2).fold(0.0_f32, f32::max);
        let is_deep = |c: &(IVec3, u8, f32)| c.2 >= deepest - 0.01;
        let deep_count = self.candidates.iter().filter(|c| is_deep(c)).count();
        let pick = (h(1) * deep_count as f32) as usize % deep_count;
        let Some(&(c, d, air)) = self.candidates.iter().filter(|c| is_deep(c)).nth(pick) else {
            return;
        };
        let jitter = Vec3::new(h(2), h(3), h(4)) * 2.0 - 1.0;
        let center = self.min + (c.as_vec3() + 0.5 + jitter * 0.45) * v;
        // Room to the edge of the cloud's space, with a margin for the
        // distance transform's error and the encoding.
        let room = (-self.region_at(center) * 0.9 - 0.02).max(0.0);
        let arrival = (d as f32 / 255.0 / FILLED_MAX).clamp(0.0, 1.0);
        let size = s.size * (1.0 + s.size_jitter * (2.0 * h(5) - 1.0)) * (1.0 - s.taper * arrival * arrival);
        let shape = match s.shapes {
            ShapeMix::Single(p) => p,
            ShapeMix::Mixed => match (h(6) * 10.0) as u32 {
                0..=4 => Polyhedron::Icosahedron,
                5..=8 => Polyhedron::Dodecahedron,
                _ => Polyhedron::Octahedron,
            },
        };
        // Keep pointy shapes in a mix within the same reach.
        let size = if matches!(s.shapes, ShapeMix::Mixed) {
            size * Polyhedron::Icosahedron.corner_ratio() / shape.corner_ratio()
        } else {
            size
        };
        // Respect the overhang into open air.
        let clearance = ((air - 0.5) * v - jitter.length() * 0.45 * v).max(0.0);
        // A smooth union bulges at most blend/4 past the puffs.
        let max_reach = clearance + s.overhang - s.blend * 0.25;
        let nominal = size;
        let size = size.min(max_reach / (shape.corner_ratio() * POP_OVERSHOOT));
        // Crystals aren't clipped when drawn, so they must fit entirely.
        let size = if self.spec.crystal {
            size.min(room / (shape.corner_ratio() * POP_OVERSHOOT))
        } else {
            size
        };
        if size < nominal * 0.45 {
            // Too close to the edge to matter; neighbours cover it.
            return;
        }
        let z = 2.0 * h(7) - 1.0;
        let (sin_a, cos_a) = sin_cos(h(8) * core::f32::consts::TAU);
        let rxy = (1.0 - z * z).max(0.0).sqrt();
        let axis = Vec3::new(rxy * cos_a, z, rxy * sin_a);
        let spin = s.spin * (0.5 + h(9)) * if h(10) < 0.5 { -1.0 } else { 1.0 };
        self.puffs.push(Puff {
            center,
            radius: size,
            room,
            arrival,
            shape,
            axis,
            spin,
            phase: h(11) * core::f32::consts::TAU,
        });
    }

    /// Bins about two thirds of the puff spacing measured fastest; coarser if
    /// they would not fit the limits (which only costs speed).
    fn bin(&mut self, work: &mut Work) {
        let limits = self.settings.limits;
        let blend = self.settings.blend;
        while !work.exhausted() {
            let builder = self.binner.get_or_insert_with(|| BinBuilder::new(self.bin_cell));
            work.spend(BIN_CHUNK as i64 * cost::BIN_PUFF);
            if !builder.step(&self.puffs, blend, BIN_CHUNK) {
                continue;
            }
            let Some(bins) = self.binner.take().map(BinBuilder::finish) else {
                continue;
            };
            let fits = bins.ranges.len() <= limits.max_cells && bins.indices.len() <= limits.max_indices;
            if fits || self.bin_attempt >= 12 {
                self.bins = Some(bins);
                self.stage = Stage::Done;
                return;
            }
            self.bin_attempt += 1;
            self.bin_cell *= 1.25;
        }
    }

    /// The finished cloud. Call once [`Self::step`] has returned `true`.
    pub fn into_shape(self) -> CloudShape {
        let filled = self.order.len() as u32;
        let empty = filled == 0;
        let dims = self.dims.as_uvec3();
        let count = self.count;
        let mut grid = Grid {
            min: self.min,
            voxel_size: self.v,
            dims,
            distance: if empty || self.distance.is_empty() {
                vec![EMPTY_VOXEL; count]
            } else {
                self.distance
            },
            region: if empty || self.region.is_empty() {
                vec![255; count]
            } else {
                self.region
            },
            occupancy: if empty {
                vec![occupancy::UNKNOWN; count]
            } else {
                self.occupancy
            },
            filled,
            bounds_min: self.bounds.0,
            bounds_max: self.bounds.1,
            bricks: self.bricks_done,
            front_bounds: self.front,
        };
        if grid.bricks.is_empty() {
            grid.build_bricks();
        }
        if grid.front_bounds.is_empty() {
            grid.build_front_bounds();
        }
        let facets = self.spec.facets.map(|_| Facets {
            bins: self.bins.unwrap_or_else(|| Bins::build(&[], self.settings.blend, 1.0)),
            puffs: self.puffs,
            settings: self.settings,
        });
        CloudShape { grid, facets }
    }
}

/// Run a whole fill at once.
pub(crate) fn fill_now<L: Level + ?Sized>(spec: FillSpec, level: &L, units: &Units) -> CloudShape {
    let mut job = FillJob::new(spec);
    job.step(level, units, &mut Work::unlimited());
    job.into_shape()
}

#[cfg(test)]
mod profile {
    use super::*;
    use crate::level::FlatGround;

    /// Time per stage of a fill, and the slowest single steps (run with
    /// `--release --ignored --nocapture`).
    #[test]
    #[ignore = "timing probe"]
    fn stage_times() {
        let mut spec = FillSpec {
            origin: Vec3::ZERO,
            dims: UVec3::new(48, 24, 48),
            voxel: 0.3,
            level_voxel: 0.3,
            volume: 90.0,
            up_cost: 1.4,
            down_cost: 0.9,
            lumpiness: 0.3,
            lump_size: 1.6,
            floor_fraction: 0.25,
            seed: 7,
            facets: Some(FacetSettings::metric()),
            source: Source::Point,
            crystal: false,
        };
        for faceted in [false, true, false, true] {
            spec.facets = faceted.then(FacetSettings::metric);
            let mut job = FillJob::new(spec.clone());
            let level = FlatGround::default();
            let units = Units::METRES_Y_UP;
            // (stage before, stage after, time, units)
            let mut calls: Vec<(Stage, Stage, std::time::Duration, i64)> = Vec::new();
            // FILL_PROFILE_BUDGET=20000 prints every step of a budgeted fill.
            let budget: i64 = std::env::var("FILL_PROFILE_BUDGET")
                .ok()
                .and_then(|b| b.parse().ok())
                .unwrap_or(1);
            loop {
                let mut work = Work::new(budget, 4);
                let before = job.stage;
                let t = std::time::Instant::now();
                let done = job.step(&level, &units, &mut work);
                calls.push((before, job.stage, t.elapsed(), budget - work.left()));
                if budget > 1 {
                    let c = calls[calls.len() - 1];
                    println!(
                        "    {:?} -> {:?}: {:.3} ms, {} units",
                        c.0,
                        c.1,
                        c.2.as_secs_f64() * 1e3,
                        c.3
                    );
                }
                if done {
                    break;
                }
            }
            println!("faceted {faceted}:");
            let mut stages: Vec<Stage> = Vec::new();
            for c in &calls {
                if !stages.contains(&c.0) {
                    stages.push(c.0);
                }
            }
            for stage in stages {
                let (d, u) = calls
                    .iter()
                    .filter(|c| c.0 == stage)
                    .fold((std::time::Duration::ZERO, 0), |(d, u), c| (d + c.2, u + c.3));
                println!(
                    "  {stage:?}: {:.3} ms, {u} units, {:.1} ns/unit",
                    d.as_secs_f64() * 1e3,
                    d.as_secs_f64() * 1e9 / u.max(1) as f64
                );
            }
            calls.sort_by_key(|c| std::cmp::Reverse(c.2));
            for c in calls.iter().take(4) {
                println!(
                    "  slow step: {:?} -> {:?}, {:.3} ms, {} units",
                    c.0,
                    c.1,
                    c.2.as_secs_f64() * 1e3,
                    c.3
                );
            }
        }
    }
}
