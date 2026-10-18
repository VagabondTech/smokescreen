//! # bevy_smokescreen
//!
//! Smoke that fills rooms, pours through doorways, stops at walls and blocks
//! sight, for Bevy 0.20: smoke grenades, smoke walls, poison gas, fog banks
//! and steam. In two crates:
//!
//! - **[`smokescreen`]** (re-exported) is the simulation, with no engine in it:
//!   the flood fill, the cloud's shape over time, bullet and explosion holes,
//!   and the gameplay queries ([`Smokes::occlusion`], [`Smokes::blocks_sight`]).
//!   It runs on your ticks, in your units, and gives the same answers on every
//!   machine, so a server and its bots link it directly.
//! - **This crate** draws what the core computed, and wraps it in ECS
//!   conveniences: a [`Smoke`] component with presets, bullet and explosion
//!   messages, [`SmokeVision`] for gameplay systems, and [`SmokeObstacle`] to
//!   make level meshes block smoke.
//!
//! Three looks share the core's fill:
//!
//! - **Volumetric** ([`SmokeStyle::Volumetric`]): soft, light-scattering smoke
//!   raymarched at reduced resolution.
//! - **Faceted** ([`SmokeStyle::Faceted`]): hard-edged, tumbling polyhedral
//!   puffs merged like metaballs, sphere-traced against a distance field.
//! - **Crystal** ([`SmokeStyle::Crystal`]): tumbling polyhedra that don't
//!   merge, drawn as ordinary meshes. The cheapest look, and the one whose
//!   gameplay checks are cheapest and match the picture exactly.
//!
//! The work is split so that each piece runs where it is cheapest:
//!
//! 1. **Fill (core, CPU, a little every tick).** A Dijkstra fill spreads
//!    through a small voxel grid until it reaches the cloud's volume, stopping
//!    at walls and flowing through doorways. Each voxel stores *when* the smoke
//!    reaches it. Faceted and crystal clouds also seed their puffs.
//! 2. **Upload (once per cloud).** The grid goes into a slot of a 3D texture
//!    atlas, with sun and sky visibility worked out on a background thread.
//! 3. **Bake (GPU, only while something changes).** Growth, fade and holes
//!    per voxel, then per-voxel lighting.
//! 4. **Per camera.** Volumetric smokes are raymarched at reduced resolution
//!    inside the screen rectangle they cover; faceted smokes are traced
//!    coarsely and only silhouettes and creases traced again at full
//!    resolution. Crystal smokes are ordinary meshes, one entity per puff,
//!    drawn and shadowed by Bevy's PBR renderer.
//!
//! One dial per camera sets the cost of volumetric and faceted smoke:
//! [`SmokeCamera::quality`], or [`SmokeCamera::budget_ms`] to turn it
//! automatically to fit a millisecond budget. See [`quality`].
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_smokescreen::prelude::*;
//!
//! fn main() {
//!     App::new()
//!         .add_plugins((DefaultPlugins, SmokePlugin::default()))
//!         .add_systems(Startup, setup)
//!         .run();
//! }
//!
//! fn setup(mut commands: Commands) {
//!     commands.spawn((Camera3d::default(), SmokeCamera::default()));
//!     // A grenade popping at the origin.
//!     commands.spawn((Smoke::grenade(), Transform::from_xyz(0.0, 0.0, 0.0)));
//! }
//! ```
//!
//! The README in the repository root has a longer guide: presets, level
//! setup, gameplay queries, networking and tuning.

mod crystal;
mod noise;
mod obstacles;
pub mod quality;
mod render;

use std::sync::Arc;

use bevy::{
    ecs::system::SystemParam,
    light::{DirectionalLight, GlobalAmbientLight, PointLight},
    platform::collections::HashMap,
    prelude::*,
    render::{RenderApp, extract_component::ExtractComponent},
    tasks::{AsyncComputeTaskPool, Task, futures::check_ready},
    transform::TransformSystems,
};

pub use crystal::{CrystalPuff, polyhedron_mesh};
pub use obstacles::SmokeObstacle;
pub use quality::{FacetOcclusion, FacetQuality, FacetShadows, SmokeQuality, VolumetricQuality};
pub use smokescreen;
/// How a smoke is shaped: soft volumetric density, merged facets, or
/// separate crystals.
pub use smokescreen::Style as SmokeStyle;
/// A cloud's timing (see [`smokescreen::Timeline`]).
pub use smokescreen::Timeline as SmokeTimeline;
pub use smokescreen::{
    BoxObstacles, CloudId, CloudSource, CloudSpec, Config, FacetRule, FacetSettings, FlatGround, HoleSettings, Level,
    ObstacleBox, Polyhedron, ShapeMix, Smokes, Tuning, Units, UpAxis, VoxelObstacles,
};

use render::{FrameFacet, FrameLight, FrameSlot, SlotUpload, SmokeFrame, SmokeUploads};
use smokescreen::light::{LightRays, visibility};

pub mod prelude {
    pub use crate::{
        BoxObstacles, CrystalPuff, FacetQuality, FacetRule, FacetSettings, FlatGround, NoiseWithinRule, ObstacleBox,
        Polyhedron, ShapeMix, Smoke, SmokeBullet, SmokeCamera, SmokeClock, SmokeExplosion, SmokeLevel, SmokeObstacle,
        SmokePlugin, SmokeQuality, SmokeResolution, SmokeSettings, SmokeSource, SmokeStyle, SmokeSun, SmokeSystems,
        SmokeVision, SmokeWorld, Units, UpAxis, VolumetricQuality, VoxelObstacles,
    };
}

/// Compiles the README's examples as doc tests, so they keep up with the API.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;

/// Adds smoke simulation and rendering.
///
/// The core [`Config`] (your units, tick rate and [`Tuning`]) sizes the GPU
/// atlas, so it is fixed when the plugin is added: one slot per cloud
/// [`Tuning::max_clouds`] allows (at most 32), so every cloud that counts is
/// drawn. With the defaults the atlases and noise take about 24 MB of VRAM and
/// the faceted style's puff buffers about 10 MB; lower `max_clouds` to save
/// memory. Per camera at 1080p, add about 7 MB for volumetric smoke and up to
/// about 35 MB for faceted smoke at a reduced-resolution dial step. Crystal
/// smoke needs none of this: it is drawn as ordinary meshes.
///
/// Positions in Bevy (transforms, messages, [`SmokeVision`]) are in
/// [`Config::units`]: Y-up metres by default, but a world in Z-up inches
/// works just as well, with the core and the renderer agreeing.
#[derive(Clone, Debug)]
pub struct SmokePlugin {
    /// The simulation's configuration: units, ticks per second and the rules.
    pub config: Config,
}

impl Default for SmokePlugin {
    fn default() -> Self {
        Self {
            config: Config::new(Units::METRES_Y_UP, 128.0),
        }
    }
}

/// System sets, in order, all in `PostUpdate` after transform propagation.
/// Write [`SmokeBullet`]/[`SmokeExplosion`] messages in `Update` (or anywhere
/// before [`SmokeSystems::Simulate`]).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum SmokeSystems {
    /// Pop new smokes, apply impacts, advance the simulation to
    /// [`SmokeClock::tick`].
    Simulate,
    /// Gather everything the renderer needs and pick each camera's tier.
    PrepareRender,
}

impl Plugin for SmokePlugin {
    fn build(&self, app: &mut App) {
        let world = Smokes::new(
            self.config.clone(),
            Arc::new(FlatGround::new(0.0, self.config.units.up)) as SharedLevel,
        );
        let internal = world.internal_tuning();
        if internal.max_clouds > render::MAX_SLOTS {
            warn!(
                "Tuning::max_clouds is {}, but at most {} volumetric or faceted smokes are drawn at once; the rest would still block sight. Lower max_clouds, or use the crystal style, which has no such limit.",
                internal.max_clouds,
                render::MAX_SLOTS
            );
        }
        let config = SmokeConfig {
            max_smokes: (internal.max_clouds as u32).clamp(1, render::MAX_SLOTS as u32),
            grid_dims: internal.grid_dims,
            voxel_size: internal.voxel_size,
            world_voxel_size: self.config.tuning.voxel_size,
            facet_limits: internal.facets.limits,
            units: self.config.units,
            tick_rate: self.config.tick_rate,
        };
        let uploads = SmokeUploads::default();

        app.insert_resource(config)
            .insert_resource(SmokeWorld(world))
            .insert_resource(uploads.clone())
            .init_resource::<SmokeClock>()
            .init_resource::<SmokeSettings>()
            .init_resource::<SlotTable>()
            .init_resource::<SmokeLighting>()
            .init_resource::<SmokeEntities>()
            .init_resource::<SmokeFrame>()
            .init_resource::<crystal::CrystalClouds>()
            .init_resource::<obstacles::ObstacleBuild>()
            .add_message::<SmokeBullet>()
            .add_message::<SmokeExplosion>()
            .add_systems(Startup, crystal::setup_meshes)
            .add_systems(PreUpdate, advance_clock)
            .configure_sets(
                PostUpdate,
                (SmokeSystems::Simulate, SmokeSystems::PrepareRender)
                    .chain()
                    .after(TransformSystems::Propagate),
            )
            .add_systems(
                PostUpdate,
                (
                    (
                        obstacles::voxelise_obstacles,
                        sync_level,
                        pop_smokes,
                        apply_impacts,
                        advance_world,
                        retire_smokes,
                    )
                        .chain()
                        .in_set(SmokeSystems::Simulate),
                    (
                        light_clouds,
                        build_frame,
                        crystal::sync_crystals,
                        quality::update_smoke_quality,
                    )
                        .chain()
                        .in_set(SmokeSystems::PrepareRender),
                ),
            );
        render::build(app, config, uploads);
    }

    fn finish(&self, app: &mut App) {
        render::finish(app);
    }
}

/// A level shared between the simulation and lighting threads.
pub type SharedLevel = Arc<dyn Level + Send + Sync>;

/// Fixed configuration derived from [`SmokePlugin`]: what the renderer sizes
/// its buffers by. Lengths in metres (the core's internal frame).
#[derive(Resource, Clone, Copy, Debug)]
pub struct SmokeConfig {
    /// Volumetric and faceted smokes drawn at once: [`Tuning::max_clouds`],
    /// at most 32.
    pub max_smokes: u32,
    /// Voxels per cloud.
    pub grid_dims: UVec3,
    /// Voxel edge length, metres.
    pub voxel_size: f32,
    /// Voxel edge length in the world's units ([`Tuning::voxel_size`]):
    /// [`SmokeObstacle`] voxelises with it, so its voxels are the very ones
    /// the fills ask about.
    pub world_voxel_size: f32,
    pub facet_limits: smokescreen::FacetLimits,
    /// The units of the Bevy world (and of everything passed to the core).
    pub units: Units,
    pub tick_rate: f32,
}

impl SmokeConfig {
    /// The core's internal frame (metres, Y up) from the Bevy world's.
    pub fn internal_from_world(&self) -> Mat4 {
        let m = self.units.metres_per_unit;
        let axis = |v: Vec3| (self.units.dir_to_internal(v) * m).extend(0.0);
        Mat4::from_cols(axis(Vec3::X), axis(Vec3::Y), axis(Vec3::Z), Vec4::W)
    }
}

/// Every smoke: the core simulation, ready for gameplay queries.
///
/// The [`Smoke`] component is a convenience on top: you can also pop clouds,
/// add holes and query directly here, for example from your networking code
/// with the server's ticks. Everything in it is drawn.
#[derive(Resource, Deref, DerefMut)]
pub struct SmokeWorld(pub Smokes<SharedLevel>);

/// The tick the smoke is simulated and drawn at.
///
/// By default the plugin counts ticks from Bevy's virtual time
/// ([`Config::tick_rate`] per second), so pausing virtual time freezes the
/// smoke. To drive smoke from your own simulation, set `auto` to `false` and
/// write `tick` (and `draw_tick`) yourself before `PostUpdate`.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmokeClock {
    /// The newest simulation tick: fills advance to it, impacts land on it
    /// and [`SmokeVision`] answers for it.
    pub tick: u64,
    /// The tick drawn, for example your interpolation tick slightly behind
    /// `tick`. `None` draws `tick`.
    pub draw_tick: Option<u64>,
    /// Count `tick` from `Time<Virtual>` every frame.
    pub auto: bool,
}

impl Default for SmokeClock {
    fn default() -> Self {
        Self {
            tick: 0,
            draw_tick: None,
            auto: true,
        }
    }
}

impl SmokeClock {
    /// The tick being drawn.
    pub fn drawn(&self) -> u64 {
        self.draw_tick.unwrap_or(self.tick)
    }
}

/// Look of all smokes. Change at runtime freely: nothing here changes what
/// hides what (that is [`Tuning`], fixed in the plugin's [`Config`]).
#[derive(Resource, Clone, Debug)]
pub struct SmokeSettings {
    /// Style of smokes whose [`Smoke::style`] is `None`.
    pub style: SmokeStyle,
    /// Extinction (1/m) of fully dense volumetric smoke. 7 means you can't
    /// see past about half a metre inside a smoke.
    pub extinction: f32,
    /// Scattering colour of the smoke particles (the surface colour of
    /// faceted smoke).
    pub albedo: LinearRgba,
    /// Henyey–Greenstein anisotropy. Slightly positive gives a soft glow
    /// when looking towards the sun through thin smoke.
    pub anisotropy: f32,
    /// Multiplier on the sun's illuminance.
    pub sun_intensity: f32,
    /// Multiplier on ambient light.
    pub ambient_intensity: f32,
    /// Ambient radiance to use instead of [`GlobalAmbientLight`]. Set this when
    /// your scene is lit by an environment map.
    pub ambient_override: Option<LinearRgba>,
    /// Strength of the multiple-scattering approximation. 0 is single
    /// scattering only, which looks too dark; around 1 the sunlit side reads
    /// almost as bright as a white wall, like CS2 smokes.
    pub multi_scattering: f32,
    /// How strongly smoke deep inside the cloud is shielded from ambient light.
    pub ambient_occlusion: f32,
    /// Fraction of ambient light smoke keeps where the level hides the sky
    /// (indoors, under overhangs). Bevy doesn't occlude ambient light on
    /// surfaces, so around 0.5 keeps indoor smoke about as bright as indoor
    /// walls; lower it if your scene has baked GI or SSAO.
    pub indoor_ambient: f32,
    /// Spatial frequency of the detail noise, in tiles per metre.
    pub noise_frequency: f32,
    /// How much the billow noise carves the cloud's outer layers (0..1).
    /// The noise only ever thins the smoke, so it shows through some smoke
    /// the rule counts, most while a cloud fades, unless
    /// [`noise_within_rule`](Self::noise_within_rule) says otherwise.
    pub erosion: f32,
    /// How much the fine noise frays the edges (0..1).
    pub detail_erosion: f32,
    /// Whether volumetric smoke's noise may show through what the rule
    /// blocks. Off by default; see [`NoiseWithinRule`].
    pub noise_within_rule: NoiseWithinRule,
    /// Drift of the noise, in world units per second. A little upward drift
    /// reads as rising smoke.
    pub wind: Vec3,
    /// Crystal puffs cast shadows onto the level (and each other).
    pub crystal_shadows: bool,
    /// Draw faceted and crystal smoke as flat magenta: shows exactly which
    /// pixels are opaque (useful for checking silhouettes and tier fairness).
    pub debug_silhouette: bool,
    /// Draw volumetric smoke as how much of the scene shows through it, over
    /// nothing: full red and blue, and that share in green, so opaque smoke
    /// is magenta and thin smoke nearly white. With the camera's
    /// [`Tonemapping`](bevy::core_pipeline::tonemapping::Tonemapping) off,
    /// a screenshot holds it to 8 bits, sRGB-encoded (the demo's `agreement`
    /// run and `tools/agreement.py` compare it with the rule).
    pub debug_transmittance: bool,
}

impl Default for SmokeSettings {
    fn default() -> Self {
        Self {
            style: SmokeStyle::Volumetric,
            extinction: 7.0,
            albedo: LinearRgba::rgb(0.9, 0.9, 0.92),
            anisotropy: 0.25,
            sun_intensity: 1.0,
            ambient_intensity: 1.0,
            ambient_override: None,
            multi_scattering: 1.0,
            ambient_occlusion: 0.85,
            indoor_ambient: 0.5,
            noise_frequency: 0.22,
            erosion: 0.6,
            detail_erosion: 0.35,
            noise_within_rule: NoiseWithinRule::Off,
            wind: Vec3::new(0.12, 0.22, 0.05),
            crystal_shadows: true,
            debug_silhouette: false,
            debug_transmittance: false,
        }
    }
}

/// How volumetric smoke's noise relates to the rule ([`SmokeSettings::noise_within_rule`]).
///
/// The noise only ever thins the smoke, so it can show through some of what
/// the rule ([`Tuning::sight_threshold`]) blocks, most while a cloud fades.
/// Every player sees the same thinning, so between players that is fair; a
/// server, bots or abilities that use the rule see less than players there.
/// For those games, `Capped` and `Filled` keep volumetric smoke opaque on
/// every line the rule blocks (at [`Smoke::density`] 1 or more), each with a
/// different cost. Both leave thin smoke (wisps, edges, silhouettes) all its
/// noise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NoiseWithinRule {
    /// The noise may thin smoke the rule counts. Billowy, and fading clouds
    /// dissolve into wisps. Rays end once less than 5 % of the scene shows
    /// through them, so a line through ln(20) / [`SmokeSettings::extinction`]
    /// of the cloud as drawn (0.43 m by default) is opaque. `Filled` does the
    /// same; `Capped` goes on to 1 %, which its budget is worked out from.
    #[default]
    Off,
    /// Along any line of sight the noise takes no more cloud than the rule
    /// leaves spare: [`Tuning::sight_threshold`] less the ln(100) /
    /// [`SmokeSettings::extinction`] of cloud the picture needs to be opaque
    /// (0.8 − 0.66 = 0.14 m by default). Thicker smoke looks smoother, and a
    /// fading cloud stays a pale, smooth dome where the rule still blocks. The
    /// cheapest: a ray stops sampling the noise once it has taken all it may
    /// (about 13 % less than `Off` close up at the best dial step on an Apple
    /// M4). A threshold under ln(100) / extinction leaves the noise nothing to
    /// take, so volumetric smoke is then drawn without it.
    Capped,
    /// The noise runs as with `Off`, and on a line through more cloud than
    /// the picture needs to be opaque (ln(20) / extinction, 0.43 m by
    /// default, as with `Off`) the opacity it took is filled back in behind
    /// it, completely by the rule's threshold. Keeps the billows; a fading
    /// cloud stays a textured dome where the rule still blocks. Costs about
    /// 12 % more than `Off` close up at the best dial step on an Apple M4.
    Filled,
}

/// Level geometry the fill respects, in the world's units.
///
/// Without one, smoke only knows a flat ground at height 0 (and says so in
/// the log). Set it at map load, before smokes pop: clouds already filled
/// keep their shape.
///
/// ```no_run
/// # use bevy::prelude::*;
/// # use bevy_smokescreen::prelude::*;
/// # use std::sync::Arc;
/// fn load_level(mut commands: Commands) {
///     let boxes = vec![ObstacleBox::from_center_size(Vec3::new(0.0, 1.5, 4.0), Vec3::new(8.0, 3.0, 0.2))];
///     // Pre-voxelise once so every smoke is just bit lookups.
///     let voxels = VoxelObstacles::from_level(
///         Vec3::new(-50.0, -1.0, -50.0), Vec3::new(50.0, 10.0, 50.0), 0.3,
///         &BoxObstacles::new(boxes),
///     );
///     commands.insert_resource(SmokeLevel(Arc::new(voxels)));
/// }
/// ```
#[derive(Resource, Clone)]
pub struct SmokeLevel(pub SharedLevel);

/// Marks the directional light that lights smoke. Without it the first
/// directional light found is used.
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct SmokeSun;

/// A bullet passing through smoke at the current [`SmokeClock::tick`]. Leaves
/// a tunnel that closes in about 2 s.
#[derive(Message, Clone, Copy, Debug)]
pub struct SmokeBullet {
    pub start: Vec3,
    pub end: Vec3,
}

/// An explosion inside or near smoke at the current [`SmokeClock::tick`].
/// Clears a sphere that refills over about 5 s.
#[derive(Message, Clone, Copy, Debug)]
pub struct SmokeExplosion {
    pub center: Vec3,
    /// Radius of the cleared sphere. `None` uses [`HoleSettings::explosion_radius`].
    pub radius: Option<f32>,
}

/// Add to any `Camera3d` that should see smoke. Holds the camera's
/// performance dial.
///
/// ```no_run
/// # use bevy_smokescreen::prelude::*;
/// // Fixed quality, 0 (cheapest) ..= 1 (best), in six steps:
/// let fixed = SmokeCamera { quality: 0.6, ..Default::default() };
/// // Or: best quality that fits 1.5 ms of GPU time, re-decided every frame.
/// let budgeted = SmokeCamera::budget(1.5);
/// ```
#[derive(Component, Clone, Copy, Debug, ExtractComponent)]
#[extract_app(RenderApp)]
#[require(SmokeQuality)]
pub struct SmokeCamera {
    /// The dial: 0 = cheapest .. 1 = best, in six steps (see
    /// [`quality::TIER_NAMES`]). With `budget_ms` set, this is the highest
    /// step the budget controller may use.
    pub quality: f32,
    /// Keep this camera's smoke passes under this many milliseconds of GPU
    /// time by turning the dial every frame. Needs
    /// `bevy::render::diagnostic::RenderDiagnosticsPlugin` for GPU timings.
    pub budget_ms: Option<f32>,
    /// Vary the step jitter every frame. Turn on with TAA, which then
    /// averages the jitter away; leave off without TAA for a stable dither.
    pub temporal_jitter: bool,
    /// Pin the volumetric knobs instead of deriving them from the dial.
    pub volumetric: Option<VolumetricQuality>,
    /// Pin the faceted knobs instead of deriving them from the dial.
    pub facets: Option<FacetQuality>,
}

/// Fraction of the camera resolution (per axis) the volumetric smoke is
/// raymarched at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SmokeResolution {
    Full,
    TwoThirds,
    Half,
    Quarter,
}

impl SmokeResolution {
    /// Camera pixels per raymarched pixel, per axis.
    pub fn divisor(self) -> f32 {
        match self {
            Self::Full => 1.0,
            Self::TwoThirds => 1.5,
            Self::Half => 2.0,
            Self::Quarter => 4.0,
        }
    }
}

impl SmokeCamera {
    pub const LOW: Self = Self::with_quality(0.2);
    pub const MEDIUM: Self = Self::with_quality(0.4);
    pub const HIGH: Self = Self::with_quality(0.8);
    pub const ULTRA: Self = Self::with_quality(1.0);

    /// A fixed quality, 0 (cheapest) ..= 1 (best).
    pub const fn with_quality(quality: f32) -> Self {
        Self {
            quality,
            budget_ms: None,
            temporal_jitter: false,
            volumetric: None,
            facets: None,
        }
    }

    /// The best quality that fits `ms` milliseconds of GPU time per frame.
    pub const fn budget(ms: f32) -> Self {
        Self {
            budget_ms: Some(ms),
            ..Self::ULTRA
        }
    }

    /// Volumetric knobs at a tier (or the pinned override).
    pub fn volumetric_quality(&self, tier: usize) -> VolumetricQuality {
        self.volumetric.unwrap_or(VolumetricQuality::tier(tier))
    }

    /// Faceted knobs at a tier (or the pinned override).
    pub fn facet_quality(&self, tier: usize) -> FacetQuality {
        self.facets.unwrap_or(FacetQuality::tier(tier))
    }
}

impl Default for SmokeCamera {
    fn default() -> Self {
        Self::HIGH
    }
}

/// What a [`Smoke`] spreads from, relative to its entity. World units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SmokeSource {
    /// From the entity's position: a grenade.
    #[default]
    Point,
    /// From every point between the entity and `offset` from it: a smoke
    /// wall.
    Line { offset: Vec3 },
    /// From every open point of a box centred on the entity: a fog bank, a
    /// gas-filled room. The box must fit the fill grid
    /// ([`Tuning::grid_dims`] × [`Tuning::voxel_size`]).
    Box { half_extents: Vec3 },
}

/// A cloud of smoke popping where the entity is, at the current
/// [`SmokeClock::tick`]. A convenience over [`Smokes::pop`] on [`SmokeWorld`]: the entity
/// holds the cloud's look, and (by default) despawns when the cloud is gone.
///
/// Start from a preset ([`Smoke::grenade`], [`Smoke::wall`], [`Smoke::gas`],
/// [`Smoke::fog_bank`], [`Smoke::steam_vent`]) and adjust with the `with_*`
/// builders. A clone is a new cloud with the same settings: it pops its own.
#[derive(Component, Debug)]
#[require(Transform)]
pub struct Smoke {
    /// What it spreads from.
    pub source: SmokeSource,
    /// Volume in cubic world units. `None` uses [`Tuning::volume`] (90 m³ by
    /// default, a CS2-sized smoke).
    pub volume: Option<f32>,
    /// Seconds from the pop until the cloud is gone. `None` uses the tuning's
    /// timeline.
    pub lifetime: Option<f32>,
    /// Seconds the cloud takes to spread to full size. `None` uses the
    /// tuning's timeline.
    pub grow_time: Option<f32>,
    /// Multiplies [`SmokeSettings::albedo`]. Team-coloured smokes, signal smoke...
    pub tint: Color,
    /// Multiplies how dense volumetric smoke looks. A look only: what hides
    /// what is fixed by the tuning.
    pub density: f32,
    /// Fill seed. `None` derives one from the position, so the same throw
    /// gives the same smoke on every machine.
    pub seed: Option<u32>,
    /// Style. `None` uses [`SmokeSettings::style`] at the moment of the pop.
    pub style: Option<SmokeStyle>,
    /// Despawn the entity when the cloud is gone.
    pub despawn_when_done: bool,
    cloud: Option<CloudId>,
}

impl Default for Smoke {
    fn default() -> Self {
        Self {
            source: SmokeSource::Point,
            volume: None,
            lifetime: None,
            grow_time: None,
            tint: Color::WHITE,
            density: 1.0,
            seed: None,
            style: None,
            despawn_when_done: true,
            cloud: None,
        }
    }
}

impl Clone for Smoke {
    fn clone(&self) -> Self {
        Self { cloud: None, ..*self }
    }
}

impl Smoke {
    /// A smoke grenade: spreads from where it lands to a CS2-sized cloud and
    /// lasts 18 s. The same as `Smoke::default()`.
    pub fn grenade() -> Self {
        Self::default()
    }

    /// A wall of smoke from the entity to `offset` from it, as from a line of
    /// smoke pots. It spreads the default volume along the line.
    pub fn wall(offset: Vec3) -> Self {
        Self {
            source: SmokeSource::Line { offset },
            lifetime: Some(25.0),
            ..default()
        }
    }

    /// A drifting cloud of greenish poison gas. Use [`SmokeVision::density_at`]
    /// to hurt whoever stands in it.
    pub fn gas() -> Self {
        Self {
            lifetime: Some(25.0),
            grow_time: Some(4.0),
            tint: Color::srgb(0.75, 0.95, 0.35),
            density: 0.55,
            ..default()
        }
    }

    /// A bank of fog filling a box (`half_extents` either side of the
    /// entity) for as long as the entity lives.
    pub fn fog_bank(half_extents: Vec3) -> Self {
        Self {
            source: SmokeSource::Box { half_extents },
            volume: Some(8.0 * half_extents.x * half_extents.y * half_extents.z * 0.9),
            lifetime: Some(1.0e7),
            grow_time: Some(6.0),
            density: 0.6,
            despawn_when_done: false,
            ..default()
        }
    }

    /// Steam hissing from a vent: spreads slowly, white and thin, and clears
    /// after 15 s.
    pub fn steam_vent() -> Self {
        Self {
            lifetime: Some(15.0),
            grow_time: Some(8.0),
            tint: Color::srgb(1.0, 1.0, 1.0),
            density: 0.5,
            ..default()
        }
    }

    /// Spread from `source` instead.
    pub fn with_source(mut self, source: SmokeSource) -> Self {
        self.source = source;
        self
    }

    /// Take `seconds` to spread to full size.
    pub fn with_grow_time(mut self, seconds: f32) -> Self {
        self.grow_time = Some(seconds);
        self
    }

    /// Fill `volume` cubic world units.
    pub fn with_volume(mut self, volume: f32) -> Self {
        self.volume = Some(volume);
        self
    }

    /// Be gone `seconds` after the pop.
    pub fn with_lifetime(mut self, seconds: f32) -> Self {
        self.lifetime = Some(seconds);
        self
    }

    /// Colour the smoke (a look only).
    pub fn with_tint(mut self, tint: Color) -> Self {
        self.tint = tint;
        self
    }

    /// Use a fixed fill seed.
    pub fn with_seed(mut self, seed: u32) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Use this look instead of [`SmokeSettings::style`].
    pub fn with_style(mut self, style: SmokeStyle) -> Self {
        self.style = Some(style);
        self
    }

    /// The cloud in [`SmokeWorld`], once popped.
    pub fn cloud(&self) -> Option<CloudId> {
        self.cloud
    }
}

/// Gameplay queries against every smoke at the current [`SmokeClock::tick`]:
/// bot vision, server-side visibility, audio occlusion. The same answers the
/// core gives anywhere else, in world units.
#[derive(SystemParam)]
pub struct SmokeVision<'w> {
    world: ResMut<'w, SmokeWorld>,
    clock: Res<'w, SmokeClock>,
    settings: Res<'w, SmokeSettings>,
}

impl SmokeVision<'_> {
    /// The tick the answers are for.
    pub fn tick(&self) -> u64 {
        self.clock.tick
    }

    /// The amount of cloud between two points, every smoke added up, in world
    /// units (see [`Smokes::occlusion`]).
    pub fn occlusion(&mut self, from: Vec3, to: Vec3) -> f32 {
        let tick = self.clock.tick;
        self.world.occlusion(from, to, tick)
    }

    /// Does smoke hide `to` from `from`? True once the segment passes through
    /// [`Tuning::sight_threshold`] of cloud.
    pub fn blocks_sight(&mut self, from: Vec3, to: Vec3) -> bool {
        let tick = self.clock.tick;
        self.world.blocks_sight(from, to, tick)
    }

    /// Fraction of light (0..1) that gets through, from the amount of cloud
    /// and [`SmokeSettings::extinction`]: for audio and other soft effects.
    pub fn transmittance(&mut self, from: Vec3, to: Vec3) -> f32 {
        let tick = self.clock.tick;
        let amount = self.world.occlusion(from, to, tick);
        let metres = self.world.units().length_to_internal(amount);
        (-metres * self.settings.extinction).exp()
    }

    /// Density (0..1 per smoke, summed) at a point.
    pub fn density_at(&mut self, p: Vec3) -> f32 {
        let tick = self.clock.tick;
        self.world.density_at(p, tick)
    }
}

/// Which entity owns which cloud, to remove clouds whose entity went away.
#[derive(Resource, Default)]
struct SmokeEntities(HashMap<Entity, CloudId>);

/// Sun and sky visibility per cloud, worked out on a background thread.
#[derive(Resource, Default)]
struct SmokeLighting(HashMap<CloudId, Light>);

enum Light {
    Pending(Task<Vec<[u8; 2]>>),
    Ready(Arc<Vec<[u8; 2]>>),
}

/// Which GPU atlas slot holds which cloud.
#[derive(Resource, Default)]
struct SlotTable {
    owners: Vec<Option<SlotOwner>>,
    warned_full: bool,
}

struct SlotOwner {
    cloud: CloudId,
    /// Uploaded with lighting (else with everything fully lit, for now).
    lit: bool,
    /// Bake this frame even if nothing else changed.
    force_bake: bool,
    /// Open holes last frame: a change means one just closed.
    holes: u32,
}

impl SlotTable {
    fn slot_of(&self, cloud: CloudId) -> Option<u32> {
        self.owners
            .iter()
            .position(|o| o.as_ref().is_some_and(|o| o.cloud == cloud))
            .map(|i| i as u32)
    }

    fn allocate(&mut self, max: u32, cloud: CloudId) -> Option<u32> {
        if self.owners.len() < max as usize {
            self.owners.resize_with(max as usize, || None);
        }
        let slot = self.owners.iter().position(Option::is_none)?;
        self.owners[slot] = Some(SlotOwner {
            cloud,
            lit: false,
            force_bake: true,
            holes: 0,
        });
        Some(slot as u32)
    }
}

/// Direction towards the smoke's sun, plus its colour times illuminance.
fn find_sun(lights: &Query<(&DirectionalLight, &GlobalTransform, Has<SmokeSun>)>) -> Option<(Vec3, Vec3)> {
    lights
        .iter()
        .max_by_key(|(_, _, marked)| *marked)
        .map(|(light, transform, _)| {
            (
                transform.back().as_vec3(),
                light.color.to_linear().to_vec3() * light.illuminance,
            )
        })
}

fn advance_clock(mut clock: ResMut<SmokeClock>, time: Res<Time<Virtual>>, config: Res<SmokeConfig>) {
    if clock.auto {
        let tick = (time.elapsed_secs_f64() * config.tick_rate as f64).floor() as u64;
        if clock.tick != tick {
            clock.tick = tick;
        }
    }
}

fn sync_level(level: Option<Res<SmokeLevel>>, mut world: ResMut<SmokeWorld>) {
    if let Some(level) = level
        && level.is_changed()
    {
        world.set_level(level.0.clone());
    }
}

#[allow(clippy::too_many_arguments)]
fn pop_smokes(
    mut smokes: Query<(Entity, &mut Smoke, &GlobalTransform)>,
    mut world: ResMut<SmokeWorld>,
    mut entities: ResMut<SmokeEntities>,
    clock: Res<SmokeClock>,
    settings: Res<SmokeSettings>,
    level: Option<Res<SmokeLevel>>,
    obstacles: Res<obstacles::ObstacleBuild>,
    mut warned: Local<bool>,
) {
    // The first level built from marked meshes is on its way: wait for it.
    if level.is_none() && obstacles.pending() {
        return;
    }
    for (entity, mut smoke, transform) in &mut smokes {
        if smoke.cloud.is_some() {
            continue;
        }
        if level.is_none() && !*warned {
            *warned = true;
            warn!(
                "No SmokeLevel resource: smoke only knows a flat ground at height 0 and will pass through walls. Insert SmokeLevel with your level geometry before smokes pop."
            );
        }
        let origin = transform.translation();
        let seed = smoke.seed.unwrap_or_else(|| CloudSpec::seed_from_origin(origin));
        let mut spec = CloudSpec::new(origin, clock.tick, seed, smoke.style.unwrap_or(settings.style));
        spec.source = match smoke.source {
            SmokeSource::Point => CloudSource::Point,
            SmokeSource::Line { offset } => CloudSource::Line { end: origin + offset },
            SmokeSource::Box { half_extents } => CloudSource::Box { half_extents },
        };
        spec.volume = smoke.volume;
        if smoke.lifetime.is_some() || smoke.grow_time.is_some() {
            let base = world.config().tuning.timeline;
            spec.timeline = Some(SmokeTimeline {
                lifetime: smoke.lifetime.unwrap_or(base.lifetime),
                grow_time: smoke.grow_time.unwrap_or(base.grow_time),
                ..base
            });
        }
        let id = world.pop(spec);
        smoke.cloud = Some(id);
        entities.0.insert(entity, id);
    }
}

fn apply_impacts(
    mut bullets: MessageReader<SmokeBullet>,
    mut explosions: MessageReader<SmokeExplosion>,
    mut world: ResMut<SmokeWorld>,
    clock: Res<SmokeClock>,
) {
    for b in bullets.read() {
        world.bullet(b.start, b.end, clock.tick);
    }
    for e in explosions.read() {
        world.explosion(e.center, e.radius, clock.tick);
    }
}

fn advance_world(mut world: ResMut<SmokeWorld>, clock: Res<SmokeClock>) {
    world.advance(clock.tick);
}

/// Despawn entities whose cloud is gone, and forget clouds whose entity is.
fn retire_smokes(
    mut commands: Commands,
    smokes: Query<(Entity, &Smoke)>,
    mut removed: RemovedComponents<Smoke>,
    mut world: ResMut<SmokeWorld>,
    mut entities: ResMut<SmokeEntities>,
    clock: Res<SmokeClock>,
) {
    for entity in removed.read() {
        if let Some(id) = entities.0.remove(&entity) {
            world.remove(id);
        }
    }
    for (entity, smoke) in &smokes {
        let Some(id) = smoke.cloud else {
            continue;
        };
        let gone = world.cloud(id).is_none_or(|c| clock.tick >= c.end_tick());
        if gone && smoke.despawn_when_done {
            commands.entity(entity).despawn();
        }
    }
}

/// Start (and collect) the background work out how much sun and sky each
/// cloud's voxels see.
fn light_clouds(
    world: Res<SmokeWorld>,
    mut lighting: ResMut<SmokeLighting>,
    lights: Query<(&DirectionalLight, &GlobalTransform, Has<SmokeSun>)>,
) {
    let units = *world.units();
    let sun = find_sun(&lights).map(|(dir, _)| dir);
    let rays = LightRays {
        sun,
        sun_length: units.length_from_internal(60.0),
        sky_length: units.length_from_internal(20.0),
    };
    for cloud in world.clouds() {
        let Some(shape) = cloud.shape() else {
            continue;
        };
        // Crystals are lit by Bevy's own renderer.
        if lighting.0.contains_key(&cloud.id()) || shape.is_empty() || cloud.spec().style == SmokeStyle::Crystal {
            continue;
        }
        let (shape, level) = (shape.clone(), world.level().clone());
        let task = AsyncComputeTaskPool::get().spawn(async move { visibility(&shape.grid, &level, &units, &rays) });
        lighting.0.insert(cloud.id(), Light::Pending(task));
    }
    for light in lighting.0.values_mut() {
        if let Light::Pending(task) = light
            && let Some(done) = check_ready(task)
        {
            *light = Light::Ready(Arc::new(done));
        }
    }
    lighting.0.retain(|id, _| world.cloud(*id).is_some());
}

/// The box whose screen rectangle a smoke is dispatched over this tick: what
/// it occupies now ([`smokescreen::CloudState::active`]), much smaller than
/// its whole box while it blooms and fades. Volumetric: outside it every
/// voxel a sample reads is past the growth front or eaten by the fade, so a
/// ray that misses it gathers nothing. Faceted: `active` covers each puff's
/// bounding sphere plus the blend radius, but blended puffs can in principle
/// reach up to (corner ratio − 1) × blend further, so it is padded by that.
/// Rays still march from where they enter the whole box, so the picture is
/// the same as without it. Nothing occupied: an empty box.
fn drawn_bounds(state: &smokescreen::CloudState, facets: Option<&smokescreen::Facets>) -> (Vec3, Vec3) {
    let (lo, hi) = state.bounds;
    let Some((alo, ahi)) = state.active else {
        return (lo, lo);
    };
    let pad = facets.map_or(0.0, |f| (Polyhedron::Cube.corner_ratio() - 1.0) * f.settings.blend);
    ((alo - pad).max(lo), (ahi + pad).min(hi))
}

#[allow(clippy::too_many_arguments)]
fn build_frame(
    mut frame: ResMut<SmokeFrame>,
    mut world: ResMut<SmokeWorld>,
    mut slots: ResMut<SlotTable>,
    smokes: Query<&Smoke>,
    lighting: Res<SmokeLighting>,
    uploads: Res<SmokeUploads>,
    clock: Res<SmokeClock>,
    settings: Res<SmokeSettings>,
    config: Res<SmokeConfig>,
    dir_lights: Query<(&DirectionalLight, &GlobalTransform, Has<SmokeSun>)>,
    point_lights: Query<(&PointLight, &GlobalTransform, &ViewVisibility)>,
    ambient: Option<Res<GlobalAmbientLight>>,
    time: Res<Time>,
    mut last: Local<Option<(Vec3, u64)>>,
) {
    let frame = frame.as_mut();
    frame.slots.clear();
    frame.holes.clear();
    frame.lights.clear();
    frame.bake.clear();
    frame.facet_puffs.clear();

    let units = config.units;
    let sun = find_sun(&dir_lights);
    frame.has_sun = sun.is_some();
    if let Some((dir, color)) = sun {
        frame.sun_direction = units.dir_to_internal(dir);
        frame.sun_color = color * settings.sun_intensity;
    } else {
        frame.sun_direction = Vec3::Y;
        frame.sun_color = Vec3::ZERO;
    }
    let ambient = settings.ambient_override.map(|c| c.to_vec3()).unwrap_or_else(|| {
        ambient
            .map(|a| a.color.to_linear().to_vec3() * a.brightness)
            .unwrap_or(Vec3::ZERO)
    });
    frame.ambient = ambient * settings.ambient_intensity;
    frame.settings = settings.clone();
    frame.settings.wind = units.to_internal(settings.wind);
    frame.sight_threshold = world.internal_tuning().sight_threshold;
    frame.voxel_size = config.voxel_size;
    frame.grid_dims = config.grid_dims;
    frame.internal_from_world = config.internal_from_world();
    frame.time = time.elapsed_secs_wrapped();
    frame.frame = frame.frame.wrapping_add(1);

    // Re-bake everything when lighting-relevant settings or the sun move.
    let fingerprint = {
        let s = &settings;
        let bits = [s.extinction, s.multi_scattering, s.ambient_occlusion, s.indoor_ambient]
            .iter()
            .fold(0u64, |acc, v| acc.rotate_left(21) ^ v.to_bits() as u64);
        (frame.sun_direction, bits)
    };
    let global_rebake = match *last {
        Some((dir, bits)) => dir.dot(fingerprint.0) < 0.99999 || bits != fingerprint.1,
        None => true,
    };
    *last = Some(fingerprint);

    // Looks of clouds that came from a `Smoke` entity.
    let looks: HashMap<CloudId, (Vec3, f32)> = smokes
        .iter()
        .filter_map(|s| s.cloud.map(|id| (id, (s.tint.to_linear().to_vec3(), s.density))))
        .collect();

    let view = world.view(clock.drawn());
    // Free the slots of clouds no longer drawn.
    for owner in slots.owners.iter_mut() {
        if owner
            .as_ref()
            .is_some_and(|o| !view.clouds().any(|c| c.state.id == o.cloud))
        {
            *owner = None;
        }
    }
    let slots = slots.as_mut();
    // Crystals are drawn as meshes (see `crystal`), not from the atlas.
    for cloud in view.clouds().filter(|c| c.spec.style != SmokeStyle::Crystal) {
        if frame.slots.len() >= render::MAX_SLOTS {
            break;
        }
        let id = cloud.state.id;
        let light = match lighting.0.get(&id) {
            Some(Light::Ready(l)) => Some(l.clone()),
            _ => None,
        };
        let layer = match slots.slot_of(id) {
            Some(layer) => layer,
            None => match slots.allocate(config.max_smokes, id) {
                Some(layer) => {
                    uploads.0.lock().unwrap().push(SlotUpload {
                        layer,
                        shape: cloud.shape.clone(),
                        lighting: light.clone(),
                    });
                    if let Some(owner) = slots.owners[layer as usize].as_mut() {
                        owner.lit = light.is_some();
                    }
                    layer
                }
                None => {
                    if !slots.warned_full {
                        slots.warned_full = true;
                        warn!(
                            "All {} smoke slots are in use; extra smokes are simulated but not drawn until a slot frees up. Raise Tuning::max_clouds (at most 32) if this is expected.",
                            config.max_smokes
                        );
                    }
                    continue;
                }
            },
        };
        let Some(owner) = slots.owners[layer as usize].as_mut() else {
            continue;
        };
        // Lighting arrived after the first upload: upload again with it.
        if !owner.lit
            && let Some(light) = &light
        {
            uploads.0.lock().unwrap().push(SlotUpload {
                layer,
                shape: cloud.shape.clone(),
                lighting: Some(light.clone()),
            });
            owner.lit = true;
            owner.force_bake = true;
        }

        let state = cloud.state;
        let hole_start = frame.holes.len() as u32;
        for h in &state.holes {
            if frame.holes.len() >= render::MAX_HOLES {
                break;
            }
            frame.holes.push(*h);
        }
        let hole_count = frame.holes.len() as u32 - hole_start;
        let needs_bake = global_rebake
            || owner.force_bake
            || cloud.timeline.is_growing(state.age)
            || state.dissolve > 0.0
            || hole_count > 0
            || hole_count != owner.holes;
        owner.force_bake = false;
        owner.holes = hole_count;
        if needs_bake {
            frame.bake.push(frame.slots.len() as u32);
        }

        // Faceted clouds: this tick's puff sizes and orientations.
        let facet = cloud.shape.facets.as_ref().map(|f| {
            let puff_offset = frame.facet_puffs.len() as u32;
            frame.facet_puffs.extend(state.live.iter().map(render::pack_puff));
            FrameFacet {
                bins_min: f.bins.min,
                cell: f.bins.cell,
                dims: f.bins.dims.as_uvec3(),
                puff_offset,
                puff_count: state.live.len() as u32,
                blend: f.settings.blend,
                region_pad: cloud.shape.grid.voxel_size * 0.5,
                puff_size: f.settings.size,
            }
        });
        // Offset into the tiling noise so neighbouring smokes don't billow in sync.
        let noise_seed = (smokescreen::math::hash_u32(cloud.spec.seed) >> 8) as f32 / (1u32 << 24) as f32 * 64.0;
        let (tint, density) = looks.get(&id).copied().unwrap_or((Vec3::ONE, 1.0));
        let (drawn_min, drawn_max) = drawn_bounds(state, cloud.shape.facets.as_ref());
        frame.slots.push(FrameSlot {
            layer,
            grid_min: cloud.shape.grid.min,
            voxel_size: cloud.shape.grid.voxel_size,
            bounds_min: state.bounds.0,
            bounds_max: state.bounds.1,
            drawn_min,
            drawn_max,
            growth: state.growth,
            dissolve: state.dissolve,
            density,
            tint,
            seed: noise_seed,
            hole_start,
            hole_count,
            facet,
        });
    }
    if slots.owners.iter().any(Option::is_none) {
        slots.warned_full = false;
    }

    // Point lights near any smoke, brightest first, in the internal frame.
    if !frame.slots.is_empty() {
        let m = units.metres_per_unit;
        let mut candidates: Vec<FrameLight> = point_lights
            .iter()
            .filter(|(light, _, vis)| vis.get() && light.intensity > 0.0)
            .filter_map(|(light, transform, _)| {
                let p = units.to_internal(transform.translation());
                let r = light.range * m;
                let near_smoke = frame.slots.iter().any(|s| {
                    let closest = p.clamp(s.bounds_min, s.bounds_max);
                    closest.distance_squared(p) < r * r
                });
                near_smoke.then(|| FrameLight {
                    position: p,
                    range: r,
                    // Lumens to candela, matching Bevy's point lights; Bevy
                    // measures falloff in world units, so scale it to metres.
                    color: light.color.to_linear().to_vec3() * light.intensity / (4.0 * core::f32::consts::PI)
                        * (m * m),
                })
            })
            .collect();
        candidates.sort_by(|a, b| b.color.max_element().total_cmp(&a.color.max_element()));
        candidates.truncate(render::MAX_LIGHTS);
        frame.lights = candidates;
    }
}
