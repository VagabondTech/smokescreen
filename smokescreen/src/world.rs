//! All the clouds of a match: pops, holes, ticks and queries.

use std::sync::Arc;

use glam::{UVec3, Vec3};

use crate::{
    chords::{ChordBounds, ChordField, CoreField, CrystalField},
    facets::{FacetField, FacetRule, FacetSettings, LivePuff},
    fill::{CloudShape, FillJob, FillSpec, Source, Work},
    holes::{HOLE_BUFFER, HoleKind, HoleSettings, HoleShape, holes_near},
    level::Level,
    math::{boxes_overlap, clip_segment, distance_to_segment, hash_u32, safe_recip, slab},
    timeline::{DensityTable, Timeline},
    units::Units,
    volume::{VolumeField, VolumeSteps},
};

/// The rules: everything that decides a cloud's shape and what it hides.
/// Every machine in a match must use the same tuning.
///
/// Lengths are in the caller's units, volumes in cubic caller units, times in
/// seconds. Build one with [`Tuning::new`], which converts the defaults to
/// your units.
#[derive(Clone, Debug, PartialEq)]
pub struct Tuning {
    /// Voxels per cloud along each axis (across, up, across). Rounded up to
    /// multiples of 4.
    pub grid_dims: UVec3,
    /// Voxel edge length.
    pub voxel_size: f32,
    /// Default cloud volume. The fill stops once this much is filled, so a
    /// cloud in a corridor runs further than one in the open.
    pub volume: f32,
    /// Cost multiplier for spreading upwards. Above 1 makes the cloud flatter
    /// and wider.
    pub up_cost: f32,
    /// Cost multiplier for spreading downwards. Below 1 lets smoke pour off
    /// ledges.
    pub down_cost: f32,
    /// Amplitude (0..1) of the low-frequency noise on spreading costs, which
    /// makes the outline lumpy instead of a perfect dome.
    pub lumpiness: f32,
    /// Size of the lumps.
    pub lump_size: f32,
    /// Fraction of the grid height below the detonation point.
    pub floor_fraction: f32,
    /// Default timeline.
    pub timeline: Timeline,
    /// Shape of faceted clouds.
    pub facets: FacetSettings,
    /// What counts as cloud in a faceted cloud's rule.
    pub facet_rule: FacetRule,
    /// How far apart a volumetric cloud's rule samples a line: every voxel,
    /// or every half voxel for checks that count a little more precisely
    /// and cost about 1.5 times as much.
    pub volume_steps: VolumeSteps,
    /// Shape of crystal clouds. `blend` is ignored: crystals never merge.
    pub crystals: FacetSettings,
    /// Bullet and explosion holes; `None` turns them off.
    pub holes: Option<HoleSettings>,
    /// Holes per cloud. Beyond this the oldest open ones stop counting.
    /// `bevy_smokescreen` draws up to 512 at once across its smokes: 16
    /// for each of 32.
    pub max_holes_per_cloud: usize,
    /// Most clouds at once. Past this the oldest stop counting (renderers
    /// stop drawing them too), so a renderer that can draw this many never
    /// leaves a cloud that blocks sight undrawn. `bevy_smokescreen` draws up to 32.
    pub max_clouds: usize,
    /// A line of sight is blocked once it passes through this much cloud,
    /// every cloud it crosses added up. 0 blocks on any contact, which suits
    /// the crystal style: what players see is then exactly what blocks.
    pub sight_threshold: f32,
}

impl Tuning {
    /// Defaults, in metres: a CS2-sized 90 m³ cloud of 0.3 m voxels, blocking
    /// sight through 0.8 m of cloud.
    pub fn metric() -> Self {
        Self {
            grid_dims: UVec3::new(48, 24, 48),
            voxel_size: 0.3,
            volume: 90.0,
            up_cost: 1.4,
            down_cost: 0.9,
            lumpiness: 0.3,
            lump_size: 1.6,
            floor_fraction: 0.25,
            timeline: Timeline::default(),
            facets: FacetSettings::metric(),
            facet_rule: FacetRule::Exact,
            volume_steps: VolumeSteps::Voxel,
            crystals: FacetSettings::crystal(),
            holes: Some(HoleSettings::metric()),
            max_holes_per_cloud: 16,
            max_clouds: 32,
            sight_threshold: 0.8,
        }
    }

    /// The defaults converted to `units`.
    pub fn new(units: &Units) -> Self {
        Self::metric().map_lengths(|l| units.length_from_internal(l), |v| units.volume_from_internal(v))
    }

    fn map_lengths(&self, length: impl Fn(f32) -> f32 + Copy, volume: impl Fn(f32) -> f32) -> Self {
        Self {
            voxel_size: length(self.voxel_size),
            volume: volume(self.volume),
            lump_size: length(self.lump_size),
            facets: self.facets.map_lengths(length),
            crystals: self.crystals.map_lengths(length),
            holes: self.holes.map(|h| h.map_lengths(length)),
            sight_threshold: length(self.sight_threshold),
            ..self.clone()
        }
    }

    /// In metres, with the grid rounded to multiples of 4.
    fn to_internal(&self, units: &Units) -> Self {
        let mut t = self.map_lengths(|l| units.length_to_internal(l), |v| units.volume_to_internal(v));
        t.grid_dims = (t.grid_dims.max(UVec3::splat(8)) + 3) / 4 * 4;
        t.voxel_size = t.voxel_size.max(1e-3);
        t
    }
}

/// Everything a [`Smokes`] world needs to know up front.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The caller's units and up axis: every position, length and result
    /// the world takes or gives is in them.
    pub units: Units,
    /// Simulation ticks per second.
    pub tick_rate: f32,
    /// How clouds spread, look and block sight, in [`Self::units`].
    pub tuning: Tuning,
    /// Work units per tick spent on unfinished fills, about 10 ns each plus
    /// [`Self::level_query_units`] per level query. If a fill would still be
    /// running when its cloud starts ([`Timeline::start`]), the budget grows
    /// to spread what is left evenly over the ticks until then; a fill still
    /// unfinished at that point is finished on the spot. Performance only:
    /// the finished cloud is the same whatever the budget.
    ///
    /// With the default tuning a volumetric fill is about 260,000 units and
    /// a faceted one about 660,000, so at 128 ticks a second the default
    /// budget needs about 0.1 s for a volumetric fill and 0.26 s for a
    /// faceted one. With the default [`Timeline::start`] of 0.1 s, faceted
    /// fills therefore run at about two and a half times the budget: give
    /// faceted clouds a later start to keep to it.
    pub fill_budget: u32,
    /// Work units charged per level query (default 4, about 40 ns):
    /// [`crate::Level::is_blocked`] or [`crate::Level::is_inside`].
    pub level_query_units: u32,
    /// Ticks after a cloud or hole has ended that it is kept, so a renderer
    /// drawing slightly in the past still sees it.
    pub history_ticks: u64,
    /// Ticks whose per-tick state is cached (the newest simulation tick, the
    /// tick being drawn, ...).
    pub cached_ticks: usize,
}

impl Config {
    /// Default tuning in `units`, a 0.2 ms fill budget per tick and a second
    /// of history.
    pub fn new(units: Units, tick_rate: f32) -> Self {
        Self {
            units,
            tick_rate,
            tuning: Tuning::new(&units),
            fill_budget: 20_000,
            level_query_units: 4,
            history_ticks: tick_rate.max(1.0).ceil() as u64,
            cached_ticks: 4,
        }
    }
}

/// How a cloud is shaped. All three styles share the fill, timeline, holes
/// and rule; only what counts as cloud differs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Style {
    /// A soft density field. The amount of cloud along a segment is its
    /// density integrated along it (metres of full-density cloud).
    #[default]
    Volumetric,
    /// Hard-edged tumbling polyhedral puffs merged like metaballs. The amount
    /// of cloud along a segment is the length of it inside the puffs (or
    /// their cores, see [`FacetRule`]).
    ///
    /// Experimental: `bevy_smokescreen` draws it by sphere tracing, which
    /// costs 10 to 40 times what a volumetric smoke costs on the GPU, and
    /// bullet tunnels make it about three times dearer again. At the cheaper
    /// steps of the plugin's quality dial, tunnels you can see right through
    /// come out a few pixels wider than at the best. The label is about
    /// drawing it: its simulation and rule are no less finished than the
    /// other styles'.
    Faceted,
    /// Hard-edged tumbling polyhedra that don't merge: cheap to draw as plain
    /// meshes and to check exactly. Holes shrink whole puffs out of the way.
    /// The amount of cloud along a segment is the length of it inside them.
    Crystal,
}

/// What a cloud spreads from. Positions and lengths in the caller's units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CloudSource {
    /// From its origin: a grenade.
    #[default]
    Point,
    /// From every point of the segment from its origin to `end`: a smoke
    /// wall.
    Line {
        /// The segment's other end.
        end: Vec3,
    },
    /// From every open point of the box centred on its origin: a fog bank, a
    /// gas-filled room. The box must fit the fill grid
    /// ([`Tuning::grid_dims`]); set the cloud's volume to about the box's.
    Box {
        /// Half the box's size along each axis.
        half_extents: Vec3,
    },
}

/// A cloud: where, when and how it popped. This is all a server has to send.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudSpec {
    /// Where the grenade came to rest, in the caller's units.
    pub origin: Vec3,
    /// The tick it popped at: its timeline counts from here.
    pub pop_tick: u64,
    /// Seeds the fill's lumps and the puffs' shapes and spin
    /// ([`CloudSpec::seed_from_origin`] makes one from the origin).
    pub seed: u32,
    /// What it looks like, and so what the rule counts.
    pub style: Style,
    /// What it spreads from (the origin by default).
    pub source: CloudSource,
    /// Volume instead of [`Tuning::volume`].
    pub volume: Option<f32>,
    /// Timeline instead of [`Tuning::timeline`].
    pub timeline: Option<Timeline>,
}

impl CloudSpec {
    /// A grenade at `origin` popping at `pop_tick`, with the tuning's volume
    /// and timeline.
    pub fn new(origin: Vec3, pop_tick: u64, seed: u32, style: Style) -> Self {
        Self {
            origin,
            pop_tick,
            seed,
            style,
            source: CloudSource::Point,
            volume: None,
            timeline: None,
        }
    }

    /// The same cloud spreading from `source`.
    pub fn with_source(mut self, source: CloudSource) -> Self {
        self.source = source;
        self
    }

    /// A seed made from the origin's bits, so the same throw gives the same
    /// cloud everywhere.
    pub fn seed_from_origin(origin: Vec3) -> u32 {
        hash_u32(origin.x.to_bits() ^ origin.y.to_bits().rotate_left(11) ^ origin.z.to_bits().rotate_left(22))
    }
}

/// A cloud's handle within one [`Smokes`] world. Handles count up in the
/// order clouds are added, so they aren't shared between machines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CloudId(pub(crate) u64);

struct Ready {
    shape: Arc<CloudShape>,
    /// Faceted: seconds after the pop when each puff pops in.
    arrival: Vec<f32>,
    bounds: (Vec3, Vec3),
}

impl Ready {
    fn new(shape: Arc<CloudShape>) -> Self {
        let bounds = shape.bounds();
        Self {
            shape,
            arrival: Vec::new(),
            bounds,
        }
    }
}

enum Fill {
    Pending(Box<FillJob>),
    Ready(Ready),
}

struct Cloud {
    id: CloudId,
    spec: CloudSpec,
    timeline: Timeline,
    /// First tick at which the cloud has started to grow (the fill's
    /// deadline).
    start_tick: u64,
    /// First tick at which it is gone.
    end_tick: u64,
    fill: Fill,
}

impl Cloud {
    #[inline]
    fn age(&self, tick: u64, rate: f32) -> Option<f32> {
        (tick >= self.spec.pop_tick).then(|| (tick - self.spec.pop_tick) as f32 / rate)
    }

    fn ready(&self) -> Option<&Ready> {
        match &self.fill {
            Fill::Ready(r) => Some(r),
            Fill::Pending(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct HoleEntry {
    tick: u64,
    /// Orders holes of the same tick, independently of arrival order.
    key: u64,
    kind: HoleKind,
}

fn hole_key(kind: &HoleKind) -> u64 {
    let words: [u32; 7] = match *kind {
        HoleKind::Bullet { start, end } => [
            1,
            start.x.to_bits(),
            start.y.to_bits(),
            start.z.to_bits(),
            end.x.to_bits(),
            end.y.to_bits(),
            end.z.to_bits(),
        ],
        HoleKind::Explosion { center, radius } => [
            2,
            center.x.to_bits(),
            center.y.to_bits(),
            center.z.to_bits(),
            radius.to_bits(),
            0,
            0,
        ],
    };
    let (mut a, mut b) = (0x243f_6a88_u32, 0x85a3_08d3_u32);
    for w in words {
        a = hash_u32(a ^ w);
        b = hash_u32(b.rotate_left(5) ^ w ^ a);
    }
    ((a as u64) << 32) | b as u64
}

/// One cloud at one tick: its growth, holes and (faceted) puffs.
#[derive(Clone, Debug, Default)]
#[doc(hidden)]
pub struct CloudState {
    cloud: usize,
    pub id: CloudId,
    /// Seconds since the pop.
    pub age: f32,
    /// [`Timeline::growth`] at this age.
    pub growth: f32,
    /// [`Timeline::dissolve`] at this age.
    pub dissolve: f32,
    /// Volumetric: the density of every fill-distance byte at this age,
    /// which samples interpolate (see [`DensityTable`]). Faceted and crystal
    /// clouds have the empty table.
    pub(crate) density: DensityTable,
    /// Box containing everything the cloud can occupy (internal frame).
    pub bounds: (Vec3, Vec3),
    /// Box containing everything it occupies at this tick, or `None` if
    /// nothing (internal frame). Smaller than `bounds` while it grows and
    /// fades.
    pub active: Option<(Vec3, Vec3)>,
    /// Open holes, oldest first (internal frame; bullets clipped to the
    /// cloud).
    pub holes: Vec<HoleShape>,
    /// Faceted and crystal: every puff's size and orientation (internal
    /// frame).
    pub live: Vec<LivePuff>,
}

impl Default for CloudId {
    fn default() -> Self {
        CloudId(u64::MAX)
    }
}

struct Snapshot {
    tick: u64,
    generation: u64,
    stamp: u64,
    len: usize,
    states: Vec<CloudState>,
    /// Clouds present past [`Tuning::max_clouds`], left out (the oldest).
    uncounted: usize,
    /// A faceted cloud is present whose puffs are big enough for the chord
    /// bounds to be worth trying (see `View::facet_bounds`).
    facets_to_bound: bool,
    broad: BroadPhase,
}

/// Cells per axis of a [`BroadPhase`] grid.
const BROAD_CELLS: usize = 16;

/// Padding of a query's box when it asks the grid: a segment's end lies
/// within rounding of where its slab tests put it, so the candidates are
/// a superset either way and the answers are exactly those of looking at
/// every cloud.
const BROAD_PAD: Vec3 = Vec3::splat(1e-3);

/// Which clouds a query has to look at: a coarse grid over the clouds'
/// boxes (across and along; the up axis is ignored), each cell holding a
/// bit per cloud whose box overlaps it. A segment looks at the clouds in
/// the cells its own box covers, in index order, so the sums and early
/// stops are exactly those of looking at every cloud: a cloud the segment
/// crosses overlaps every cell its box and the segment's box share, and the
/// clouds the grid leaves out would have failed the box test anyway.
#[derive(Default)]
struct BroadPhase {
    /// Corner of the grid (x, z) and cells per metre.
    min: [f32; 2],
    inv_cell: [f32; 2],
    /// Per cell, a bit per state index (x fastest). Empty when there are
    /// no clouds, or more than a mask holds: then every cloud is looked at.
    masks: Vec<u64>,
    all: bool,
}

/// The states a query must look at (see [`BroadPhase`]).
#[derive(Clone, Copy)]
enum Candidates {
    All,
    /// A bit per state index.
    Mask(u64),
}

impl BroadPhase {
    fn build(&mut self, states: &[CloudState]) {
        self.masks.clear();
        self.all = states.len() > 64;
        if states.is_empty() || self.all {
            return;
        }
        let (mut lo, mut hi) = (Vec3::MAX, Vec3::MIN);
        for st in states {
            lo = lo.min(st.bounds.0);
            hi = hi.max(st.bounds.1);
        }
        self.min = [lo.x, lo.z];
        // Cells at least a millimetre across, so a point maps to one cell.
        let n = BROAD_CELLS as f32;
        self.inv_cell = [n / (hi.x - lo.x).max(1e-3), n / (hi.z - lo.z).max(1e-3)];
        self.masks.resize(BROAD_CELLS * BROAD_CELLS, 0);
        for (i, st) in states.iter().enumerate() {
            let (x0, x1) = self.span(0, st.bounds.0.x, st.bounds.1.x);
            let (z0, z1) = self.span(1, st.bounds.0.z, st.bounds.1.z);
            for z in z0..=z1 {
                for x in x0..=x1 {
                    self.masks[x + BROAD_CELLS * z] |= 1 << i;
                }
            }
        }
    }

    /// The cells (inclusive, clamped to the grid) an interval covers along
    /// an axis.
    #[inline]
    fn span(&self, axis: usize, lo: f32, hi: f32) -> (usize, usize) {
        let cell =
            |v: f32| (((v - self.min[axis]) * self.inv_cell[axis]).floor() as i32).clamp(0, BROAD_CELLS as i32 - 1);
        (cell(lo) as usize, cell(hi) as usize)
    }

    /// The clouds whose boxes the box `lo..hi` could touch.
    #[inline]
    fn candidates(&self, lo: Vec3, hi: Vec3) -> Candidates {
        if self.all {
            return Candidates::All;
        }
        if self.masks.is_empty() {
            return Candidates::Mask(0);
        }
        let (x0, x1) = self.span(0, lo.x, hi.x);
        let (z0, z1) = self.span(1, lo.z, hi.z);
        let mut mask = 0;
        for z in z0..=z1 {
            for x in x0..=x1 {
                mask |= self.masks[x + BROAD_CELLS * z];
            }
        }
        Candidates::Mask(mask)
    }
}

/// Call `visit` on the candidate states in index order until it returns
/// `Some`, and return that.
#[inline]
fn visit_candidates<T>(
    states: &[CloudState],
    candidates: Candidates,
    mut visit: impl FnMut(&CloudState) -> Option<T>,
) -> Option<T> {
    match candidates {
        Candidates::All => states.iter().find_map(visit),
        Candidates::Mask(mut mask) => {
            while mask != 0 {
                let i = mask.trailing_zeros() as usize;
                mask &= mask - 1;
                if let Some(out) = visit(&states[i]) {
                    return Some(out);
                }
            }
            None
        }
    }
}

/// Every cloud of a match, and the gameplay queries against them.
///
/// The caller supplies the ticks: a cloud at tick `t` is a pure function of
/// its [`CloudSpec`], the level, the [`Tuning`] and `t`, so every machine
/// that is told about a pop (or a hole) computes the same cloud.
///
/// ```
/// use smokescreen::*;
/// use glam::Vec3;
///
/// let config = Config::new(Units::METRES_Y_UP, 128.0);
/// let mut smokes = Smokes::new(config, FlatGround::default());
/// smokes.pop(CloudSpec::new(Vec3::ZERO, 100, 7, Style::Faceted));
/// // Every tick: spend the fill budget, then query.
/// for tick in 100..400 {
///     smokes.advance(tick);
/// }
/// let eye = Vec3::new(-10.0, 1.5, 0.0);
/// let target = Vec3::new(10.0, 1.5, 0.0);
/// assert!(smokes.blocks_sight(eye, target, 400));
/// assert!(smokes.occlusion(eye, target, 400) > 2.0);
/// ```
pub struct Smokes<L> {
    config: Config,
    /// The tuning in metres.
    internal: Tuning,
    level: L,
    clouds: Vec<Cloud>,
    /// Sorted by (tick, key).
    holes: Vec<HoleEntry>,
    next_id: u64,
    generation: u64,
    snapshots: Vec<Snapshot>,
    stamp: u64,
    last_tick: Option<u64>,
    scratch: Vec<usize>,
}

impl<L: Level> Smokes<L> {
    /// An empty world: no clouds yet, smoke spreading around `level`.
    pub fn new(config: Config, level: L) -> Self {
        let internal = config.tuning.to_internal(&config.units);
        Self {
            config,
            internal,
            level,
            clouds: Vec::new(),
            holes: Vec::new(),
            next_id: 0,
            generation: 0,
            snapshots: Vec::new(),
            stamp: 0,
            last_tick: None,
            scratch: Vec::new(),
        }
    }

    /// The configuration it was made with.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The caller's units ([`Config::units`]).
    pub fn units(&self) -> &Units {
        &self.config.units
    }

    /// The tuning in metres, with the grid rounded as it is used.
    #[doc(hidden)]
    pub fn internal_tuning(&self) -> &Tuning {
        &self.internal
    }

    /// The level smoke spreads around.
    pub fn level(&self) -> &L {
        &self.level
    }

    /// Replace the level. Clouds already filled keep their shape; fills in
    /// progress start over against the new level.
    pub fn set_level(&mut self, level: L) {
        self.level = level;
        for i in 0..self.clouds.len() {
            if matches!(self.clouds[i].fill, Fill::Pending(_)) {
                let spec = self.fill_spec(&self.clouds[i].spec);
                self.clouds[i].fill = Fill::Pending(Box::new(FillJob::new(spec)));
            }
        }
    }

    fn rate(&self) -> f32 {
        self.config.tick_rate
    }

    fn fill_spec(&self, spec: &CloudSpec) -> FillSpec {
        make_fill_spec(&self.internal, self.config.tuning.voxel_size, &self.config.units, spec)
    }

    /// First tick from `pop` at which `pred(age)` holds, near `seconds` after
    /// it (`pred` must be monotonic in age).
    fn first_tick(&self, pop: u64, seconds: f32, pred: impl Fn(f32) -> bool) -> u64 {
        let rate = self.rate();
        let age = |k: u64| k as f32 / rate;
        // The estimate is off by at most a tick or two of rounding.
        let mut k = (seconds.max(0.0) * rate).ceil() as u64;
        while k > 0 && pred(age(k - 1)) {
            k -= 1;
        }
        for _ in 0..4 {
            if pred(age(k)) {
                break;
            }
            k += 1;
        }
        pop.saturating_add(k)
    }

    fn new_cloud(&mut self, spec: CloudSpec, fill: Fill) -> CloudId {
        let id = CloudId(self.next_id);
        self.next_id += 1;
        let timeline = spec.timeline.unwrap_or(self.config.tuning.timeline);
        let start_tick = self.first_tick(spec.pop_tick, timeline.start, |a| timeline.has_started(a));
        let end_tick = self.first_tick(spec.pop_tick, timeline.lifetime, |a| timeline.is_finished(a));
        let mut cloud = Cloud {
            id,
            spec,
            timeline,
            start_tick,
            end_tick,
            fill,
        };
        if let Fill::Ready(r) = &mut cloud.fill {
            r.arrival = arrival_times(&r.shape, &cloud.timeline);
        }
        self.clouds.push(cloud);
        self.generation += 1;
        id
    }

    /// A grenade popped. Its fill runs a little every [`Self::advance`] and
    /// is finished on the spot if it is needed earlier.
    pub fn pop(&mut self, spec: CloudSpec) -> CloudId {
        let job = FillJob::new(self.fill_spec(&spec));
        self.new_cloud(spec, Fill::Pending(Box::new(job)))
    }

    /// A cloud whose fill was computed elsewhere (by [`fill_cloud`], or
    /// received from a server). It must come from the same spec, level and
    /// tuning, or machines will disagree.
    ///
    /// A [`CloudShape`]'s contents are not part of the API yet: to send one
    /// between machines, run the same version of this crate on both. Its
    /// grid is used as it is. One put together by hand (received field by
    /// field, say) needs its bricks (`Grid::build_bricks`), and checks are
    /// faster with its front bounds and, if it is volumetric, its sample
    /// cells (`Grid::build_front_bounds`, `Grid::build_sample_cells`);
    /// without those they give the same answers. [`fill_cloud`] builds all
    /// three.
    pub fn insert(&mut self, spec: CloudSpec, shape: Arc<CloudShape>) -> CloudId {
        self.new_cloud(spec, Fill::Ready(Ready::new(shape)))
    }

    /// Remove a cloud now, whatever its age.
    pub fn remove(&mut self, id: CloudId) -> bool {
        let before = self.clouds.len();
        self.clouds.retain(|c| c.id != id);
        let removed = self.clouds.len() != before;
        if removed {
            self.generation += 1;
        }
        removed
    }

    /// Remove every cloud and hole.
    pub fn clear(&mut self) {
        self.clouds.clear();
        self.holes.clear();
        self.generation += 1;
    }

    fn add_hole(&mut self, kind: HoleKind, tick: u64) {
        if self.internal.holes.is_none() {
            return;
        }
        let entry = HoleEntry {
            tick,
            key: hole_key(&kind),
            kind,
        };
        let at = self.holes.partition_point(|h| (h.tick, h.key) <= (tick, entry.key));
        self.holes.insert(at, entry);
        self.generation += 1;
    }

    /// A shot from `start` to `end` at `tick`: a tunnel through any cloud it
    /// crosses. Ignored when holes are off.
    pub fn bullet(&mut self, start: Vec3, end: Vec3, tick: u64) {
        let units = self.config.units;
        self.add_hole(
            HoleKind::Bullet {
                start: units.to_internal(start),
                end: units.to_internal(end),
            },
            tick,
        );
    }

    /// An explosion at `tick`: clears a sphere (default radius from the
    /// tuning) that refills. Ignored when holes are off.
    pub fn explosion(&mut self, center: Vec3, radius: Option<f32>, tick: u64) {
        let units = self.config.units;
        let Some(holes) = self.internal.holes else {
            return;
        };
        let radius = radius.map_or(holes.explosion_radius, |r| units.length_to_internal(r));
        self.add_hole(
            HoleKind::Explosion {
                center: units.to_internal(center),
                radius,
            },
            tick,
        );
    }

    fn finish_fill(&mut self, i: usize) {
        if let Fill::Pending(job) = &mut self.clouds[i].fill {
            job.step(&self.level, &self.config.units, &mut Work::unlimited());
        }
        self.complete(i);
    }

    /// Turn a finished job into a ready cloud.
    fn complete(&mut self, i: usize) {
        let cloud = &mut self.clouds[i];
        let Fill::Pending(job) = &mut cloud.fill else {
            return;
        };
        if !job.is_done() {
            return;
        }
        let job = std::mem::replace(job, Box::new(FillJob::new(placeholder_spec())));
        let mut ready = Ready::new(Arc::new(job.into_shape()));
        ready.arrival = arrival_times(&ready.shape, &cloud.timeline);
        cloud.fill = Fill::Ready(ready);
    }

    /// Finish every fill whose cloud has started growing by `tick`.
    fn finish_due(&mut self, tick: u64) {
        for i in 0..self.clouds.len() {
            if tick >= self.clouds[i].start_tick && matches!(self.clouds[i].fill, Fill::Pending(_)) {
                self.finish_fill(i);
            }
        }
    }

    /// Call once per tick (calling again for the same tick is harmless).
    /// Spends the fill budget, finishes fills that are due, and forgets
    /// clouds and holes that ended more than [`Config::history_ticks`] ago.
    pub fn advance(&mut self, tick: u64) {
        // 0 when the same tick comes again: nothing more to spend.
        let elapsed = self.last_tick.map_or(1, |last| tick.saturating_sub(last)).min(8) as i64;
        self.last_tick = Some(self.last_tick.map_or(tick, |last| last.max(tick)));
        self.finish_due(tick);

        // Earliest deadline first. The budget grows if a fill would otherwise
        // still be running when its cloud starts, spreading what is left
        // evenly over the ticks until then.
        let query = self.config.level_query_units as i64;
        let mut pending = std::mem::take(&mut self.scratch);
        pending.clear();
        pending.extend((0..self.clouds.len()).filter(|&i| matches!(self.clouds[i].fill, Fill::Pending(_))));
        pending.sort_by_key(|&i| (self.clouds[i].start_tick, self.clouds[i].id));
        let mut budget = self.config.fill_budget as i64 * elapsed;
        let mut due = 0;
        for &i in &pending {
            if let Fill::Pending(job) = &self.clouds[i].fill {
                due += job.remaining(query);
                let ticks = self.clouds[i].start_tick.saturating_sub(tick).max(1) as i64;
                budget = budget.max(due * elapsed / ticks);
            }
        }
        let mut work = Work::new(budget, query);
        for &i in &pending {
            if work.left() <= 0 {
                break;
            }
            if let Fill::Pending(job) = &mut self.clouds[i].fill
                && job.step(&self.level, &self.config.units, &mut work)
            {
                self.complete(i);
            }
        }
        self.scratch = pending;

        // Forget what can no longer matter.
        let history = self.config.history_ticks;
        let before = self.clouds.len();
        self.clouds.retain(|c| tick < c.end_tick.saturating_add(history));
        if self.clouds.len() != before {
            self.generation += 1;
        }
        if let Some(h) = self.internal.holes {
            let lasting = (h.max_duration() * self.config.tick_rate).ceil() as u64 + 1;
            self.holes
                .retain(|e| tick < e.tick.saturating_add(lasting).saturating_add(history));
        }
    }

    /// Clouds whose fill is still running.
    pub fn pending_fills(&self) -> usize {
        self.clouds
            .iter()
            .filter(|c| matches!(c.fill, Fill::Pending(_)))
            .count()
    }

    /// Every cloud, in the order they were added.
    pub fn clouds(&self) -> impl Iterator<Item = CloudInfo<'_>> {
        self.clouds.iter().map(CloudInfo::new)
    }

    /// The cloud with this handle, if the world still has it.
    pub fn cloud(&self, id: CloudId) -> Option<CloudInfo<'_>> {
        self.clouds.iter().find(|c| c.id == id).map(CloudInfo::new)
    }

    /// Compute (or reuse) the state of every cloud at `tick`, finishing any
    /// fill needed for it. Returns the snapshot's index.
    fn prepare(&mut self, tick: u64) -> usize {
        self.stamp += 1;
        if let Some(i) = self
            .snapshots
            .iter()
            .position(|s| s.tick == tick && s.generation == self.generation)
        {
            self.snapshots[i].stamp = self.stamp;
            return i;
        }
        self.finish_due(tick);
        let slot = if self.snapshots.len() < self.config.cached_ticks.max(1) {
            self.snapshots.push(Snapshot {
                tick,
                generation: 0,
                stamp: 0,
                len: 0,
                states: Vec::new(),
                uncounted: 0,
                facets_to_bound: false,
                broad: BroadPhase::default(),
            });
            self.snapshots.len() - 1
        } else {
            (0..self.snapshots.len())
                .min_by_key(|&i| self.snapshots[i].stamp)
                .unwrap_or(0)
        };
        let rate = self.config.tick_rate;
        let max_clouds = self.internal.max_clouds;
        let snap = &mut self.snapshots[slot];
        snap.tick = tick;
        snap.generation = self.generation;
        snap.stamp = self.stamp;
        snap.len = 0;
        for (ci, cloud) in self.clouds.iter().enumerate() {
            let (Some(ready), Some(age)) = (cloud.ready(), cloud.age(tick, rate)) else {
                continue;
            };
            let tl = &cloud.timeline;
            if !tl.has_started(age) || tl.is_finished(age) || ready.shape.is_empty() {
                continue;
            }
            if snap.len == snap.states.len() {
                snap.states.push(CloudState::default());
            }
            let st = &mut snap.states[snap.len];
            snap.len += 1;
            st.cloud = ci;
            st.id = cloud.id;
            st.age = age;
            st.growth = tl.growth(age);
            st.dissolve = tl.dissolve(age);
            // Kept from this slot's last tick while growth and dissolve are
            // the same (a grown cloud's, every tick).
            if ready.shape.facets.is_none() {
                st.density.set(st.growth, st.dissolve);
            } else {
                st.density.set(0.0, 0.0);
            }
            st.bounds = ready.bounds;
            st.holes.clear();
            if let Some(hs) = &self.internal.holes {
                collect_holes(
                    &self.holes,
                    cloud.spec.pop_tick,
                    tick,
                    rate,
                    hs,
                    ready.bounds,
                    self.internal.max_holes_per_cloud,
                    &mut st.holes,
                );
            }
            st.live.clear();
            if let Some(f) = &ready.shape.facets {
                st.live.extend(
                    f.puffs
                        .iter()
                        .zip(&ready.arrival)
                        .map(|(p, t_in)| p.live(*t_in, age, tl, &f.settings)),
                );
                if cloud.spec.style == Style::Crystal {
                    shrink_for_holes(&mut st.live, &st.holes);
                }
            }
            st.active = active_bounds(&ready.shape, st);
        }
        // The same order on every machine, whatever order the pops arrived
        // in (sums over clouds depend on it), and the newest `max_clouds`.
        let clouds = &self.clouds;
        let states = &mut snap.states[..snap.len];
        states.sort_unstable_by_key(|st| {
            let c = &clouds[st.cloud];
            let o = c.spec.origin;
            (
                c.spec.pop_tick,
                c.spec.seed,
                o.x.to_bits(),
                o.y.to_bits(),
                o.z.to_bits(),
                c.spec.style as u8,
                c.id,
            )
        });
        snap.uncounted = snap.len.saturating_sub(max_clouds);
        if snap.len > max_clouds {
            states.rotate_left(snap.len - max_clouds);
            snap.len = max_clouds;
        }
        snap.facets_to_bound = snap.states[..snap.len]
            .iter()
            .any(|st| st.dissolve <= LATE_FADE && clouds[st.cloud].spec.style == Style::Faceted);
        snap.broad.build(&snap.states[..snap.len]);
        slot
    }

    /// Every cloud at `tick`, for many queries (from many threads) or for
    /// drawing. Computing a tick's state costs about 20 ns per faceted puff;
    /// it is cached for the last few ticks asked for.
    pub fn view(&mut self, tick: u64) -> View<'_> {
        let slot = self.prepare(tick);
        let snap = &self.snapshots[slot];
        View {
            tick,
            states: &snap.states[..snap.len],
            uncounted: snap.uncounted,
            clouds: &self.clouds,
            config: &self.config,
            facets_to_bound: snap.facets_to_bound,
            broad: &snap.broad,
        }
    }

    /// The amount of cloud along the segment `from..to` at `tick`, every
    /// cloud added up, in the caller's units (see [`Style`]).
    pub fn occlusion(&mut self, from: Vec3, to: Vec3, tick: u64) -> f32 {
        self.view(tick).occlusion(from, to)
    }

    /// Does the segment `from..to` pass through at least
    /// [`Tuning::sight_threshold`] of cloud at `tick`? The answer
    /// `occlusion > 0 && occlusion >= threshold` gives, but it stops
    /// counting early. Through faceted clouds under [`FacetRule::Exact`] it
    /// decides most lines from exact bounds on the amount instead of the
    /// sphere trace: the puffs' hard polyhedra lie inside the cloud, and the
    /// polyhedra grown by the blend radius contain it. Only lines within
    /// 2 cm of the threshold are traced. The trace places each surface
    /// crossing to about 5 mm, so the two could only differ on a line that
    /// crosses the surface many times; none did in 730,000 random lines.
    pub fn blocks_sight(&mut self, from: Vec3, to: Vec3, tick: u64) -> bool {
        self.view(tick).blocks_sight(from, to)
    }

    /// Density (0..1 per cloud, summed) at a point at `tick`. Faceted clouds
    /// count 1 inside, 0 outside.
    pub fn density_at(&mut self, p: Vec3, tick: u64) -> f32 {
        self.view(tick).density_at(p)
    }
}

/// Crystal puffs shrink out of the way of holes: each keeps its corners
/// clear of every open hole's core, so a shot opens a clean tunnel through
/// the crystals. The core is as wide as the faceted style's hole:
/// `radius × min(1, 1.5 × strength)` around a bullet's path, 0.8 of that
/// around a blast.
fn shrink_for_holes(live: &mut [LivePuff], holes: &[HoleShape]) {
    for h in holes {
        let blast = if h.a == h.b { 0.8 } else { 1.0 };
        let clear = h.radius * (h.strength * 1.5).min(1.0) * blast;
        if clear <= 0.0 {
            continue;
        }
        for p in live.iter_mut() {
            if p.radius <= 0.0 {
                continue;
            }
            let reach = p.radius * p.ratio;
            let d = distance_to_segment(p.center, h.a, h.b);
            if d < clear + reach {
                p.radius *= (d - clear).max(0.0) / reach;
                p.core = p.core.min(p.radius);
            }
        }
    }
}

/// What a cloud occupies at one tick (see [`CloudState::active`]).
fn active_bounds(shape: &CloudShape, state: &CloudState) -> Option<(Vec3, Vec3)> {
    match &shape.facets {
        Some(f) => {
            let mut bounds: Option<(Vec3, Vec3)> = None;
            for p in &state.live {
                if p.radius <= 0.0 {
                    continue;
                }
                let r = Vec3::splat(p.radius * p.ratio + f.settings.blend);
                let (lo, hi) = (p.center - r, p.center + r);
                bounds = Some(bounds.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
            }
            bounds
        }
        None => {
            let grid = &shape.grid;
            if grid.front_bounds.is_empty() {
                return Some(state.bounds);
            }
            // As in `VolumeField::amount`: no density from this byte on.
            let skip_from = state.density.empty_from() as usize;
            if skip_from == 0 {
                return None;
            }
            grid.front_bounds[skip_from - 1]
        }
    }
}

/// Faceted: seconds after the pop when each puff pops in.
fn arrival_times(shape: &CloudShape, timeline: &Timeline) -> Vec<f32> {
    shape
        .facets
        .as_ref()
        .map(|f| f.puffs.iter().map(|p| p.arrival_time(timeline)).collect())
        .unwrap_or_default()
}

fn placeholder_spec() -> FillSpec {
    FillSpec {
        origin: Vec3::ZERO,
        source: Source::Point,
        crystal: false,
        dims: UVec3::splat(4),
        voxel: 1.0,
        level_voxel: 1.0,
        volume: 0.0,
        up_cost: 1.0,
        down_cost: 1.0,
        lumpiness: 0.0,
        lump_size: 1.0,
        floor_fraction: 0.0,
        seed: 0,
        facets: None,
    }
}

/// The newest `max` open holes from events between the pop and `tick` that
/// overlap `bounds`, oldest first.
#[allow(clippy::too_many_arguments)]
fn collect_holes(
    holes: &[HoleEntry],
    pop_tick: u64,
    tick: u64,
    rate: f32,
    settings: &HoleSettings,
    bounds: (Vec3, Vec3),
    max: usize,
    out: &mut Vec<HoleShape>,
) {
    let end = holes.partition_point(|h| h.tick <= tick);
    for h in holes[..end].iter().rev() {
        if out.len() >= max || h.tick < pop_tick {
            break;
        }
        let age = (tick - h.tick) as f32 / rate;
        let Some(mut shape) = h.kind.shape(age, settings) else {
            continue;
        };
        if !boxes_overlap(h.kind.bounds(settings), bounds) {
            continue;
        }
        if let HoleKind::Bullet { start, end } = h.kind {
            // Clip the tunnel to the cloud; inside it nothing changes.
            let pad = Vec3::splat(settings.bullet_radius);
            let Some((t0, t1)) = clip_segment(start, end, bounds.0 - pad, bounds.1 + pad) else {
                continue;
            };
            shape.a = start.lerp(end, t0);
            shape.b = start.lerp(end, t1);
        }
        out.push(shape);
    }
    out.reverse();
}

/// Run a whole fill at once, outside any world (for example on a server that
/// sends finished clouds). Same result as [`Smokes::pop`] gives.
pub fn fill_cloud<L: Level + ?Sized>(config: &Config, spec: &CloudSpec, level: &L) -> CloudShape {
    let internal = config.tuning.to_internal(&config.units);
    crate::fill::fill_now(
        make_fill_spec(&internal, config.tuning.voxel_size, &config.units, spec),
        level,
        &config.units,
    )
}

/// A fill's parameters in metres, from tuning in metres (and the voxel size
/// in the caller's units, for asking the level).
fn make_fill_spec(t: &Tuning, level_voxel: f32, units: &Units, spec: &CloudSpec) -> FillSpec {
    let source = match spec.source {
        CloudSource::Point => Source::Point,
        CloudSource::Line { end } => Source::Line {
            end: units.to_internal(end),
        },
        CloudSource::Box { half_extents } => Source::Box {
            half: (units.dir_to_internal(half_extents) * units.metres_per_unit).abs(),
        },
    };
    let (facets, crystal) = match spec.style {
        Style::Volumetric => (None, false),
        Style::Faceted => (Some(t.facets), false),
        Style::Crystal => (
            Some(FacetSettings {
                blend: 0.0,
                ..t.crystals
            }),
            true,
        ),
    };
    FillSpec {
        origin: units.to_internal(spec.origin),
        source,
        dims: t.grid_dims,
        voxel: t.voxel_size,
        level_voxel,
        volume: spec.volume.map_or(t.volume, |v| units.volume_to_internal(v)),
        up_cost: t.up_cost,
        down_cost: t.down_cost,
        lumpiness: t.lumpiness,
        lump_size: t.lump_size,
        floor_fraction: t.floor_fraction,
        seed: spec.seed,
        facets,
        crystal,
    }
}

/// Slack (metres) the chord bounds keep from the sight threshold, so they
/// never contradict the sphere trace, which locates each surface crossing
/// to about [`crate::facets::MIN_STEP`]: a few crossings' worth.
const FACET_MARGIN: f32 = 0.02;

/// Dissolve past which faceted clouds are traced without trying the bounds.
const LATE_FADE: f32 = 0.7;

/// What the chord bounds settled (see `View::facet_bounds`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Blocked,
    Clear,
    Trace,
}

/// What a world knows about one cloud.
#[derive(Clone, Copy)]
pub struct CloudInfo<'a> {
    cloud: &'a Cloud,
}

impl<'a> CloudInfo<'a> {
    fn new(cloud: &'a Cloud) -> Self {
        Self { cloud }
    }

    /// Its handle.
    pub fn id(&self) -> CloudId {
        self.cloud.id
    }

    /// The spec it was added with.
    pub fn spec(&self) -> &'a CloudSpec {
        &self.cloud.spec
    }

    /// The timeline it runs on: its spec's, or else the tuning's.
    pub fn timeline(&self) -> &'a Timeline {
        &self.cloud.timeline
    }

    /// First tick at which the cloud has started to grow.
    pub fn start_tick(&self) -> u64 {
        self.cloud.start_tick
    }

    /// First tick at which it is gone.
    pub fn end_tick(&self) -> u64 {
        self.cloud.end_tick
    }

    /// The finished fill, once it is.
    pub fn shape(&self) -> Option<&'a Arc<CloudShape>> {
        self.cloud.ready().map(|r| &r.shape)
    }

    /// Seconds since the pop at `tick` (`None` before the pop).
    pub fn age(&self, tick: u64, tick_rate: f32) -> Option<f32> {
        self.cloud.age(tick, tick_rate)
    }
}

/// Every cloud at one tick. Answers any number of queries without
/// allocating, from any number of threads.
#[derive(Clone, Copy)]
pub struct View<'a> {
    tick: u64,
    states: &'a [CloudState],
    uncounted: usize,
    clouds: &'a [Cloud],
    config: &'a Config,
    facets_to_bound: bool,
    broad: &'a BroadPhase,
}

/// One cloud at the view's tick.
#[derive(Clone, Copy)]
#[doc(hidden)]
pub struct CloudView<'a> {
    pub state: &'a CloudState,
    pub shape: &'a Arc<CloudShape>,
    pub spec: &'a CloudSpec,
    pub timeline: &'a Timeline,
    volume_steps: VolumeSteps,
}

impl<'a> CloudView<'a> {
    /// The faceted cloud's distance field, if it is faceted.
    pub fn facet_field(&self) -> Option<FacetField<'a>> {
        let f = self
            .shape
            .facets
            .as_ref()
            .filter(|_| self.spec.style == Style::Faceted)?;
        Some(FacetField {
            grid: &self.shape.grid,
            bins: &f.bins,
            live: &self.state.live,
            blend: f.settings.blend,
            holes: &self.state.holes,
        })
    }

    /// The faceted cloud's cores (see [`FacetRule::Core`]), if it is faceted.
    pub(crate) fn core_field(&self) -> Option<CoreField<'a>> {
        let f = self
            .shape
            .facets
            .as_ref()
            .filter(|_| self.spec.style == Style::Faceted)?;
        Some(CoreField {
            bins: &f.bins,
            live: &self.state.live,
            holes: &self.state.holes,
        })
    }

    /// The faceted cloud's hard polyhedra (see [`FacetRule::Chords`]), if it
    /// is faceted.
    pub fn chord_field(&self) -> Option<ChordField<'a>> {
        let f = self
            .shape
            .facets
            .as_ref()
            .filter(|_| self.spec.style == Style::Faceted)?;
        Some(ChordField {
            bins: &f.bins,
            live: &self.state.live,
            puffs: &f.puffs,
            holes: &self.state.holes,
        })
    }

    /// The crystal cloud's puffs, if it is crystal.
    pub(crate) fn crystal_field(&self) -> Option<CrystalField<'a>> {
        let f = self
            .shape
            .facets
            .as_ref()
            .filter(|_| self.spec.style == Style::Crystal)?;
        Some(CrystalField {
            bins: &f.bins,
            live: &self.state.live,
        })
    }

    /// The volumetric cloud's density field, if it is volumetric.
    pub(crate) fn volume_field(&self) -> Option<VolumeField<'a>> {
        self.shape.facets.is_none().then_some(VolumeField {
            grid: &self.shape.grid,
            density: &self.state.density,
            holes: &self.state.holes,
            step: self.volume_steps.step(self.shape.grid.voxel_size),
        })
    }
}

impl<'a> View<'a> {
    /// The tick it shows.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// How many clouds present at this tick it leaves out, past
    /// [`Tuning::max_clouds`]: the oldest, which neither count nor are drawn.
    #[doc(hidden)]
    pub fn uncounted(&self) -> usize {
        self.uncounted
    }

    /// The clouds present at this tick (started and not yet gone).
    #[doc(hidden)]
    pub fn clouds(&self) -> impl Iterator<Item = CloudView<'a>> + 'a {
        let clouds = self.clouds;
        let volume_steps = self.config.tuning.volume_steps;
        self.states.iter().filter_map(move |state| {
            let cloud = &clouds[state.cloud];
            let ready = cloud.ready()?;
            Some(CloudView {
                state,
                shape: &ready.shape,
                spec: &cloud.spec,
                timeline: &cloud.timeline,
                volume_steps,
            })
        })
    }

    /// See [`Smokes::occlusion`].
    pub fn occlusion(&self, from: Vec3, to: Vec3) -> f32 {
        let mut acc = 0.0;
        self.integrate(from, to, &mut acc, &|_| false);
        self.config.units.length_from_internal(acc)
    }

    /// See [`Smokes::blocks_sight`].
    pub fn blocks_sight(&self, from: Vec3, to: Vec3) -> bool {
        // Faceted clouds under the exact rule: decide from chord bounds
        // where they settle it (see `facet_bounds`).
        if self.facets_to_bound && self.config.tuning.facet_rule == FacetRule::Exact {
            return self.blocks_sight_bounded(from, to);
        }
        let units = self.config.units;
        let threshold = self.config.tuning.sight_threshold;
        let mut acc = 0.0;
        self.integrate(from, to, &mut acc, &|acc| {
            let amount = units.length_from_internal(acc);
            amount > 0.0 && amount >= threshold
        })
    }

    /// [`Self::blocks_sight`] with faceted clouds under the exact rule: from
    /// the chord bounds where they settle it, tracing the segment only
    /// where they don't. Kept out of line so that the plain path above
    /// compiles as it did without it.
    #[inline(never)]
    fn blocks_sight_bounded(&self, from: Vec3, to: Vec3) -> bool {
        let units = self.config.units;
        let threshold = self.config.tuning.sight_threshold;
        let stop = |acc: f32| {
            let amount = units.length_from_internal(acc);
            amount > 0.0 && amount >= threshold
        };
        match self.facet_bounds(from, to, &stop) {
            Verdict::Blocked => true,
            Verdict::Clear => false,
            Verdict::Trace => {
                let mut acc = 0.0;
                self.integrate(from, to, &mut acc, &stop)
            }
        }
    }

    /// `blocks_sight` by bounds: every cloud's amount is at least its
    /// lower bound (crystal, core and volumetric clouds integrated as usual,
    /// faceted ones by [`ChordBounds`]) and at most its upper bound, so the
    /// segment is blocked once the lower bounds reach the threshold and
    /// clear if the upper bounds never do. The sphere trace locates each
    /// surface crossing to about [`crate::facets::MIN_STEP`]; a margin of
    /// [`FACET_MARGIN`] keeps the bounds on its side of the threshold, so a
    /// segment is only left to the trace when it is within that of blocked.
    fn facet_bounds(&self, from: Vec3, to: Vec3, stop: &dyn Fn(f32) -> bool) -> Verdict {
        let units = &self.config.units;
        let margin = FACET_MARGIN;
        let a = units.to_internal(from);
        let delta = units.to_internal(to) - a;
        let len = delta.length();
        if len.is_nan() || len <= 0.0 {
            return Verdict::Clear;
        }
        let rd = delta / len;
        let inv = safe_recip(rd);
        let (mut lower, mut upper) = (0.0f32, 0.0f32);
        let mut crossed = false;
        let mut buf = [HoleShape::NONE; HOLE_BUFFER];
        let end = units.to_internal(to);
        let candidates = self.broad.candidates(a.min(end) - BROAD_PAD, a.max(end) + BROAD_PAD);
        let settled = visit_candidates(self.states, candidates, |state| {
            let (lo, hi) = state.bounds;
            let (t0, t1) = slab(a, inv, lo, hi);
            let (t0, t1) = (t0.max(0.0), t1.min(len));
            if t1 <= t0 {
                return None;
            }
            let (alo, ahi) = state.active?;
            let (u0, u1) = slab(a, inv, alo, ahi);
            let (u0, u1) = (u0.max(t0), u1.min(t1));
            if u1 < u0 {
                return None;
            }
            let cloud = &self.clouds[state.cloud];
            let ready = cloud.ready()?;
            let shape = &ready.shape;
            crossed = true;
            // Faceted: chord bounds. Any other cloud: its exact amount,
            // which serves as both bounds.
            let mut part = 0.0;
            let stopped = match &shape.facets {
                Some(_) if cloud.spec.style == Style::Faceted && state.dissolve > LATE_FADE => {
                    // Late in the fade the puffs left are small next to
                    // the blend radius, so the bounds seldom settle a line;
                    // the trace is cheap then anyway.
                    return Some(Verdict::Trace);
                }
                Some(f) if cloud.spec.style == Style::Faceted => {
                    // Over the whole box, like the trace: a grown polyhedron
                    // can reach a little past what the puffs occupy.
                    let holes = holes_near(&state.holes, a, inv, t0, t1, |h| h.radius * 0.2, &mut buf);
                    let b = ChordBounds {
                        bins: &f.bins,
                        live: &state.live,
                        puffs: &f.puffs,
                        blend: f.settings.blend,
                        holes,
                    }
                    .amount_until(a, rd, t0, t1, &|l| stop(lower + l - margin));
                    upper += b.upper - b.lower;
                    part = b.lower;
                    stop(lower + part - margin)
                }
                Some(f) => CrystalField {
                    bins: &f.bins,
                    live: &state.live,
                }
                .amount(a, rd, u0, u1, &mut part, &|p| stop(lower + p - margin)),
                None => {
                    let holes = holes_near(&state.holes, a, inv, t0, t1, |_| 0.0, &mut buf);
                    VolumeField {
                        grid: &shape.grid,
                        density: &state.density,
                        holes,
                        step: self.config.tuning.volume_steps.step(shape.grid.voxel_size),
                    }
                    .amount(a, rd, t0, t1, &mut part, &|p| stop(lower + p - margin))
                }
            };
            lower += part;
            upper += part;
            stopped.then_some(Verdict::Blocked)
        });
        if let Some(verdict) = settled {
            return verdict;
        }
        // Nothing crossed, or the bounds settle it (with the margin: the
        // amounts of the other clouds were summed in a different order
        // from the trace's, which can differ in the last bit).
        if !crossed || !stop(upper + margin) {
            Verdict::Clear
        } else if stop(lower - margin) {
            Verdict::Blocked
        } else {
            Verdict::Trace
        }
    }

    /// See [`Smokes::density_at`].
    pub fn density_at(&self, p: Vec3) -> f32 {
        let p = self.config.units.to_internal(p);
        let core_rule = self.config.tuning.facet_rule == FacetRule::Core;
        let chord_rule = self.config.tuning.facet_rule == FacetRule::Chords;
        let mut sum = 0.0;
        let candidates = self.broad.candidates(p - BROAD_PAD, p + BROAD_PAD);
        visit_candidates(self.states, candidates, |state| {
            let (lo, hi) = state.bounds;
            if p.cmplt(lo).any() || p.cmpgt(hi).any() {
                return None;
            }
            let c = &self.clouds[state.cloud];
            let ready = c.ready()?;
            let cloud = CloudView {
                state,
                shape: &ready.shape,
                spec: &c.spec,
                timeline: &c.timeline,
                volume_steps: self.config.tuning.volume_steps,
            };
            let inside = |yes: bool| if yes { 1.0 } else { 0.0 };
            sum += if let Some(c) = cloud.crystal_field() {
                inside(c.inside(p))
            } else if let (true, Some(c)) = (core_rule, cloud.core_field()) {
                inside(c.inside(p))
            } else if let (true, Some(c)) = (chord_rule, cloud.chord_field()) {
                inside(c.inside(p))
            } else if let Some(f) = cloud.facet_field() {
                inside(f.sdf(p) < 0.0)
            } else if let Some(v) = cloud.volume_field() {
                v.density(p)
            } else {
                0.0
            };
            None::<()>
        });
        sum
    }

    fn integrate(&self, from: Vec3, to: Vec3, acc: &mut f32, stop: &dyn Fn(f32) -> bool) -> bool {
        let units = &self.config.units;
        let a = units.to_internal(from);
        let delta = units.to_internal(to) - a;
        let len = delta.length();
        if len.is_nan() || len <= 0.0 {
            return false;
        }
        let rd = delta / len;
        let inv = safe_recip(rd);
        let facet_rule = self.config.tuning.facet_rule;
        let end = units.to_internal(to);
        let candidates = self.broad.candidates(a.min(end) - BROAD_PAD, a.max(end) + BROAD_PAD);
        visit_candidates(self.states, candidates, |state| {
            // Broad phase: the cloud's box, then what it occupies now.
            let (lo, hi) = state.bounds;
            let (t0, t1) = slab(a, inv, lo, hi);
            let (t0, t1) = (t0.max(0.0), t1.min(len));
            if t1 <= t0 {
                return None;
            }
            let (alo, ahi) = state.active?;
            let (u0, u1) = slab(a, inv, alo, ahi);
            let (u0, u1) = (u0.max(t0), u1.min(t1));
            if u1 < u0 {
                return None;
            }
            let cloud = &self.clouds[state.cloud];
            let ready = cloud.ready()?;
            let shape = &ready.shape;
            // Only the holes that could matter along this segment; the rest
            // would change nothing.
            let mut buf = [HoleShape::NONE; HOLE_BUFFER];
            let stopped = match &shape.facets {
                Some(f) if cloud.spec.style == Style::Crystal => CrystalField {
                    bins: &f.bins,
                    live: &state.live,
                }
                .amount(a, rd, u0, u1, acc, stop),
                Some(f) if facet_rule == FacetRule::Core => {
                    let holes = holes_near(&state.holes, a, inv, u0, u1, |h| h.radius * 0.2, &mut buf);
                    CoreField {
                        bins: &f.bins,
                        live: &state.live,
                        holes,
                    }
                    .amount(a, rd, u0, u1, acc, stop)
                }
                Some(f) if facet_rule == FacetRule::Chords => {
                    let holes = holes_near(&state.holes, a, inv, u0, u1, |h| h.radius * 0.2, &mut buf);
                    ChordField {
                        bins: &f.bins,
                        live: &state.live,
                        puffs: &f.puffs,
                        holes,
                    }
                    .amount(a, rd, u0, u1, acc, stop)
                }
                Some(f) => {
                    // A hole changes the field (and so the integrator's
                    // steps) up to about two puffs away.
                    let reach = 2.0 * f.settings.max_reach();
                    let holes = holes_near(&state.holes, a, inv, t0, t1, |h| h.radius * 0.2 + reach, &mut buf);
                    FacetField {
                        grid: &shape.grid,
                        bins: &f.bins,
                        live: &state.live,
                        blend: f.settings.blend,
                        holes,
                    }
                    .amount(a, rd, t0, t1, acc, stop)
                }
                None => {
                    let holes = holes_near(&state.holes, a, inv, t0, t1, |_| 0.0, &mut buf);
                    VolumeField {
                        grid: &shape.grid,
                        density: &state.density,
                        holes,
                        step: self.config.tuning.volume_steps.step(shape.grid.voxel_size),
                    }
                    .amount(a, rd, t0, t1, acc, stop)
                }
            };
            stopped.then_some(())
        })
        .is_some()
    }
}
