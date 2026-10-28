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
//! If a setup step is missing (no [`SmokeCamera`], no level, [`SmokeObstacle`]
//! on an entity without a mesh, and so on), the plugin says so in the log,
//! once, with the fix.
//!
//! ## Versions
//!
//! `bevy_smokescreen` 0.2 is for Bevy 0.20. Later 0.2 releases may add to
//! the API without breaking code written the way these docs write it:
//!
//! - enums marked `#[non_exhaustive]` may gain variants, so match them with
//!   a `_` arm;
//! - structs whose fields are all public may gain fields, so build them
//!   from a constructor ([`SmokePlugin::new`], [`SmokeCamera::HIGH`]) or with
//!   `..default()` ([`SmokeSettings`], [`SmokeCamera`], [`SmokeClock`],
//!   [`SmokeBullet`], [`SmokeExplosion`], and the core's settings the same
//!   way);
//! - a [`Smoke`] is built with its presets and builders, and may gain both.
//!
//! The simulation's results follow the core's rule: 0.2 releases avoid
//! changing them where they can, but run the same version on every machine
//! in a match. Items hidden from these docs may change in any release.
//!
//! The README in the repository root has a longer guide: presets, level
//! setup, gameplay queries, networking and tuning. For coding assistants,
//! `llms.txt` (in the repository, and at the root of this crate's package)
//! maps the API on one page, with recipes.

#![warn(missing_docs)]

mod crystal;
mod noise;
mod obstacles;
pub mod quality;
mod render;
mod setup;

use std::sync::Arc;

use bevy::{
    asset::AssetEventSystems,
    camera::visibility::VisibilitySystems,
    ecs::system::SystemParam,
    light::{DirectionalLight, GlobalAmbientLight, PointLight},
    platform::collections::HashMap,
    prelude::*,
    render::{RenderApp, RenderPlugin, extract_component::ExtractComponent},
    tasks::{AsyncComputeTaskPool, Task, futures::check_ready},
    transform::{TransformPlugin, TransformSystems},
};

pub use crystal::CrystalPuff;
pub use obstacles::SmokeObstacle;
pub use quality::{FacetOcclusion, FacetQuality, FacetShadows, SmokeQuality, VolumetricQuality};
pub use smokescreen;
/// How a smoke is shaped: soft volumetric density, merged facets, or
/// separate crystals.
pub use smokescreen::Style as SmokeStyle;
/// A cloud's timing (see [`smokescreen::Timeline`]).
pub use smokescreen::Timeline as SmokeTimeline;
/// Every smoke at one tick ([`smokescreen::View`]): [`SmokeVision::view`]
/// and [`Smokes::view`] give one. It answers the same questions from any
/// number of threads, without allocating.
pub use smokescreen::View as SmokeView;
pub use smokescreen::{
    BoxObstacles, CloudId, CloudSource, CloudSpec, Config, ConfigError, FacetRule, FacetSettings, FlatGround,
    HoleSettings, Level, ObstacleBox, Polyhedron, ShapeMix, Smokes, Tuning, Units, UpAxis, VolumeSteps,
    VoxelBytesError, VoxelObstacles,
};

use render::{FrameFacet, FrameLight, FrameSlot, SlotUpload, SmokeFrame, SmokeUploads};
use setup::{Mistake, SetupWarnings, shown};
use smokescreen::light::{LightRays, visibility};

/// What most apps use: `use bevy_smokescreen::prelude::*;`.
pub mod prelude {
    pub use crate::{
        BoxObstacles, CrystalPuff, DefaultSmokeStyle, FacetQuality, FacetRule, FacetSettings, FlatGround,
        NoiseWithinRule, ObstacleBox, Polyhedron, ShapeMix, Smoke, SmokeBullet, SmokeCamera, SmokeClock,
        SmokeExplosion, SmokeLevel, SmokeObstacle, SmokePlugin, SmokeQuality, SmokeResolution, SmokeSettings,
        SmokeSource, SmokeStyle, SmokeSun, SmokeSystems, SmokeView, SmokeVision, SmokeWorld, Units, UpAxis,
        VolumeSteps, VolumetricQuality, VoxelObstacles,
    };
}

/// Compiles the README's examples as doc tests, so they keep up with the API.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;

/// Compiles llms.txt's recipes as doc tests, so they keep up with the API.
#[cfg(doctest)]
#[doc = include_str!("../llms.txt")]
pub struct LlmsTxtDoctests;

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
///
/// Build it with [`SmokePlugin::new`] or `default()`: later versions may
/// add fields.
#[derive(Clone, Debug)]
pub struct SmokePlugin {
    /// The simulation's configuration: units, ticks per second and the rules.
    pub config: Config,
}

impl SmokePlugin {
    /// The plugin with your [`Config`]: units, ticks per second and rules.
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Default for SmokePlugin {
    /// Y-up metres at 128 ticks a second, with the default rules.
    fn default() -> Self {
        Self::new(Config::new(Units::METRES_Y_UP, 128.0))
    }
}

/// System sets, in order, all in `PostUpdate` after transform propagation.
/// Write [`SmokeBullet`]/[`SmokeExplosion`] messages in `Update` (or anywhere
/// before [`SmokeSystems::Simulate`]). Later versions may add sets.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SmokeSystems {
    /// Pop new smokes, apply impacts, advance the simulation to
    /// [`SmokeClock::tick`], then prepare [`SmokeVision`]'s views again, so
    /// systems after it see this frame's pops and impacts. It ends before
    /// Bevy's visibility systems start (see [`SmokeSystems::PrepareRender`]),
    /// so a system ordered after them can't be ordered before this set.
    Simulate,
    /// Gather everything the renderer needs and pick each camera's tier.
    /// Only with Bevy's `RenderPlugin`: a server has none of this work.
    /// Crystal puffs are moved, shown and hidden first, before Bevy's
    /// [`VisibilitySystems::VisibilityPropagate`] and
    /// [`VisibilitySystems::CalculateBounds`] and its asset events, so each
    /// puff is drawn in the frame the rule counts it. Point lights are read
    /// after Bevy's [`VisibilitySystems::CheckVisibility`], which decides
    /// which of them can reach a camera's view.
    PrepareRender,
}

impl Plugin for SmokePlugin {
    fn build(&self, app: &mut App) {
        if let Err(e) = self.config.validate() {
            panic!("SmokePlugin: {e}");
        }
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
        let mut uploads = SmokeUploads::default();
        uploads.drained = render::build(app, config, uploads.clone());
        // What `SmokeVision` answers from until the clock first moves.
        let mut world = SmokeWorld(world);
        world.prepare_vision(&SmokeClock::default());

        app.insert_resource(config)
            .insert_resource(world)
            .insert_resource(uploads)
            .init_resource::<SmokeClock>()
            .init_resource::<SmokeSettings>()
            .init_resource::<DefaultSmokeStyle>()
            .init_resource::<SlotTable>()
            .init_resource::<SmokeLighting>()
            .init_resource::<SmokeEntities>()
            .init_resource::<SmokeFrame>()
            .init_resource::<SetupWarnings>()
            .init_resource::<crystal::CrystalClouds>()
            .init_resource::<obstacles::ObstacleBuild>()
            .add_message::<SmokeBullet>()
            .add_message::<SmokeExplosion>()
            // A clock inserted with the app, for Startup systems.
            .add_systems(PreStartup, prepare_views)
            .add_systems(PreUpdate, (advance_clock, prepare_views).chain())
            .configure_sets(
                PostUpdate,
                (SmokeSystems::Simulate, SmokeSystems::PrepareRender)
                    .chain()
                    .after(TransformSystems::Propagate),
            )
            .add_systems(
                PostUpdate,
                (
                    obstacles::voxelise_obstacles,
                    sync_level,
                    pop_smokes,
                    apply_impacts,
                    advance_world,
                    retire_smokes,
                    prepare_views,
                    warn_of_clouds,
                )
                    .chain()
                    .in_set(SmokeSystems::Simulate),
            );
    }

    fn finish(&self, app: &mut App) {
        render::finish(app);
        // Drawing, only where something draws: a server (MinimalPlugins)
        // does none of its work each frame.
        if app.is_plugin_added::<RenderPlugin>() {
            app.add_systems(Startup, crystal::setup_meshes).add_systems(
                PostUpdate,
                (
                    (
                        // Before Bevy works out what is visible and sends
                        // asset events: a puff the rule counts is culled
                        // where it is now and drawn this frame, a new cloud's
                        // material with it.
                        crystal::sync_crystals
                            .before(VisibilitySystems::VisibilityPropagate)
                            .before(VisibilitySystems::CalculateBounds)
                            .before(AssetEventSystems),
                        light_clouds,
                        build_frame,
                        quality::update_smoke_quality,
                    )
                        .chain()
                        .in_set(SmokeSystems::PrepareRender),
                    // Reads `ViewVisibility`, which Bevy has cleared until then.
                    gather_point_lights
                        .after(build_frame)
                        .after(VisibilitySystems::CheckVisibility)
                        .in_set(SmokeSystems::PrepareRender),
                ),
            );
        }
        if !app.is_plugin_added::<TransformPlugin>() {
            app.add_systems(
                PostUpdate,
                warn_without_transforms
                    .before(pop_smokes)
                    .in_set(SmokeSystems::Simulate),
            );
        }
    }
}

/// A level shared between the simulation and lighting threads.
pub type SharedLevel = Arc<dyn Level + Send + Sync>;

/// Fixed configuration derived from [`SmokePlugin`]: what the renderer sizes
/// its buffers by. Lengths in metres (the core's internal frame).
#[derive(Resource, Clone, Copy, Debug)]
pub(crate) struct SmokeConfig {
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
/// with the server's ticks. Everything in it is drawn. A cloud popped or
/// inserted here directly has the default look (no tint, density 1) and no
/// entity, and lasts until it ends or you remove it; for a server's cloud
/// with a look and an entity, spawn [`Smoke::from_spec`]. It derefs to
/// [`Smokes`], so call those methods on it.
///
/// Its queries answer for any tick, with every change made so far:
/// [`Smokes::view`] gives one tick's [`SmokeView`] to share between threads
/// (lag compensation at the tick a shooter saw, say). [`SmokeVision`] needs
/// only read access instead, and answers as of the views the plugin
/// prepares (see [`SmokeWorld::prepare_vision`]).
#[derive(Resource, Debug, Deref, DerefMut)]
pub struct SmokeWorld(Smokes<SharedLevel>);

/// The pins of [`SmokeVision`]'s views (see [`Smokes::pin_view`]).
const VISION_PIN: usize = 0;
const DRAWN_PIN: usize = 1;

impl SmokeWorld {
    /// Prepares [`SmokeVision`]'s views of `clock`'s tick and drawn tick
    /// from the world as it is now. The plugin does this when the clock
    /// moves (in `PreUpdate`) and again at the end of
    /// [`SmokeSystems::Simulate`], after the frame's pops and impacts, so
    /// most games never call it.
    ///
    /// Call it after moving the clock yourself between those points (a
    /// fixed-step game that sets [`SmokeClock::tick`] in `FixedUpdate`,
    /// say), or after changing the world directly when systems before
    /// `PostUpdate` should see it. Until then `SmokeVision` answers as
    /// before: for the tick it was prepared for (it warns once when the
    /// clock has moved on), and without the changes.
    pub fn prepare_vision(&mut self, clock: &SmokeClock) {
        self.0.pin_view(VISION_PIN, clock.tick);
        self.0.pin_view(DRAWN_PIN, clock.drawn());
    }
}

/// The tick the smoke is simulated and drawn at.
///
/// By default the plugin counts ticks from Bevy's virtual time
/// ([`Config::tick_rate`] per second), so pausing virtual time freezes the
/// smoke. To drive smoke from your own simulation, set `auto` to `false` and
/// write `tick` (and `draw_tick` and `draw_fraction`) yourself before
/// `PostUpdate`. [`SmokeVision`] answers for the tick the clock had when
/// the plugin prepared its views in `PreUpdate`: write the clock in `First`,
/// or call [`SmokeWorld::prepare_vision`] after writing it.
///
/// Volumetric smoke's noise moves on this clock too, not on each machine's
/// own: players who draw the same tick and fraction with the same
/// [`SmokeSettings`] see the same billows.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct SmokeClock {
    /// The newest simulation tick: fills advance to it, impacts land on it,
    /// [`Smoke`] entities pop on it unless they bring their own
    /// ([`Smoke::pop_tick`], [`Smoke::from_spec`]), and [`SmokeVision`]
    /// answers for it.
    pub tick: u64,
    /// The tick drawn, for example your interpolation tick slightly behind
    /// `tick`. `None` draws `tick`.
    pub draw_tick: Option<u64>,
    /// How far the frame is past the drawn tick, from 0 to 1: your
    /// interpolation fraction. Only the volumetric noise reads it, so that it
    /// moves smoothly between ticks; left at 0, it moves once a tick.
    pub draw_fraction: f32,
    /// Count `tick` and `draw_fraction` from `Time<Virtual>` every frame.
    pub auto: bool,
}

impl Default for SmokeClock {
    fn default() -> Self {
        Self {
            tick: 0,
            draw_tick: None,
            draw_fraction: 0.0,
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

/// Look of all smokes. Change at runtime freely, and per player (a graphics
/// menu, say): nothing here changes what hides what. That is the rules:
/// [`Tuning`], fixed in the plugin's [`Config`], and each smoke's style
/// ([`DefaultSmokeStyle`], [`Smoke::with_style`]).
///
/// Lengths here are in metres whatever [`Config::units`] says: the
/// extinction is per metre and the noise frequency in tiles per metre. Only
/// `wind` is in world units.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct SmokeSettings {
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
    /// a smoke camera is lit by an environment map or its own
    /// [`AmbientLight`] (the plugin warns): the smoke's lighting is the same
    /// for every camera.
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

/// The style of smokes that don't choose their own ([`Smoke::style`] is
/// `None`), read when each one pops. Volumetric by default.
///
/// The style is a rule, not a look: it decides what hides what. In a
/// multiplayer game every machine must give a smoke the same one, so set
/// this the same everywhere (or give each smoke its own with
/// [`Smoke::with_style`]); a cloud from [`Smoke::from_spec`] takes its
/// spec's. Changing it changes only smokes that pop afterwards.
///
/// ```no_run
/// # use bevy::prelude::*;
/// # use bevy_smokescreen::prelude::*;
/// App::new()
///     .add_plugins((DefaultPlugins, SmokePlugin::default()))
///     // Every smoke crystal, unless it says otherwise.
///     .insert_resource(DefaultSmokeStyle(SmokeStyle::Crystal));
/// ```
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deref, DerefMut)]
pub struct DefaultSmokeStyle(pub SmokeStyle);

/// How volumetric smoke's noise relates to the rule ([`SmokeSettings::noise_within_rule`]).
///
/// The noise only ever thins the smoke, so it can show through some of what
/// the rule ([`Tuning::sight_threshold`]) blocks, most while a cloud fades.
/// Every player sees the same thinning, so between players that is fair; a
/// server, bots or abilities that use the rule see less than players there.
/// For those games, `Capped` and `Filled` keep volumetric smoke opaque on
/// every line the rule blocks (at [`Smoke::density`] 1 or more), each with a
/// different cost. Both leave thin smoke (wisps, edges, silhouettes) all its
/// noise. Later versions may add modes, so a `match` on it needs a `_` arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
/// fn load_level(mut commands: Commands) {
///     let boxes = vec![ObstacleBox::from_center_size(Vec3::new(0.0, 1.5, 4.0), Vec3::new(8.0, 3.0, 0.2))];
///     // Pre-voxelise once so every smoke is just bit lookups.
///     let voxels = VoxelObstacles::from_level(
///         Vec3::new(-50.0, -1.0, -50.0), Vec3::new(50.0, 10.0, 50.0), 0.3,
///         &BoxObstacles::new(boxes),
///     );
///     commands.insert_resource(SmokeLevel::new(voxels));
/// }
/// ```
#[derive(Resource, Clone)]
pub struct SmokeLevel(
    /// The level: anything behind [`Level`], in world units.
    pub SharedLevel,
);

impl SmokeLevel {
    /// The level from anything behind [`Level`], in world units.
    pub fn new(level: impl Level + Send + Sync + 'static) -> Self {
        Self(Arc::new(level))
    }
}

/// Marks the directional light that lights volumetric and faceted smoke.
/// Without it, the only directional light is used; with several, mark one
/// (the plugin warns if none or more than one is marked).
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct SmokeSun;

/// A bullet passing through smoke: a tunnel that closes in about 2 s.
///
/// Build it with `..default()` (`SmokeBullet { start, end, ..default() }`):
/// later versions may add fields.
#[derive(Message, Clone, Copy, Debug, Default, PartialEq)]
pub struct SmokeBullet {
    /// Where the shot starts, in world units.
    pub start: Vec3,
    /// Where it ends: what it hit, or the end of its range.
    pub end: Vec3,
    /// The tick the shot was fired on. `None` lands it on the current
    /// [`SmokeClock::tick`]. In a networked game, give it the shot's own
    /// tick (as [`Smoke::with_pop_tick`] does for a pop): a shot heard late,
    /// or replayed, then opens the same tunnel on every machine. A tick
    /// still to come opens it then.
    pub tick: Option<u64>,
}

/// An explosion inside or near smoke: a cleared sphere that refills over
/// about 5 s.
///
/// Build it with `..default()` (`SmokeExplosion { center, ..default() }`):
/// later versions may add fields.
#[derive(Message, Clone, Copy, Debug, Default, PartialEq)]
pub struct SmokeExplosion {
    /// Where it went off, in world units.
    pub center: Vec3,
    /// Radius of the cleared sphere. `None` uses [`HoleSettings::explosion_radius`].
    pub radius: Option<f32>,
    /// The tick it went off on. `None` lands it on the current
    /// [`SmokeClock::tick`]; see [`SmokeBullet::tick`].
    pub tick: Option<u64>,
}

/// Add to any `Camera3d` that should see smoke. Holds the camera's
/// performance dial.
///
/// Volumetric and faceted smoke is drawn only through cameras with it, and
/// only perspective ones; the plugin warns if no active camera has it, or if
/// one is orthographic. Crystal smoke is plain meshes, drawn by any camera.
///
/// ```no_run
/// # use bevy_smokescreen::prelude::*;
/// // Fixed quality, 0 (cheapest) ..= 1 (best), in six steps:
/// let fixed = SmokeCamera { quality: 0.6, ..Default::default() };
/// // Or: best quality that fits 1.5 ms of GPU time, re-decided every frame.
/// let budgeted = SmokeCamera::budget(1.5);
/// ```
#[derive(Component, Clone, Copy, Debug, PartialEq, ExtractComponent)]
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
    /// March volumetric smoke as coarse as where the rays actually see smoke
    /// allows ([`VolumetricQuality::resolution_by_distance`]), measured on
    /// the GPU and read back a few frames later
    /// ([`SmokeQuality::measured_depth`]). Inside a dense cloud the rays end
    /// long before its far side, so close up the march can go to a quarter
    /// where the far side alone allows a half. Off, the far side of the
    /// farthest smoke's box decides, as in 0.1. On by default.
    pub measured_depth: bool,
}

/// Fraction of the camera resolution (per axis) the volumetric smoke is
/// raymarched at. Later versions may add fractions, so a `match` on it needs
/// a `_` arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SmokeResolution {
    /// Every pixel.
    #[default]
    Full,
    /// Two thirds per axis: about 44 % of the pixels.
    TwoThirds,
    /// Half per axis: a quarter of the pixels.
    Half,
    /// A quarter per axis: a sixteenth of the pixels.
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
    /// The cheapest of the dial's six steps ([`quality::TIER_NAMES`]),
    /// `quality` 0.
    pub const MINIMUM: Self = Self::with_quality(0.0);
    /// The second step, `quality` 0.2.
    pub const LOWER: Self = Self::with_quality(0.2);
    /// The third step, `quality` 0.4.
    pub const LOW: Self = Self::with_quality(0.4);
    /// The fourth step, `quality` 0.6.
    pub const MEDIUM: Self = Self::with_quality(0.6);
    /// The fifth step, `quality` 0.8: the default.
    pub const HIGH: Self = Self::with_quality(0.8);
    /// The best, `quality` 1.
    pub const ULTRA: Self = Self::with_quality(1.0);

    /// A fixed quality, 0 (cheapest) ..= 1 (best).
    pub const fn with_quality(quality: f32) -> Self {
        Self {
            quality,
            budget_ms: None,
            temporal_jitter: false,
            volumetric: None,
            facets: None,
            measured_depth: true,
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

/// What a [`Smoke`] spreads from, relative to its entity, in world units and
/// world axes: the entity's rotation doesn't turn a line or a box. Later
/// versions may add shapes, so a `match` on it needs a `_` arm.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub enum SmokeSource {
    /// From the entity's position: a grenade.
    #[default]
    Point,
    /// From every point between the entity and `offset` from it: a smoke
    /// wall.
    Line {
        /// The line's other end, from the entity.
        offset: Vec3,
    },
    /// From every open point of a box centred on the entity: a fog bank, a
    /// gas-filled room. The box must fit the fill grid
    /// ([`Tuning::grid_dims`] × [`Tuning::voxel_size`]).
    Box {
        /// Half the box's size along each axis.
        half_extents: Vec3,
    },
}

/// A cloud of smoke popping where the entity is, at the current
/// [`SmokeClock::tick`] (or its [`pop_tick`](Self::pop_tick)). A convenience
/// over [`Smokes::pop`] on [`SmokeWorld`]: the entity holds the cloud's look,
/// and (by default) despawns when the cloud is gone.
///
/// Start from a preset ([`Smoke::grenade`], [`Smoke::wall`], [`Smoke::gas`],
/// [`Smoke::fog_bank`], [`Smoke::steam_vent`]) and adjust with the `with_*`
/// builders. A clone is a new cloud with the same settings: it pops its own.
///
/// In a networked game every machine must pop the same cloud on the same
/// tick. For a cloud a server announced, spawn [`Smoke::from_spec`] with the
/// [`CloudSpec`] it sent: that cloud exactly, with your look. When every
/// machine spawns the same `Smoke` from the same event instead (lockstep,
/// rollback), give it the event's tick with [`Smoke::with_pop_tick`].
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
    /// Seconds from the pop until the cloud starts to grow: a fuse, and the
    /// time its fill has to run in. `None` uses the tuning's timeline
    /// ([`Timeline::start`](smokescreen::Timeline::start)).
    pub start: Option<f32>,
    /// Seconds at the end of the lifetime spent dissolving. `None` uses the
    /// tuning's timeline.
    pub fade_time: Option<f32>,
    /// Multiplies [`SmokeSettings::albedo`]. Team-coloured smokes, signal smoke...
    pub tint: Color,
    /// Multiplies how dense volumetric smoke looks. A look only: what hides
    /// what is fixed by the tuning.
    pub density: f32,
    /// Fill seed. `None` derives one from the position, so the same throw
    /// gives the same smoke on every machine.
    pub seed: Option<u32>,
    /// Style, a rule. `None` uses [`DefaultSmokeStyle`] as it is at the pop.
    pub style: Option<SmokeStyle>,
    /// The tick it pops at. `None` pops at the current [`SmokeClock::tick`].
    /// Set it to the tick of the event that threw the smoke, and a pop heard
    /// late keeps pace with everyone else's: the cloud grows, blocks and
    /// fades on the same ticks. Every machine must then spawn it from the
    /// same builders at the same position, with no parent: a parent's
    /// transform is applied in floating point, so the position (and the seed
    /// made from it) may come out in other bits. A tick still to come pops
    /// it then.
    pub pop_tick: Option<u64>,
    /// Despawn the entity when the cloud is gone. The cloud stays in
    /// [`SmokeWorld`] for [`Config::history_ticks`] more, as every cloud
    /// does, for views of the ticks before; despawning the entity sooner
    /// removes the cloud at once.
    pub despawn_when_done: bool,
    /// Pop exactly this, whatever the fields above say ([`Smoke::from_spec`]).
    spec: Option<CloudSpec>,
    cloud: Option<CloudId>,
}

impl Default for Smoke {
    fn default() -> Self {
        Self {
            source: SmokeSource::Point,
            volume: None,
            lifetime: None,
            grow_time: None,
            start: None,
            fade_time: None,
            tint: Color::WHITE,
            density: 1.0,
            seed: None,
            style: None,
            pop_tick: None,
            despawn_when_done: true,
            spec: None,
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

    /// A wall of smoke from the entity to `offset` from it (world axes), as
    /// from a line of smoke pots. It spreads the default volume along the
    /// line.
    pub fn wall(offset: Vec3) -> Self {
        Self {
            source: SmokeSource::Line { offset },
            lifetime: Some(25.0),
            ..default()
        }
    }

    /// A drifting cloud of greenish poison gas. Use [`SmokeVision::density_of`]
    /// with its [`Smoke::cloud`] to hurt whoever stands in it:
    /// [`SmokeVision::density_at`] adds up every smoke, gas or not.
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
    /// entity, along the world's axes) for as long as the entity lives.
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

    /// The cloud a server announced, to the bit: its origin, pop tick and
    /// every rule are `spec`'s, the cloud [`Smokes::pop`] of it gives. The
    /// entity's position, the clock and this smoke's rule fields (source,
    /// volume, lifetime, grow time, start, fade time, seed, style, pop tick)
    /// aren't used. The
    /// look ([`tint`](Self::tint), [`density`](Self::density)) and
    /// [`despawn_when_done`](Self::despawn_when_done) are the entity's, as
    /// for any smoke. Give it a [`Transform`] at `spec.origin` if anything
    /// else needs its position.
    pub fn from_spec(spec: CloudSpec) -> Self {
        Self {
            spec: Some(spec),
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

    /// Start growing `seconds` after the pop: a fuse (see [`Smoke::start`]).
    pub fn with_start(mut self, seconds: f32) -> Self {
        self.start = Some(seconds);
        self
    }

    /// Spend the last `seconds` of the lifetime dissolving.
    pub fn with_fade_time(mut self, seconds: f32) -> Self {
        self.fade_time = Some(seconds);
        self
    }

    /// Colour the smoke (a look only).
    pub fn with_tint(mut self, tint: Color) -> Self {
        self.tint = tint;
        self
    }

    /// Make volumetric smoke look this dense (a look only: see
    /// [`Smoke::density`]).
    pub fn with_density(mut self, density: f32) -> Self {
        self.density = density;
        self
    }

    /// Despawn the entity once the cloud is gone, or keep it (see
    /// [`Smoke::despawn_when_done`]).
    pub fn with_despawn_when_done(mut self, despawn: bool) -> Self {
        self.despawn_when_done = despawn;
        self
    }

    /// Use a fixed fill seed.
    pub fn with_seed(mut self, seed: u32) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Use this style instead of [`DefaultSmokeStyle`].
    pub fn with_style(mut self, style: SmokeStyle) -> Self {
        self.style = Some(style);
        self
    }

    /// Pop at `tick` instead of the current tick: the tick of the event that
    /// threw it (see [`Smoke::pop_tick`]).
    pub fn with_pop_tick(mut self, tick: u64) -> Self {
        self.pop_tick = Some(tick);
        self
    }

    /// The cloud in [`SmokeWorld`], once popped: for
    /// [`SmokeVision::density_of`], for example.
    pub fn cloud(&self) -> Option<CloudId> {
        self.cloud
    }
}

/// Gameplay queries against every smoke at the current [`SmokeClock::tick`]:
/// bot vision, server-side visibility, audio occlusion, gas damage. The same
/// answers the core gives anywhere else, in world units.
///
/// It only reads, so systems that use it run in parallel with each other
/// and with any system that only reads [`SmokeWorld`]. It answers from
/// views the plugin prepares: when the clock moves (in `PreUpdate`), and
/// again once [`SmokeSystems::Simulate`] has applied the frame's pops and
/// impacts. Every system between two of those sees the same smoke, whatever
/// order they run in. Changes made to [`SmokeWorld`] directly show from the
/// next one, or from [`SmokeWorld::prepare_vision`]; for other ticks (lag
/// compensation), or every change made so far, ask [`SmokeWorld`] itself.
///
/// For many queries, take the [`view`](Self::view) once: it is shared by
/// every thread.
///
/// ```no_run
/// # use bevy::prelude::*;
/// # use bevy_smokescreen::prelude::*;
/// #[derive(Component)]
/// struct Bot {
///     sees_player: bool,
/// }
///
/// fn bot_vision(vision: SmokeVision, mut bots: Query<(&GlobalTransform, &mut Bot)>) {
///     let view = vision.view();
///     let player = Vec3::new(0.0, 1.6, 10.0);
///     bots.par_iter_mut().for_each(|(eye, mut bot)| {
///         bot.sees_player = !view.blocks_sight(eye.translation(), player);
///     });
/// }
/// ```
#[derive(SystemParam)]
pub struct SmokeVision<'w> {
    world: Res<'w, SmokeWorld>,
    clock: Res<'w, SmokeClock>,
    settings: Res<'w, SmokeSettings>,
    warnings: Res<'w, SetupWarnings>,
}

impl SmokeVision<'_> {
    /// The tick the answers are for: [`SmokeClock::tick`] when the views
    /// were prepared.
    pub fn tick(&self) -> u64 {
        self.view().tick()
    }

    /// Every smoke at [`Self::tick`], to ask many questions of from any
    /// number of threads (`par_iter`), or to hand to your own functions.
    pub fn view(&self) -> SmokeView<'_> {
        self.prepared(VISION_PIN, self.clock.tick)
    }

    /// Every smoke at the tick being drawn ([`SmokeClock::drawn`]), for
    /// checks that must agree with the picture: a name tag hidden where the
    /// smoke drawn hides its player, say.
    pub fn drawn_view(&self) -> SmokeView<'_> {
        self.prepared(DRAWN_PIN, self.clock.drawn())
    }

    /// The view `pin` keeps, which should be of `tick`.
    fn prepared(&self, pin: usize, tick: u64) -> SmokeView<'_> {
        let view = self
            .world
            .pinned_view(pin)
            .expect("SmokePlugin prepares SmokeVision's views when it is built");
        if view.tick() != tick {
            self.warnings.warn(Mistake::VisionTick, || {
                format!(
                    "SmokeVision answered for tick {}, but SmokeClock is at {tick}: the clock moved after the plugin prepared SmokeVision's views (in PreUpdate, and at the end of SmokeSystems::Simulate). Write SmokeClock in First, or call SmokeWorld::prepare_vision(&clock) after writing it.",
                    view.tick()
                )
            });
        }
        view
    }

    /// The amount of cloud between two points, every smoke added up, in world
    /// units (see [`Smokes::occlusion`]).
    pub fn occlusion(&self, from: Vec3, to: Vec3) -> f32 {
        self.view().occlusion(from, to)
    }

    /// Does smoke hide `to` from `from`? True once the segment passes through
    /// [`Tuning::sight_threshold`] of cloud.
    pub fn blocks_sight(&self, from: Vec3, to: Vec3) -> bool {
        self.view().blocks_sight(from, to)
    }

    /// Fraction of light (0..1) that gets through, from the amount of cloud
    /// and [`SmokeSettings::extinction`]: for audio and other soft effects.
    pub fn transmittance(&self, from: Vec3, to: Vec3) -> f32 {
        let amount = self.view().occlusion(from, to);
        let metres = self.world.units().length_to_internal(amount);
        (-metres * self.settings.extinction).exp()
    }

    /// Density (0..1 per smoke, summed) at a point: every smoke, gas or not.
    /// [`Self::density_of`] gives one smoke's.
    pub fn density_at(&self, p: Vec3) -> f32 {
        self.view().density_at(p)
    }

    /// One smoke's density (0..1) at a point: its share of
    /// [`Self::density_at`], to the bit, with the id from [`Smoke::cloud`].
    /// For gas damage that plain smoke doesn't do (see
    /// [`Smokes::density_of`]). [`Smoke::density`] is a look and doesn't
    /// change it.
    pub fn density_of(&self, cloud: CloudId, p: Vec3) -> f32 {
        self.view().density_of(cloud, p)
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

/// The directional lights, and whether each is marked [`SmokeSun`].
type DirectionalLights<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static DirectionalLight,
        &'static GlobalTransform,
        Has<SmokeSun>,
    ),
>;

/// The smoke's sun: its entity, the direction towards it, and its colour
/// times illuminance.
fn find_sun(lights: &DirectionalLights) -> Option<(Entity, Vec3, Vec3)> {
    lights
        .iter()
        .max_by_key(|(_, _, _, marked)| *marked)
        .map(|(entity, light, transform, _)| {
            (
                entity,
                transform.back().as_vec3(),
                light.color.to_linear().to_vec3() * light.illuminance,
            )
        })
}

fn advance_clock(mut clock: ResMut<SmokeClock>, time: Res<Time<Virtual>>, config: Res<SmokeConfig>) {
    if clock.auto {
        let (tick, fraction) = tick_at(time.elapsed_secs_f64(), config.tick_rate);
        if clock.tick != tick || clock.draw_fraction != fraction {
            clock.tick = tick;
            clock.draw_fraction = fraction;
        }
    }
}

/// The tick `seconds` falls in at `tick_rate` ticks a second, and how far
/// past it.
fn tick_at(seconds: f64, tick_rate: f32) -> (u64, f32) {
    let ticks = seconds * tick_rate as f64;
    (ticks.floor() as u64, (ticks - ticks.floor()) as f32)
}

/// Seconds on the volumetric noise's clock: the drawn tick and the fraction
/// past it, never the machine's own clock, so every player drawing the same
/// tick sees the same noise. It starts again after an hour's worth of ticks,
/// on the same tick everywhere, so an `f32` keeps its precision (Bevy's
/// clock wraps hourly too).
fn noise_time(drawn: u64, fraction: f32, tick_rate: f32) -> f32 {
    let wrap = (tick_rate as f64 * 3600.0).round().max(1.0) as u64;
    (((drawn % wrap) as f64 + fraction as f64) / tick_rate as f64) as f32
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
    default_style: Res<DefaultSmokeStyle>,
    level: Option<Res<SmokeLevel>>,
    obstacles: Res<obstacles::ObstacleBuild>,
    warnings: Res<SetupWarnings>,
) {
    // The first level built from marked meshes is on its way: wait for it.
    if level.is_none() && obstacles.pending() {
        return;
    }
    for (entity, mut smoke, transform) in &mut smokes {
        if smoke.cloud.is_some() {
            continue;
        }
        if level.is_none() {
            warnings.warn(Mistake::NoLevel, || {
                "A smoke popped with no level: smoke only knows a flat ground at height 0 and passes through walls. Mark the level's meshes, floor included, with SmokeObstacle, or insert a SmokeLevel before smokes pop. For open ground on purpose, insert SmokeLevel::new(FlatGround::new(height, UpAxis::Y))."
                    .into()
            });
        }
        // A server's spec as it is; otherwise the entity's position and the
        // smoke's settings, on its own tick or the current one.
        let spec = smoke.spec.unwrap_or_else(|| {
            let origin = transform.translation();
            let seed = smoke.seed.unwrap_or_else(|| CloudSpec::seed_from_origin(origin));
            let tick = smoke.pop_tick.unwrap_or(clock.tick);
            let mut spec = CloudSpec::new(origin, tick, seed, smoke.style.unwrap_or(default_style.0));
            spec.source = match smoke.source {
                SmokeSource::Point => CloudSource::Point,
                SmokeSource::Line { offset } => CloudSource::Line { end: origin + offset },
                SmokeSource::Box { half_extents } => CloudSource::Box { half_extents },
            };
            spec.volume = smoke.volume;
            if [smoke.start, smoke.grow_time, smoke.lifetime, smoke.fade_time]
                .iter()
                .any(Option::is_some)
            {
                let base = world.config().tuning.timeline;
                spec.timeline = Some(SmokeTimeline {
                    start: smoke.start.unwrap_or(base.start),
                    grow_time: smoke.grow_time.unwrap_or(base.grow_time),
                    lifetime: smoke.lifetime.unwrap_or(base.lifetime),
                    fade_time: smoke.fade_time.unwrap_or(base.fade_time),
                });
            }
            spec
        });
        if !warnings.warned(Mistake::TooBig)
            && let Some(message) = setup::misfit(&spec, &world)
        {
            warnings.warn(Mistake::TooBig, || message);
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
        world.bullet(b.start, b.end, b.tick.unwrap_or(clock.tick));
    }
    for e in explosions.read() {
        world.explosion(e.center, e.radius, e.tick.unwrap_or(clock.tick));
    }
}

fn advance_world(mut world: ResMut<SmokeWorld>, clock: Res<SmokeClock>) {
    world.advance(clock.tick);
}

/// Prepares [`SmokeVision`]'s views of the clock's ticks.
fn prepare_views(mut world: ResMut<SmokeWorld>, clock: Res<SmokeClock>) {
    world.prepare_vision(&clock);
}

/// Clouds that count for less than their smokes promise: past
/// `Tuning::max_clouds`, or empty, popped inside solid geometry. Servers are
/// told too, as they draw nothing.
fn warn_of_clouds(world: Res<SmokeWorld>, warnings: Res<SetupWarnings>) {
    if !warnings.warned(Mistake::TooManyClouds) && world.pinned_view(VISION_PIN).is_some_and(|v| v.uncounted() > 0) {
        let max_clouds = world.config().tuning.max_clouds;
        warnings.warn(Mistake::TooManyClouds, || {
            format!(
                "More smokes are alive than Tuning::max_clouds ({max_clouds}): the oldest stop counting, so they neither block sight nor are drawn until fewer are alive (a long-lived fog bank goes first). Raise max_clouds in SmokePlugin's Config if you need more at once: up to 32 volumetric or faceted smokes are drawn, crystal ones without a limit."
            )
        });
    }
    // The fill found nothing open to spread from.
    if !warnings.warned(Mistake::InsideSolid)
        && let Some(cloud) = world.clouds().find(|c| c.shape().is_some_and(|s| s.grid.is_empty()))
    {
        warnings.warn(Mistake::InsideSolid, || {
            format!(
                "A smoke popped inside solid level geometry at {}: nothing next to it is open, so its cloud is empty, isn't drawn and blocks nothing. Pop smokes in open air, on or just above the floor. If every smoke does this, check that the level's meshes face outwards (see SmokeObstacle).",
                shown(cloud.spec().origin)
            )
        });
    }
}

/// Added only without Bevy's `TransformPlugin`: nothing then updates
/// `GlobalTransform`, so smokes placed by their entity would pop at the
/// origin, and obstacle meshes be voxelised there.
fn warn_without_transforms(
    smokes: Query<&Smoke, Added<Smoke>>,
    obstacles: Query<(), Added<SmokeObstacle>>,
    warnings: Res<SetupWarnings>,
) {
    if !warnings.warned(Mistake::NoTransforms) && (smokes.iter().any(|s| s.spec.is_none()) || !obstacles.is_empty()) {
        warnings.warn(Mistake::NoTransforms, || {
            "Bevy's TransformPlugin isn't added, so nothing updates GlobalTransform: smokes pop at the world's origin whatever their Transform, and SmokeObstacle meshes are voxelised there too. Add TransformPlugin (part of DefaultPlugins); on a server, .add_plugins((MinimalPlugins, TransformPlugin, SmokePlugin::default())). Smokes made with Smoke::from_spec take their place from the spec and need no transforms."
                .into()
        });
    }
}

/// Despawn entities whose cloud is gone, and remove the clouds of entities
/// despawned sooner.
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
            // The cloud stays: the core forgets it after
            // `Config::history_ticks`, so views of the ticks before (lag
            // compensation, a drawn tick behind) still have it.
            entities.0.remove(&entity);
            commands.entity(entity).despawn();
        }
    }
}

/// Start (and collect) the background work out how much sun and sky each
/// cloud's voxels see.
fn light_clouds(
    world: Res<SmokeWorld>,
    mut lighting: ResMut<SmokeLighting>,
    lights: DirectionalLights,
    uploads: Res<SmokeUploads>,
) {
    // Only an upload uses it.
    if !uploads.drained {
        return;
    }
    let units = *world.units();
    let sun = find_sun(&lights).map(|(_, dir, _)| dir);
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
    dir_lights: DirectionalLights,
    ambient: Option<Res<GlobalAmbientLight>>,
    mut last: Local<Option<(Vec3, u64)>>,
    warnings: Res<SetupWarnings>,
) {
    let frame = frame.as_mut();
    frame.slots.clear();
    frame.holes.clear();
    frame.bake.clear();
    frame.facet_puffs.clear();

    let units = config.units;
    let sun = find_sun(&dir_lights);
    frame.has_sun = sun.is_some();
    if let Some((_, dir, color)) = sun {
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
    frame.time = noise_time(clock.drawn(), clock.draw_fraction, config.tick_rate);
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

    let max_clouds = world.config().tuning.max_clouds;
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
                    uploads.push(SlotUpload {
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
                    // Only past 32: up to that, `max_clouds` keeps every
                    // cloud that counts within the slots.
                    warnings.warn(Mistake::SlotsFull, || {
                        format!(
                            "More than {} volumetric or faceted smokes count at once (Tuning::max_clouds is {max_clouds}): one that pops while all {} slots are taken isn't drawn until a slot frees, though it blocks sight. Keep max_clouds at {} or less, or use the crystal style for the rest.",
                            config.max_smokes, config.max_smokes, config.max_smokes
                        )
                    });
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
            uploads.push(SlotUpload {
                layer,
                shape: cloud.shape.clone(),
                lighting: Some(light.clone()),
            });
            owner.lit = true;
            owner.force_bake = true;
        }

        let state = cloud.state;
        // Every drawn smoke's holes share one array, oldest smoke first.
        let hole_start = frame.holes.len() as u32;
        let room = render::MAX_HOLES - frame.holes.len();
        if state.holes.len() > room {
            warnings.warn(Mistake::TooManyHoles, || {
                format!(
                    "More than {} holes are open in the smokes drawn: the newest smokes' holes past that aren't drawn, though the rule counts them. Keep Tuning::max_holes_per_cloud × the smokes drawn at once (Tuning::max_clouds, at most 32) within {}.",
                    render::MAX_HOLES,
                    render::MAX_HOLES
                )
            });
        }
        frame.holes.extend(state.holes.iter().take(room));
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

    // Only volumetric and faceted smoke is lit by the sun picked here.
    if !frame.slots.is_empty()
        && !warnings.warned(Mistake::Sun)
        && let Some((sun, ..)) = sun
    {
        let lights = dir_lights.iter().len();
        let marked = dir_lights.iter().filter(|(.., marked)| *marked).count();
        if marked > 1 {
            warnings.warn(Mistake::Sun, || {
                format!(
                    "{marked} directional lights have SmokeSun; the smoke is lit by one of them ({sun}). Keep SmokeSun on one."
                )
            });
        } else if marked == 0 && lights > 1 {
            warnings.warn(Mistake::Sun, || {
                format!(
                    "{lights} directional lights and none has SmokeSun, so volumetric and faceted smoke is lit by one of them ({sun}), and which one can change. Add SmokeSun to the light that should light the smoke."
                )
            });
        }
    }
}

/// Point lights near any smoke, brightest first, in the internal frame.
///
/// Runs after Bevy's `CheckVisibility`: Bevy clears every `ViewVisibility`
/// earlier in `PostUpdate` and sets a point light's again where its range
/// reaches into a camera's view. So this keeps every light that can light
/// smoke on screen, flashes spawned this frame included, and no light the
/// game has hidden.
fn gather_point_lights(
    mut frame: ResMut<SmokeFrame>,
    config: Res<SmokeConfig>,
    point_lights: Query<(&PointLight, &GlobalTransform, &ViewVisibility)>,
) {
    let frame = frame.as_mut();
    frame.lights.clear();
    if frame.slots.is_empty() {
        return;
    }
    let units = config.units;
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
                color: light.color.to_linear().to_vec3() * light.intensity / (4.0 * core::f32::consts::PI) * (m * m),
            })
        })
        .collect();
    candidates.sort_by(|a, b| b.color.max_element().total_cmp(&a.color.max_element()));
    candidates.truncate(render::MAX_LIGHTS);
    frame.lights = candidates;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn noise_time_follows_the_drawn_tick() {
        assert_eq!(noise_time(640, 0.0, 128.0), 5.0);
        assert_eq!(noise_time(640, 0.5, 128.0), 5.003_906_3);
        // It starts again after an hour's worth of ticks, a whole number of
        // them whatever the tick rate, so every machine jumps on the same one.
        let hour = 460_800;
        assert_eq!(noise_time(hour, 0.0, 128.0), 0.0);
        assert_eq!(noise_time(hour + 640, 0.5, 128.0), noise_time(640, 0.5, 128.0));
        assert!(noise_time(hour - 1, 0.99, 128.0) > 3599.99);
        let rate = 100.0 / 3.0;
        assert_eq!(noise_time(120_000 + 7, 0.25, rate), noise_time(7, 0.25, rate));
    }

    #[test]
    fn auto_noise_time_is_virtual_time() {
        // Counted from virtual time, the noise's clock is the one it read
        // from Bevy before, to within one step of an f32 (Bevy's reading
        // rounds the nanoseconds to an f32 on their own first). On past the
        // hour, where both start again.
        for step in [
            Duration::from_secs_f32(1.0 / 20.0),
            Duration::from_secs_f32(1.0 / 30.0),
            Duration::from_nanos(16_666_667),
        ] {
            let mut time = Time::<Virtual>::default();
            while time.elapsed_secs_f64() < 4000.0 {
                time.advance_by(step);
                let (tick, fraction) = tick_at(time.elapsed_secs_f64(), 128.0);
                let ours = noise_time(tick, fraction, 128.0);
                let bevy = time.elapsed_secs_wrapped();
                let ulps = (ours.to_bits() as i64 - bevy.to_bits() as i64).abs();
                assert!(ulps <= 1, "{step:?}: {ours} against {bevy}");
            }
        }
    }

    /// A headless app with Bevy's own visibility systems: no window, no GPU.
    fn headless_app() -> App {
        headless_app_with(SmokePlugin::default())
    }

    /// The same, with this plugin.
    fn headless_app_with(plugin: SmokePlugin) -> App {
        use bevy::{
            log::LogPlugin,
            render::{RenderPlugin, settings::WgpuSettings},
            time::TimeUpdateStrategy,
            window::ExitCondition,
            winit::WinitPlugin,
        };
        let mut app = App::new();
        app.add_plugins((
            DefaultPlugins
                .build()
                .disable::<WinitPlugin>()
                .disable::<LogPlugin>()
                .set(RenderPlugin {
                    render_creation: WgpuSettings {
                        backends: None,
                        ..default()
                    }
                    .into(),
                    ..default()
                })
                .set(WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    ..default()
                }),
            plugin,
        ))
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(50)));
        app.finish();
        app.cleanup();
        app
    }

    #[test]
    fn point_lights_that_reach_the_view_light_the_smoke() {
        let mut app = headless_app();
        let camera = app
            .world_mut()
            .spawn((
                Camera3d::default(),
                Transform::from_xyz(0.0, 1.5, 9.0).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
            ))
            .id();
        app.world_mut().spawn((Smoke::default(), Transform::default()));
        for _ in 0..100 {
            app.update();
            if !app.world().resource::<SmokeFrame>().slots.is_empty() {
                break;
            }
        }
        assert!(
            !app.world().resource::<SmokeFrame>().slots.is_empty(),
            "the smoke never popped"
        );

        // Spawned this frame, so Bevy sets its `ViewVisibility` in this
        // frame's `PostUpdate`; the lights are read after that.
        let flash = PointLight {
            intensity: 60_000.0,
            range: 6.0,
            ..default()
        };
        let shown = Vec3::new(0.0, 1.0, 0.0);
        app.world_mut().spawn((flash, Transform::from_translation(shown)));
        app.world_mut()
            .spawn((flash, Transform::from_xyz(0.5, 1.0, 0.0), Visibility::Hidden));
        app.update();
        let lights = &app.world().resource::<SmokeFrame>().lights;
        assert_eq!(lights.len(), 1, "only the light that isn't hidden");
        assert_eq!(lights[0].position, shown);

        // Looking away, the light can't reach anything on screen.
        *app.world_mut().get_mut::<Transform>(camera).unwrap() =
            Transform::from_xyz(0.0, 1.5, 9.0).looking_to(Vec3::Z, Vec3::Y);
        app.update();
        assert!(app.world().resource::<SmokeFrame>().lights.is_empty());
    }

    #[test]
    fn every_hole_the_rule_counts_is_drawn() {
        let mut app = headless_app();
        // Six smokes in a row, popped a frame apart: the last is the newest.
        for k in 0..6 {
            app.world_mut().spawn((
                Smoke::default().with_seed(k + 1),
                Transform::from_xyz(10.0 * k as f32, 0.0, 0.0),
            ));
            app.update();
        }
        for _ in 0..200 {
            if app.world().resource::<SmokeFrame>().slots.len() == 6 {
                break;
            }
            app.update();
        }
        assert_eq!(
            app.world().resource::<SmokeFrame>().slots.len(),
            6,
            "the smokes never popped"
        );

        // 16 shots along the row, each through all six: 96 holes, where the
        // renderer once had room for 64, so the fifth and sixth smokes'
        // tunnels went undrawn.
        let tick = app.world().resource::<SmokeClock>().tick;
        let mut world = app.world_mut().resource_mut::<SmokeWorld>();
        for i in 0..16 {
            let (y, z) = (0.4 + 0.3 * (i / 4) as f32, -1.2 + 0.8 * (i % 4) as f32);
            world.bullet(Vec3::new(-10.0, y, z), Vec3::new(60.0, y, z), tick);
        }
        app.update();

        let frame = app.world().resource::<SmokeFrame>().clone();
        let drawn = app.world().resource::<SmokeClock>().drawn();
        let mut world = app.world_mut().resource_mut::<SmokeWorld>();
        let view = world.view(drawn);
        let clouds: Vec<_> = view.clouds().collect();
        assert_eq!(clouds.len(), frame.slots.len());
        for (k, (cloud, slot)) in clouds.iter().zip(&frame.slots).enumerate() {
            assert_eq!(cloud.state.holes.len(), 16, "smoke {k}'s holes");
            let holes = &frame.holes[slot.hole_start as usize..][..slot.hole_count as usize];
            assert_eq!(holes, cloud.state.holes, "smoke {k}'s holes drawn");
        }
    }

    /// `SmokeVision::density_of` tells gas from plain smoke: inside the gas
    /// it is all of `density_at`, and inside a grenade's smoke it is 0,
    /// where `density_at` counts that smoke too.
    #[test]
    fn density_of_tells_gas_from_smoke() {
        use bevy::ecs::system::SystemState;

        let mut app = headless_app();
        let gas = app.world_mut().spawn((Smoke::gas(), Transform::default())).id();
        app.world_mut()
            .spawn((Smoke::grenade(), Transform::from_xyz(30.0, 0.0, 0.0)));
        // Five seconds at 50 ms a frame: both have grown.
        for _ in 0..100 {
            app.update();
        }
        let cloud = app
            .world()
            .get::<Smoke>(gas)
            .and_then(Smoke::cloud)
            .expect("the gas never popped");
        let mut state = SystemState::<SmokeVision>::new(app.world_mut());
        let vision = state.get(app.world()).expect("the plugin's resources");
        let (in_gas, in_smoke) = (Vec3::new(0.0, 1.0, 0.0), Vec3::new(30.0, 1.0, 0.0));
        let gassed = vision.density_of(cloud, in_gas);
        assert!(gassed > 0.0, "no gas at {in_gas}");
        assert_eq!(gassed.to_bits(), vision.density_at(in_gas).to_bits());
        assert!(vision.density_at(in_smoke) > 0.0, "no smoke at {in_smoke}");
        assert_eq!(vision.density_of(cloud, in_smoke), 0.0);
    }

    /// A pop heard late keeps pace with the server's: a `Smoke` on the
    /// server's tick, or made from its whole spec, is the cloud
    /// `SmokeWorld::pop` of that spec gives, to the bit, at every tick. Once
    /// done, it stays for views of the ticks before, as the core keeps it.
    #[test]
    fn smokes_popped_on_the_servers_tick_match_the_servers_clouds() {
        /// Every answer compared, as bits: lines through one smoke, both and
        /// neither, and points in each.
        fn answers(smokes: &mut Smokes<SharedLevel>, clouds: [CloudId; 2], tick: u64) -> Vec<u32> {
            let lines = [
                (Vec3::new(-6.0, 1.0, -1.5), Vec3::new(18.0, 1.0, 2.5)),
                (Vec3::new(0.3, 0.5, -8.0), Vec3::new(0.3, 0.5, 8.0)),
                (Vec3::new(11.6, 0.6, -6.0), Vec3::new(11.6, 0.6, 9.0)),
                (Vec3::new(-4.0, 2.5, 6.0), Vec3::new(16.0, 0.3, -3.0)),
                (Vec3::new(5.0, 3.0, 0.0), Vec3::new(5.2, 0.1, 0.3)),
            ];
            let points = [
                Vec3::new(0.3, 1.0, -0.7),
                Vec3::new(11.6, 0.8, 1.6),
                Vec3::new(2.0, 0.4, 0.5),
                Vec3::new(13.0, 1.8, 2.2),
                Vec3::new(-1.5, 0.3, -2.0),
                Vec3::new(6.0, 0.5, 0.8),
            ];
            let mut out = Vec::new();
            for (a, b) in lines {
                out.push(smokes.occlusion(a, b, tick).to_bits());
                out.push(smokes.blocks_sight(a, b, tick) as u32);
            }
            for p in points {
                out.push(smokes.density_at(p, tick).to_bits());
                out.extend(clouds.map(|id| smokes.density_of(id, p, tick).to_bits()));
            }
            out
        }

        // The server: the core alone, with the plugin's rules and level.
        let mut server = Smokes::new(
            SmokePlugin::default().config,
            Arc::new(FlatGround::new(0.0, UpAxis::Y)) as SharedLevel,
        );
        let popped = 200;
        // What `Smoke::grenade()` pops at `at`.
        let at = Vec3::new(0.3, 0.0, -0.7);
        let grenade = CloudSpec::new(at, popped, CloudSpec::seed_from_origin(at), SmokeStyle::Volumetric);
        // What a `Smoke`'s fields can't say: a line whose end doesn't survive
        // the trip through an offset, and a timeline with its own start and
        // fade.
        let (from, end) = (Vec3::new(9.0, 0.2, 0.6), Vec3::new(14.3, 0.2, 2.7));
        assert_ne!(from + (end - from), end);
        let wall = CloudSpec {
            source: CloudSource::Line { end },
            volume: Some(60.0),
            timeline: Some(SmokeTimeline {
                start: 0.3,
                grow_time: 2.0,
                lifetime: 9.0,
                fade_time: 2.5,
            }),
            ..CloudSpec::new(from, popped, 77, SmokeStyle::Volumetric)
        };
        let theirs = [server.pop(grenade), server.pop(wall)];

        // The client hears of both 20 ticks late.
        let mut app = headless_app();
        let heard = popped + 20;
        app.insert_resource(SmokeClock {
            tick: heard,
            auto: false,
            ..default()
        });
        let tint = Color::srgb(1.0, 0.3, 0.1);
        let entities = [
            app.world_mut()
                .spawn((Smoke::grenade().with_pop_tick(popped), Transform::from_translation(at)))
                .id(),
            app.world_mut().spawn(Smoke::from_spec(wall).with_tint(tint)).id(),
        ];
        app.update();
        let ours = entities.map(|e| {
            app.world()
                .get::<Smoke>(e)
                .and_then(Smoke::cloud)
                .expect("never popped")
        });
        let world = app.world().resource::<SmokeWorld>();
        for (id, spec) in ours.into_iter().zip([grenade, wall]) {
            assert_eq!(world.cloud(id).map(|c| *c.spec()), Some(spec));
        }

        // Just heard; the wall growing; both grown; the wall fading; the wall
        // done, and a frame later; the grenade fading; the grenade done, and
        // a frame later; both forgotten. At each, the answers now and 100
        // ticks before (lag compensation, within `Config::history_ticks`).
        let ends = theirs.map(|id| server.cloud(id).map(|c| c.end_tick()).expect("the server's cloud"));
        let mut advanced = popped;
        for tick in [20, 150, 400, 1000, 1200, 1210, 2100, 2310, 2320, 2450].map(|k| popped + k) {
            app.world_mut().resource_mut::<SmokeClock>().tick = tick;
            app.update();
            for t in advanced..=tick {
                server.advance(t);
            }
            advanced = tick + 1;
            for t in [tick, tick - 100] {
                let mut world = app.world_mut().resource_mut::<SmokeWorld>();
                assert_eq!(
                    answers(&mut world, ours, t),
                    answers(&mut server, theirs, t),
                    "tick {t}"
                );
            }
            // Done smokes despawn, and leave their clouds to the core.
            for k in 0..2 {
                let alive = app.world().get_entity(entities[k]).is_ok();
                assert_eq!(alive, tick < ends[k], "smoke {k} at tick {tick}");
            }
            if tick == popped + 400 {
                // Both grown, and drawn with their entities' looks.
                assert!(server.blocks_sight(Vec3::new(-6.0, 1.0, -1.5), Vec3::new(18.0, 1.0, 2.5), tick));
                assert!(server.density_of(theirs[1], Vec3::new(11.6, 0.8, 1.6), tick) > 0.0);
                let drawn: Vec<CloudId> = app
                    .world_mut()
                    .resource_mut::<SmokeWorld>()
                    .view(tick)
                    .clouds()
                    .map(|c| c.state.id)
                    .collect();
                let slots = &app.world().resource::<SmokeFrame>().slots;
                let looks: Vec<(CloudId, Vec3)> = drawn.into_iter().zip(slots.iter().map(|s| s.tint)).collect();
                assert_eq!(looks.len(), 2);
                assert!(looks.contains(&(ours[0], Vec3::ONE)));
                assert!(looks.contains(&(ours[1], tint.to_linear().to_vec3())));
            }
        }
        assert_eq!(server.clouds().count(), 0);
        assert_eq!(app.world().resource::<SmokeWorld>().clouds().count(), 0);
    }

    /// A shot or blast heard late lands on its own tick: the messages carve
    /// the holes `SmokeWorld::bullet` and `explosion` carve at those ticks,
    /// to the bit, and one without a tick lands on the current tick.
    #[test]
    fn impacts_land_on_their_own_ticks() {
        let mut app = headless_app();
        app.insert_resource(SmokeClock {
            tick: 0,
            auto: false,
            ..default()
        });
        app.world_mut().spawn((Smoke::grenade(), Transform::default()));
        app.update();
        // The server: the same cloud, in the core alone.
        let mut server = Smokes::new(
            SmokePlugin::default().config,
            Arc::new(FlatGround::new(0.0, UpAxis::Y)) as SharedLevel,
        );
        let seed = CloudSpec::seed_from_origin(Vec3::ZERO);
        server.pop(CloudSpec::new(Vec3::ZERO, 0, seed, SmokeStyle::Volumetric));

        // Grown by tick 200; a shot at 300 and a blast at 310, both heard at
        // 320 along with a shot of that tick's.
        let (shot, blast, now) = (300, 310, 320);
        let line = (Vec3::new(-8.0, 1.0, 0.2), Vec3::new(8.0, 1.2, -0.3));
        let late = (Vec3::new(-8.0, 0.6, 1.0), Vec3::new(8.0, 0.6, 1.0));
        let center = Vec3::new(0.5, 1.0, 2.0);
        app.world_mut().resource_mut::<SmokeClock>().tick = now;
        app.world_mut().write_message(SmokeBullet {
            start: line.0,
            end: line.1,
            tick: Some(shot),
        });
        app.world_mut().write_message(SmokeExplosion {
            center,
            tick: Some(blast),
            ..default()
        });
        app.world_mut().write_message(SmokeBullet {
            start: late.0,
            end: late.1,
            ..default()
        });
        app.update();
        server.bullet(line.0, line.1, shot);
        server.explosion(center, None, blast);
        server.bullet(late.0, late.1, now);

        let across = (center - Vec3::X * 6.0, center + Vec3::X * 6.0);
        let mut world = app.world_mut().resource_mut::<SmokeWorld>();
        for t in [shot - 1, shot, shot + 5, blast, blast + 5, now, now + 50] {
            for (a, b) in [line, late, across] {
                assert_eq!(
                    world.occlusion(a, b, t).to_bits(),
                    server.occlusion(a, b, t).to_bits(),
                    "tick {t}"
                );
            }
        }
        // The shot's tunnel was open before the shot was heard.
        assert!(world.occlusion(line.0, line.1, shot + 5) < world.occlusion(line.0, line.1, shot - 1));
    }

    /// Smokes without a style of their own pop in `DefaultSmokeStyle`'s, as
    /// it is at their pop.
    #[test]
    fn smokes_without_a_style_pop_in_the_default_style() {
        let mut app = headless_app();
        app.insert_resource(DefaultSmokeStyle(SmokeStyle::Crystal));
        let plain = app.world_mut().spawn(Smoke::grenade()).id();
        let own = app
            .world_mut()
            .spawn(Smoke::grenade().with_style(SmokeStyle::Volumetric))
            .id();
        app.update();
        app.world_mut().resource_mut::<DefaultSmokeStyle>().0 = SmokeStyle::Faceted;
        let later = app.world_mut().spawn(Smoke::grenade()).id();
        app.update();
        let style = |app: &App, e: Entity| {
            let cloud = app
                .world()
                .get::<Smoke>(e)
                .and_then(Smoke::cloud)
                .expect("never popped");
            app.world()
                .resource::<SmokeWorld>()
                .cloud(cloud)
                .map(|c| c.spec().style)
        };
        assert_eq!(style(&app, plain), Some(SmokeStyle::Crystal));
        assert_eq!(style(&app, own), Some(SmokeStyle::Volumetric));
        assert_eq!(style(&app, later), Some(SmokeStyle::Faceted));
    }

    #[test]
    fn despawning_a_smoke_sooner_removes_its_cloud_at_once() {
        let mut app = headless_app();
        let smoke = app.world_mut().spawn((Smoke::grenade(), Transform::default())).id();
        app.update();
        let cloud = app
            .world()
            .get::<Smoke>(smoke)
            .and_then(Smoke::cloud)
            .expect("never popped");
        app.world_mut().despawn(smoke);
        app.update();
        assert!(app.world().resource::<SmokeWorld>().cloud(cloud).is_none());
    }

    #[test]
    fn every_crystal_puff_the_rule_counts_is_drawn() {
        use std::any::TypeId;

        use bevy::{
            asset::AssetEvent,
            camera::{
                primitives::{Aabb, Frustum, Sphere},
                visibility::VisibleEntities,
            },
            ecs::message::Messages,
            platform::collections::HashSet,
        };

        /// Puff-frames in which the picture and the rule part ways.
        #[derive(Debug, Default, PartialEq)]
        struct Faults {
            /// Counted by the rule but not drawn by the camera that sees
            /// every puff: hidden, culled, or its cloud's material not yet
            /// sent to the renderer.
            undrawn: u32,
            /// Counted, but not drawn where the rule has it.
            misplaced: u32,
            /// Drawn by a camera but not counted.
            uncounted: u32,
            /// Drawn or culled by the edge camera other than Bevy's culling
            /// of where the puff is now would have it.
            culled: u32,
        }

        let mut app = headless_app();
        // One camera sees both clouds whole, the other only the first one's
        // right side.
        let whole = app
            .world_mut()
            .spawn((
                Camera3d::default(),
                Transform::from_xyz(0.0, 3.0, 18.0).looking_at(Vec3::new(0.0, 1.5, 0.0), Vec3::Y),
            ))
            .id();
        let edge = app
            .world_mut()
            .spawn((
                Camera3d::default(),
                Camera { order: 1, ..default() },
                Transform::from_xyz(6.0, 1.5, 8.0).looking_to(Vec3::NEG_Z, Vec3::Y),
            ))
            .id();
        app.world_mut().spawn((
            Smoke {
                lifetime: Some(10.0),
                ..Smoke::grenade().with_style(SmokeStyle::Crystal).with_seed(3)
            },
            Transform::default(),
        ));

        let units = app.world().resource::<SmokeConfig>().units;
        let rotation = crystal::frame_rotation(&units);
        let mut materials = app
            .world()
            .resource::<Messages<AssetEvent<StandardMaterial>>>()
            .get_cursor();
        let mut announced = HashSet::new();
        let mut faults = Faults::default();
        let (mut counted, mut switched, mut edge_drawn, mut edge_culled) = (0, 0, 0, 0);
        let mut counted_before = HashSet::new();
        let mut late = None;
        // Puffs the rule counts in each cloud's first frame.
        let mut first_frame = HashMap::new();
        // 12 s: the 10 s cloud's whole life.
        for frame in 0..240 {
            let tick = app.world().resource::<SmokeClock>().tick;
            let mut world = app.world_mut().resource_mut::<SmokeWorld>();
            match frame {
                // A volley through the grown cloud: puffs shrink out of the
                // way, to nothing on the bullets' paths, and grow back as the
                // tunnels close.
                40 => {
                    for i in 0..9 {
                        let (y, z) = (0.6 + 0.6 * (i / 3) as f32, -1.0 + (i % 3) as f32);
                        world.bullet(Vec3::new(-10.0, y, z), Vec3::new(10.0, y, z), tick);
                    }
                }
                // A blast on its left: the puffs there go, then refill.
                48 => world.explosion(Vec3::new(-1.5, 1.0, 0.0), Some(1.5), tick),
                // A cloud heard of late, as by a client: popped half a second
                // ago, so its first frame already shows grown puffs.
                60 => {
                    let mut spec = CloudSpec::new(Vec3::new(-5.0, 0.0, -4.0), tick - 64, 4, SmokeStyle::Crystal);
                    spec.timeline = Some(SmokeTimeline {
                        lifetime: 6.0,
                        ..world.config().tuning.timeline
                    });
                    late = Some(world.pop(spec));
                }
                _ => {}
            }
            app.update();

            let drawn = app.world().resource::<SmokeClock>().drawn();
            let mut world = app.world_mut().resource_mut::<SmokeWorld>();
            let view = world.view(drawn);
            let clouds: Vec<_> = view
                .clouds()
                .filter(|c| c.spec.style == SmokeStyle::Crystal)
                .map(|c| (c.state.id, c.state.live.clone()))
                .collect();

            let world = app.world();
            let events = world.resource::<Messages<AssetEvent<StandardMaterial>>>();
            announced.extend(materials.read(events).filter_map(|e| match e {
                AssetEvent::Added { id } => Some(*id),
                _ => None,
            }));
            let drawn_by = |camera: Entity| -> HashSet<Entity> {
                let visible = world.get::<VisibleEntities>(camera).unwrap();
                visible.get(TypeId::of::<Mesh3d>()).iter().copied().collect()
            };
            let (in_whole, in_edge) = (drawn_by(whole), drawn_by(edge));
            let frustum = world.get::<Frustum>(edge).unwrap();
            let entities = world.resource::<crystal::CrystalClouds>();
            let mut counted_now = HashSet::new();
            for (id, live) in &clouds {
                first_frame
                    .entry(*id)
                    .or_insert_with(|| live.iter().filter(|p| p.radius > 0.0).count());
                let puffs = entities.puffs(*id);
                for (i, p) in live.iter().enumerate().filter(|(_, p)| p.radius > 0.0) {
                    counted += 1;
                    let Some(&e) = puffs.get(i) else {
                        faults.undrawn += 1;
                        continue;
                    };
                    counted_now.insert(e);
                    let shown = world.get::<InheritedVisibility>(e).is_some_and(|v| v.get())
                        && world.get::<ViewVisibility>(e).is_some_and(|v| v.get())
                        && in_whole.contains(&e)
                        && world
                            .get::<MeshMaterial3d<StandardMaterial>>(e)
                            .is_some_and(|m| announced.contains(&m.0.id()));
                    if !shown {
                        faults.undrawn += 1;
                    }
                    let place = GlobalTransform::from(crystal::puff_transform(&units, rotation, p));
                    if world.get::<GlobalTransform>(e) != Some(&place) {
                        faults.misplaced += 1;
                    }
                    // Bevy's own culling test (`check_visibility_cpu_culling`)
                    // of the puff where it is now.
                    let seen = world.get::<Aabb>(e).is_none_or(|aabb| {
                        let world_from_local = place.affine();
                        let sphere = Sphere {
                            center: world_from_local.transform_point3a(aabb.center),
                            radius: place.radius_vec3a(aabb.half_extents),
                        };
                        frustum.intersects_sphere(&sphere, false)
                            && frustum.intersects_obb(aabb, &world_from_local, true, false)
                    });
                    if seen != in_edge.contains(&e) {
                        faults.culled += 1;
                    }
                    if seen {
                        edge_drawn += 1;
                    } else {
                        edge_culled += 1;
                    }
                }
            }
            faults.uncounted += in_whole
                .iter()
                .chain(&in_edge)
                .filter(|e| world.get::<CrystalPuff>(**e).is_some() && !counted_now.contains(*e))
                .count() as u32;
            switched += counted_now.symmetric_difference(&counted_before).count();
            counted_before = counted_now;
        }

        assert!(counted > 10_000, "the cloud never grew: {counted} puff-frames");
        assert!(switched > 200, "few puffs switched on or off: {switched}");
        let late = late.unwrap();
        assert!(first_frame[&late] > 0, "the late cloud's first frame showed nothing");
        assert!(
            edge_drawn > 0 && edge_culled > 0,
            "the edge camera should see part of the cloud: {edge_drawn} drawn, {edge_culled} culled"
        );
        assert_eq!(faults, Faults::default(), "over {counted} puff-frames the rule counted");
        let mut puffs = app.world_mut().query::<&CrystalPuff>();
        assert_eq!(puffs.iter(app.world()).count(), 0, "puffs left once the cloud is gone");
    }

    fn warned(app: &App, mistake: Mistake) -> bool {
        app.world().resource::<SetupWarnings>().warned(mistake)
    }

    /// Run until volumetric or faceted smoke is ready to draw (a level from
    /// meshes may still be on its way), then a few frames more.
    fn run_until_drawn(app: &mut App) {
        for _ in 0..1000 {
            app.update();
            if !app.world().resource::<SmokeFrame>().slots.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !app.world().resource::<SmokeFrame>().slots.is_empty(),
            "the smoke never popped"
        );
        for _ in 0..5 {
            app.update();
        }
    }

    /// Run until `mistake` is warned about, or a while.
    fn run_until_warned(app: &mut App, mistake: Mistake) -> bool {
        for _ in 0..500 {
            app.update();
            if warned(app, mistake) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    #[test]
    fn the_quick_start_warns_of_nothing() {
        let mut app = headless_app();
        let world = app.world_mut();
        world.spawn((
            Camera3d::default(),
            SmokeCamera::HIGH,
            Transform::from_xyz(0.0, 1.7, -10.0).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
        ));
        world.spawn((
            DirectionalLight::default(),
            SmokeSun,
            Transform::from_xyz(-4.0, 10.0, -6.0).looking_at(Vec3::ZERO, Vec3::Y),
        ));
        let floor = world
            .resource_mut::<Assets<Mesh>>()
            .add(Plane3d::default().mesh().size(50.0, 50.0));
        world.spawn((Mesh3d(floor), SmokeObstacle));
        world.spawn((Smoke::grenade(), Transform::from_xyz(0.0, 0.0, 0.0)));
        run_until_drawn(&mut app);
        assert!(app.world().contains_resource::<SmokeLevel>());
        assert!(app.world().resource::<SetupWarnings>().none());
    }

    #[test]
    fn a_missing_smoke_camera_is_warned() {
        let mut app = headless_app();
        app.world_mut().spawn(Camera3d::default());
        app.world_mut().spawn((Smoke::grenade(), Transform::default()));
        run_until_drawn(&mut app);
        assert!(warned(&app, Mistake::NoSmokeCamera));
    }

    #[test]
    fn an_orthographic_smoke_camera_is_warned() {
        let mut app = headless_app();
        app.world_mut().spawn((
            Camera3d::default(),
            SmokeCamera::default(),
            Projection::Orthographic(OrthographicProjection::default_3d()),
        ));
        app.world_mut().spawn((Smoke::grenade(), Transform::default()));
        run_until_drawn(&mut app);
        assert!(warned(&app, Mistake::Orthographic));
        assert!(!warned(&app, Mistake::NoSmokeCamera));
    }

    /// Whether a smoke camera with `light` is warned about its ambient light.
    fn ambient_warned(light: impl Bundle, ambient_override: Option<LinearRgba>) -> bool {
        let mut app = headless_app();
        app.world_mut().resource_mut::<SmokeSettings>().ambient_override = ambient_override;
        app.world_mut()
            .spawn((Camera3d::default(), SmokeCamera::default(), light));
        app.world_mut().spawn((Smoke::grenade(), Transform::default()));
        run_until_drawn(&mut app);
        warned(&app, Mistake::CameraAmbient)
    }

    #[test]
    fn a_smoke_camera_with_its_own_ambient_light_is_warned() {
        use bevy::light::{AmbientLight, GeneratedEnvironmentMapLight};
        assert!(ambient_warned(AmbientLight::default(), None));
        assert!(ambient_warned(GeneratedEnvironmentMapLight::default(), None));
        assert!(!ambient_warned(AmbientLight::default(), Some(LinearRgba::gray(0.2))));
        assert!(!ambient_warned((), None));
    }

    /// Whether directional lights, marked or not, are warned about.
    fn sun_warned(marked: &[bool]) -> bool {
        let mut app = headless_app();
        for &marked in marked {
            let mut light = app.world_mut().spawn(DirectionalLight::default());
            if marked {
                light.insert(SmokeSun);
            }
        }
        app.world_mut().spawn((Smoke::grenade(), Transform::default()));
        run_until_drawn(&mut app);
        warned(&app, Mistake::Sun)
    }

    #[test]
    fn an_unclear_sun_is_warned() {
        assert!(sun_warned(&[false, false]));
        assert!(sun_warned(&[true, true]));
        assert!(!sun_warned(&[false, true]));
        assert!(!sun_warned(&[false]));
        assert!(!sun_warned(&[]));
    }

    #[test]
    fn a_smoke_with_no_level_is_warned() {
        let mut app = headless_app();
        app.world_mut().spawn((Smoke::grenade(), Transform::default()));
        app.update();
        assert!(warned(&app, Mistake::NoLevel));
    }

    #[test]
    fn a_source_bigger_than_the_fill_grid_is_warned() {
        // The fill grid is 14.4 × 14.4 across and 7.2 high by default, with
        // 1.8 below the source.
        for (smoke, too_big) in [
            (Smoke::grenade(), false),
            (Smoke::grenade().with_volume(2000.0), true),
            (Smoke::fog_bank(Vec3::new(6.0, 1.5, 4.0)), false),
            (Smoke::fog_bank(Vec3::new(8.0, 1.5, 4.0)), true),
            (Smoke::fog_bank(Vec3::new(6.0, 2.0, 4.0)), true),
            (Smoke::wall(Vec3::new(10.0, 0.0, 0.0)), false),
            (Smoke::wall(Vec3::new(10.0, 0.0, 10.0)), false),
            (Smoke::wall(Vec3::new(16.0, 0.0, 0.0)), true),
        ] {
            let mut app = headless_app();
            let source = smoke.source;
            app.world_mut().spawn((smoke, Transform::from_xyz(0.0, 1.5, 0.0)));
            app.update();
            assert_eq!(warned(&app, Mistake::TooBig), too_big, "{source:?}");
        }
    }

    #[test]
    fn a_smoke_inside_solid_geometry_is_warned() {
        let mut app = headless_app();
        let solid = BoxObstacles::new(vec![ObstacleBox::from_center_size(Vec3::ZERO, Vec3::splat(10.0))]);
        app.insert_resource(SmokeLevel::new(solid));
        app.world_mut().spawn((Smoke::grenade(), Transform::default()));
        assert!(run_until_warned(&mut app, Mistake::InsideSolid));
        assert!(!warned(&app, Mistake::NoLevel));
    }

    #[test]
    fn more_smokes_than_max_clouds_are_warned() {
        let mut plugin = SmokePlugin::default();
        plugin.config.tuning.max_clouds = 2;
        let mut app = headless_app_with(plugin);
        for k in 0..2 {
            app.world_mut().spawn((
                Smoke::grenade().with_seed(k + 1),
                Transform::from_xyz(10.0 * k as f32, 0.0, 0.0),
            ));
        }
        run_until_drawn(&mut app);
        assert!(!warned(&app, Mistake::TooManyClouds));
        app.world_mut()
            .spawn((Smoke::grenade().with_seed(3), Transform::from_xyz(20.0, 0.0, 0.0)));
        assert!(run_until_warned(&mut app, Mistake::TooManyClouds));
    }

    #[test]
    fn obstacles_without_a_mesh_are_warned() {
        let mut app = headless_app();
        app.world_mut().spawn((SmokeObstacle, Transform::default()));
        app.update();
        assert!(warned(&app, Mistake::ObstacleWithoutMesh));
    }

    #[test]
    fn an_obstacle_mesh_that_fails_to_load_is_warned() {
        let mut app = headless_app();
        let missing = app
            .world()
            .resource::<AssetServer>()
            .load::<Mesh>("missing.glb#Mesh0/Primitive0");
        app.world_mut().spawn((Mesh3d(missing), SmokeObstacle));
        app.world_mut().spawn((Smoke::grenade(), Transform::default()));
        assert!(run_until_warned(&mut app, Mistake::MeshFailed));
        // The smoke waits for the level.
        assert!(app.world().resource::<SmokeFrame>().slots.is_empty());
    }

    /// A server's app: Bevy's minimal plugins, no renderer and no assets,
    /// and transforms only if asked.
    fn server_app(transforms: bool) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, SmokePlugin::default()))
            .insert_resource(SmokeLevel::new(FlatGround::new(0.0, UpAxis::Y)))
            .insert_resource(SmokeClock {
                auto: false,
                ..default()
            });
        if transforms {
            app.add_plugins(TransformPlugin);
        }
        app.finish();
        app.cleanup();
        app
    }

    /// A dedicated server: smokes pop where their entities are and
    /// `SmokeVision` answers, while nothing is drawn or kept for drawing.
    #[test]
    fn a_dedicated_server_runs_on_minimal_plugins() {
        use bevy::ecs::system::SystemState;

        let mut app = server_app(true);
        let at = Vec3::new(5.0, 0.0, -3.0);
        let smoke = app
            .world_mut()
            .spawn((Smoke::grenade(), Transform::from_translation(at)))
            .id();
        // Five seconds, a tick a frame.
        for tick in 0..640 {
            app.world_mut().resource_mut::<SmokeClock>().tick = tick;
            app.update();
        }
        let cloud = app
            .world()
            .get::<Smoke>(smoke)
            .and_then(Smoke::cloud)
            .expect("never popped");
        let origin = app
            .world()
            .resource::<SmokeWorld>()
            .cloud(cloud)
            .map(|c| c.spec().origin);
        assert_eq!(origin, Some(at));
        let mut state = SystemState::<SmokeVision>::new(app.world_mut());
        let vision = state.get(app.world()).expect("the plugin's resources");
        assert_eq!(vision.tick(), 639);
        assert!(vision.blocks_sight(at + Vec3::new(-8.0, 1.0, 0.0), at + Vec3::new(8.0, 1.0, 0.0)));
        assert!(app.world().resource::<SmokeFrame>().slots.is_empty());
        assert_eq!(app.world().resource::<SmokeUploads>().len(), 0);
        assert!(app.world().resource::<SetupWarnings>().none());
    }

    /// On a server with Bevy's asset and mesh plugins, `SmokeObstacle`
    /// builds the level from marked meshes, as in a game.
    #[test]
    fn a_server_builds_the_level_from_marked_meshes() {
        use bevy::{asset::AssetPlugin, mesh::MeshPlugin};

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            TransformPlugin,
            AssetPlugin::default(),
            MeshPlugin,
            SmokePlugin::default(),
        ));
        app.finish();
        app.cleanup();
        let floor = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Plane3d::default().mesh().size(50.0, 50.0));
        app.world_mut().spawn((Mesh3d(floor), SmokeObstacle));
        for _ in 0..500 {
            app.update();
            if app.world().contains_resource::<SmokeLevel>() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(app.world().contains_resource::<SmokeLevel>(), "no level was built");
        assert!(app.world().resource::<SetupWarnings>().none());
    }

    /// Without `TransformPlugin`, smokes placed by their entity are warned
    /// about; a server's specs need no transforms.
    #[test]
    fn smokes_without_transforms_are_warned() {
        let mut app = server_app(false);
        app.world_mut()
            .spawn((Smoke::grenade(), Transform::from_xyz(5.0, 0.0, 0.0)));
        app.update();
        assert!(warned(&app, Mistake::NoTransforms));

        let mut app = server_app(false);
        let spec = CloudSpec::new(Vec3::new(5.0, 0.0, 0.0), 0, 1, SmokeStyle::Volumetric);
        app.world_mut().spawn(Smoke::from_spec(spec));
        app.update();
        assert!(!warned(&app, Mistake::NoTransforms));
    }

    /// `SmokeVision` only reads: a system may hold it and `SmokeWorld` too,
    /// and its answers are the world's own, to the bit.
    #[test]
    fn smoke_vision_only_reads() {
        #[derive(Resource, Default)]
        struct Seen(Vec<u32>);

        let line = (Vec3::new(-8.0, 1.0, 0.3), Vec3::new(8.0, 1.1, -0.2));
        let mut app = headless_app();
        app.insert_resource(SmokeClock {
            tick: 0,
            auto: false,
            ..default()
        })
        .init_resource::<Seen>()
        .add_systems(
            Update,
            move |vision: SmokeVision, world: Res<SmokeWorld>, mut seen: ResMut<Seen>| {
                let view = vision.view();
                seen.0 = vec![
                    view.occlusion(line.0, line.1).to_bits(),
                    vision.drawn_view().occlusion(line.0, line.1).to_bits(),
                    vision.density_at(Vec3::Y).to_bits(),
                    world.clouds().count() as u32,
                ];
            },
        );
        app.world_mut().spawn((Smoke::grenade(), Transform::default()));
        app.update();
        app.world_mut().resource_mut::<SmokeClock>().tick = 400;
        app.update();
        let mut world = app.world_mut().resource_mut::<SmokeWorld>();
        let view = world.view(400);
        let theirs = vec![
            view.occlusion(line.0, line.1).to_bits(),
            view.occlusion(line.0, line.1).to_bits(),
            view.density_at(Vec3::Y).to_bits(),
            1,
        ];
        assert!(view.occlusion(line.0, line.1) > 1.0, "the smoke never grew");
        assert_eq!(app.world().resource::<Seen>().0, theirs);
    }

    /// `SmokeVision` answers as of the views the plugin prepares: a bullet
    /// put straight into `SmokeWorld` in `Update` shows once
    /// `SmokeSystems::Simulate` is done, or at once after
    /// `SmokeWorld::prepare_vision`.
    #[test]
    fn smoke_vision_answers_as_of_its_views() {
        #[derive(Resource, Default)]
        struct Seen(Vec<u32>);

        let line = (Vec3::new(-8.0, 1.0, 0.3), Vec3::new(8.0, 1.1, -0.2));
        let record = move |vision: SmokeVision, mut seen: ResMut<Seen>| {
            seen.0.push(vision.occlusion(line.0, line.1).to_bits());
        };
        for refresh in [false, true] {
            let mut app = headless_app();
            app.insert_resource(SmokeClock {
                tick: 0,
                auto: false,
                ..default()
            })
            .init_resource::<Seen>();
            app.world_mut().spawn((Smoke::grenade(), Transform::default()));
            app.update();
            app.world_mut().resource_mut::<SmokeClock>().tick = 400;
            app.update();
            let before = app
                .world_mut()
                .resource_mut::<SmokeWorld>()
                .occlusion(line.0, line.1, 400);
            app.add_systems(
                Update,
                (
                    move |mut world: ResMut<SmokeWorld>, clock: Res<SmokeClock>| {
                        world.bullet(line.0, line.1, clock.tick - 5);
                        if refresh {
                            world.prepare_vision(&clock);
                        }
                    },
                    record,
                )
                    .chain(),
            )
            .add_systems(PostUpdate, record.after(SmokeSystems::Simulate));
            app.update();
            let after = app
                .world_mut()
                .resource_mut::<SmokeWorld>()
                .occlusion(line.0, line.1, 400);
            assert!(after < before, "the bullet left no tunnel: {before} → {after}");
            let in_update = if refresh { after } else { before };
            let seen = &app.world().resource::<Seen>().0;
            assert_eq!(seen, &[in_update.to_bits(), after.to_bits()], "refresh: {refresh}");
        }
    }

    /// A clock moved after the plugin prepared `SmokeVision`'s views (a
    /// fixed step's tick, say) is warned about, unless the views are
    /// prepared again.
    #[test]
    fn a_clock_moved_after_the_views_are_prepared_is_warned() {
        for refresh in [false, true] {
            let mut app = headless_app();
            app.insert_resource(SmokeClock {
                tick: 100,
                auto: false,
                ..default()
            })
            .add_systems(
                Update,
                (
                    move |mut clock: ResMut<SmokeClock>, mut world: ResMut<SmokeWorld>| {
                        clock.tick += 1;
                        if refresh {
                            world.prepare_vision(&clock);
                        }
                    },
                    move |vision: SmokeVision| assert_eq!(vision.tick(), 101 - u64::from(!refresh)),
                )
                    .chain(),
            );
            app.update();
            assert_eq!(warned(&app, Mistake::VisionTick), !refresh);
        }
    }

    /// The timing builders override the tuning's timeline field by field;
    /// the look and the entity's fate are the smoke's own.
    #[test]
    fn smokes_pop_with_their_own_timing() {
        let mut app = headless_app();
        let timed = Smoke::grenade()
            .with_start(2.0)
            .with_fade_time(1.5)
            .with_density(0.4)
            .with_despawn_when_done(false);
        let timed = app.world_mut().spawn(timed).id();
        let plain = app.world_mut().spawn(Smoke::grenade().with_seed(2)).id();
        app.update();
        let timeline = |e: Entity| {
            let cloud = app
                .world()
                .get::<Smoke>(e)
                .and_then(Smoke::cloud)
                .expect("never popped");
            app.world()
                .resource::<SmokeWorld>()
                .cloud(cloud)
                .and_then(|c| c.spec().timeline)
        };
        let base = app.world().resource::<SmokeWorld>().config().tuning.timeline;
        let expected = SmokeTimeline {
            start: 2.0,
            fade_time: 1.5,
            ..base
        };
        assert_eq!((timeline(timed), timeline(plain)), (Some(expected), None));
        let smoke = app.world().get::<Smoke>(timed).expect("the smoke");
        assert_eq!((smoke.density, smoke.despawn_when_done), (0.4, false));
    }

    #[test]
    #[should_panic(expected = "SmokePlugin: Config::tick_rate is 0: it must be above 0")]
    fn smoke_plugin_refuses_a_nonsense_config() {
        App::new().add_plugins((MinimalPlugins, SmokePlugin::new(Config::new(Units::METRES_Y_UP, 0.0))));
    }
}
