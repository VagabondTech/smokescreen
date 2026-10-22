//! GPU side: texture atlas, bake passes, and the per-camera passes of both
//! styles.
//!
//! Frame flow in the render world:
//! - `ExtractSchedule`: copy [`SmokeFrame`] (a few KB, plus this frame's puff
//!   transforms for faceted smokes) and drain new grid uploads.
//! - `Render / PrepareResources`: upload new grids (and puff bins) into their
//!   slot, fill the global uniform, size per-camera buffers and work out which
//!   part of each camera's screen the smokes can cover.
//! - `RenderGraph` (before cameras): bake density + lighting for the slots that
//!   changed. Usually zero, one or two slots.
//! - `Core3d` between the opaque (and transmissive) and transparent passes,
//!   so glass and particles in front of a smoke draw over it (and, as the
//!   smoke writes no depth, those behind it too), and before TAA, bloom and
//!   tonemapping:
//!   - faceted, full resolution: one fragment pass traces and shades;
//!   - faceted, reduced resolution: coarse trace (compute), classify every
//!     pixel (compute: facet interiors shaded from the coarse samples, the
//!     rest queued), retrace the queue (indirect compute), composite;
//!   - volumetric: raymarch at reduced resolution, then composite.

use std::sync::{Arc, Mutex};

use bevy::{
    asset::{embedded_asset, load_embedded_asset},
    camera::Camera3d,
    core_pipeline::{
        core_3d::{Transparent3d, main_opaque_pass_3d, main_transparent_pass_3d, prepare_core_3d_depth_textures},
        schedule::{Core3d, Core3dSystems, camera_driver},
    },
    pbr::main_transmissive_pass_3d,
    platform::collections::HashSet,
    prelude::*,
    render::{
        ExtractSchedule, GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems,
        camera::ExtractedCamera,
        diagnostic::RecordDiagnostics,
        extract_component::ExtractComponentPlugin,
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_phase::ViewSortedRenderPhases,
        render_resource::{
            binding_types::{
                sampler, storage_buffer_read_only_sized, storage_buffer_sized, texture_2d, texture_3d,
                texture_depth_2d, texture_depth_2d_multisampled, texture_storage_2d, texture_storage_3d,
                uniform_buffer,
            },
            *,
        },
        renderer::{RenderContext, RenderDevice, RenderGraph, RenderQueue, ViewQuery},
        texture::{CachedTexture, TextureCache},
        view::{ExtractedView, Msaa, ViewDepthStencilTexture, ViewTarget},
    },
    shader::{Shader, ShaderDefVal, load_shader_library},
};

use smokescreen::{CloudShape, Facets, HoleShape, LivePuff};

use crate::{
    NoiseWithinRule, SmokeCamera, SmokeConfig, SmokeSettings,
    noise::{self, NOISE_SIZE},
    quality::{FacetOcclusion, FacetShadows, SmokeQuality, screen_rect},
};

/// These must match the array sizes in `smoke_common.wesl`
/// (`limits_match_the_shaders` checks).
pub(crate) const MAX_SLOTS: usize = 32;
/// Holes drawn at once, across all smokes: the default
/// `Tuning::max_holes_per_cloud` (16) for every slot. `GpuGlobals` is then
/// 22.5 KiB, a uniform buffer well within the 64 KiB WebGPU allows.
pub(crate) const MAX_HOLES: usize = 512;
pub(crate) const MAX_LIGHTS: usize = 8;
const BAKE_WORKGROUP: u32 = 4;
const MARCH_WORKGROUP: u32 = 8;

/// One puff as the shaders read it: 16 floats.
///
/// `[center.xyz, radius, row0.xyz, shape, row1.xyz, corner_ratio, row2.xyz, 0]`
/// where the rows are the world-to-local rotation (internal frame).
pub(crate) type GpuPuff = [f32; 16];

/// A puff at this tick, packed for the GPU.
pub(crate) fn pack_puff(p: &LivePuff) -> GpuPuff {
    let (r0, r1, r2) = (p.to_local.row(0), p.to_local.row(1), p.to_local.row(2));
    [
        p.center.x,
        p.center.y,
        p.center.z,
        p.radius,
        r0.x,
        r0.y,
        r0.z,
        p.shape as u32 as f32,
        r1.x,
        r1.y,
        r1.z,
        p.ratio,
        r2.x,
        r2.y,
        r2.z,
        0.0,
    ]
}

/// `SmokeSlot::holes.z` value marking a faceted smoke.
const STYLE_FACETED: u32 = 1;
/// `SmokeView::facet.w` flags (the last one for the volumetric composite).
const FLAG_TRACED_SHADOWS: u32 = 1;
const FLAG_TRACED_OCCLUSION: u32 = 2;
const FLAG_DEBUG_SILHOUETTE: u32 = 4;
const FLAG_DEBUG_TRANSMITTANCE: u32 = 8;

// ---------------------------------------------------------------------------
// Data handed from the main world.
// ---------------------------------------------------------------------------

/// Per-frame data of a faceted smoke.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameFacet {
    pub bins_min: Vec3,
    pub cell: f32,
    pub dims: UVec3,
    /// Where this smoke's puffs start in [`SmokeFrame::facet_puffs`].
    pub puff_offset: u32,
    pub puff_count: u32,
    pub blend: f32,
    pub region_pad: f32,
    /// Typical puff inradius, metres.
    pub puff_size: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameSlot {
    pub layer: u32,
    pub grid_min: Vec3,
    pub voxel_size: f32,
    /// Bounds of everything the smoke draws (puff bins for faceted smokes).
    /// Rays march from where they enter this box, so their samples stay put
    /// while the smoke grows and fades.
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    /// What the smoke occupies at this tick (see `drawn_bounds`): only the
    /// screen rectangle this covers is dispatched.
    pub drawn_min: Vec3,
    pub drawn_max: Vec3,
    pub growth: f32,
    pub dissolve: f32,
    pub density: f32,
    pub tint: Vec3,
    pub seed: f32,
    pub hole_start: u32,
    pub hole_count: u32,
    pub facet: Option<FrameFacet>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameLight {
    pub position: Vec3,
    pub range: f32,
    pub color: Vec3,
}

/// Everything the renderer needs this frame. Rebuilt by `build_frame`, its
/// lights by `gather_point_lights`.
#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub(crate) struct SmokeFrame {
    pub slots: Vec<FrameSlot>,
    pub holes: Vec<HoleShape>,
    pub lights: Vec<FrameLight>,
    /// Indices into `slots` whose baked volume must be refreshed.
    pub bake: Vec<u32>,
    /// This frame's puffs of all faceted smokes, packed for the GPU.
    pub facet_puffs: Vec<GpuPuff>,
    pub has_sun: bool,
    pub sun_direction: Vec3,
    pub sun_color: Vec3,
    pub ambient: Vec3,
    pub settings: SmokeSettings,
    /// The rule's [`Tuning::sight_threshold`](smokescreen::Tuning), metres.
    pub sight_threshold: f32,
    pub voxel_size: f32,
    pub grid_dims: UVec3,
    /// The core's internal frame (metres, Y up) from the Bevy world's: all
    /// smoke data is in the internal frame, cameras are moved into it.
    pub internal_from_world: Mat4,
    /// Seconds on the volumetric noise's clock, from the drawn tick.
    pub time: f32,
    pub frame: u32,
}

/// Per-voxel sun and sky visibility of a cloud.
pub(crate) type Lighting = Arc<Vec<[u8; 2]>>;

pub(crate) struct SlotUpload {
    pub layer: u32,
    pub shape: Arc<CloudShape>,
    pub lighting: Option<Lighting>,
}

/// New grids waiting for upload. Shared by both worlds and drained during
/// extraction, so an upload always lands in the same frame as the slot that
/// uses it.
#[derive(Resource, Clone, Default)]
pub(crate) struct SmokeUploads(pub Arc<Mutex<Vec<SlotUpload>>>);

// ---------------------------------------------------------------------------
// GPU-side layouts. Must match `smoke_common.wesl`.
// ---------------------------------------------------------------------------

#[derive(ShaderType, Clone, Copy, Default)]
struct GpuSlot {
    /// xyz: world-space min corner of the voxel grid, w: voxel size.
    grid_min: Vec4,
    /// xyz: draw bounds min, w: atlas layer.
    bounds_min: Vec4,
    /// xyz: draw bounds max, w: dissolve (0..1).
    bounds_max: Vec4,
    /// x: growth front, y: density scale, z: extra erosion, w: noise seed offset.
    params: Vec4,
    /// rgb: albedo tint.
    tint: Vec4,
    /// x: first hole, y: hole count, z: style (1 = faceted).
    holes: UVec4,
    /// Faceted only. xyz: bins min corner, w: bin cell size.
    facet_grid: Vec4,
    /// xyz: bins per axis, w: first cell in the cell buffer.
    facet_dims: UVec4,
    /// x: first index in the index buffer, y: first puff, z: puff count.
    facet_ofs: UVec4,
    /// x: smooth-union radius, y: region clip padding.
    facet_shape: Vec4,
    /// xyz: what the smoke occupies at this tick (`FrameSlot::drawn_*`): a
    /// sample outside reads nothing, a voxel whose centre is outside holds
    /// nothing.
    drawn_min: Vec4,
    drawn_max: Vec4,
}

#[derive(ShaderType, Clone, Copy, Default)]
struct GpuHole {
    /// xyz: capsule start, w: radius.
    a: Vec4,
    /// xyz: capsule end, w: strength.
    b: Vec4,
}

#[derive(ShaderType, Clone, Copy, Default)]
struct GpuLight {
    position_range: Vec4,
    color: Vec4,
}

#[derive(ShaderType, Clone)]
struct GpuGlobals {
    slots: [GpuSlot; MAX_SLOTS],
    holes: [GpuHole; MAX_HOLES],
    lights: [GpuLight; MAX_LIGHTS],
    bake_slots: [UVec4; MAX_SLOTS / 4],
    /// xyz: voxels per slot, w: atlas layers.
    grid: UVec4,
    /// x: active slots, y: lights, z: slots to bake, w: frame.
    counts: UVec4,
    /// xyz: towards the sun, w: 1 if there is a sun.
    sun_direction: Vec4,
    sun_color: Vec4,
    ambient: Vec4,
    /// rgb: albedo, w: extinction.
    medium: Vec4,
    /// x: frequency, y: erosion, z: detail erosion, w: time.
    noise: Vec4,
    /// xyz: wind, w: anisotropy.
    wind: Vec4,
    /// x: multi-scattering, y: indoor ambient, z: ambient occlusion, w: voxel size.
    lighting: Vec4,
    /// For `SmokeSettings::noise_within_rule`. Capped: x, how much cloud
    /// (metres at full density) the noise may take from a line of sight.
    /// Filled: from y to z metres of cloud, the opacity the noise took is
    /// filled back in.
    fairness: Vec4,
}

impl GpuGlobals {
    fn zeroed() -> Self {
        Self {
            slots: [GpuSlot::default(); MAX_SLOTS],
            holes: [GpuHole::default(); MAX_HOLES],
            lights: [GpuLight::default(); MAX_LIGHTS],
            bake_slots: [UVec4::ZERO; MAX_SLOTS / 4],
            grid: UVec4::ZERO,
            counts: UVec4::ZERO,
            sun_direction: Vec4::ZERO,
            sun_color: Vec4::ZERO,
            ambient: Vec4::ZERO,
            medium: Vec4::ZERO,
            noise: Vec4::ZERO,
            wind: Vec4::ZERO,
            lighting: Vec4::ZERO,
            fairness: Vec4::ZERO,
        }
    }
}

#[derive(ShaderType, Clone, Copy)]
struct GpuView {
    world_from_clip: Mat4,
    /// xyz: camera position, w: near plane.
    camera_position: Vec4,
    /// xy: viewport size, zw: volumetric smoke buffer size.
    target_size: Vec4,
    /// xy: viewport origin in the render target.
    viewport: Vec4,
    /// x: exposure, y: step size, z: step growth, w: volumetric resolution divisor.
    march: Vec4,
    /// Volumetric smoke-buffer rectangle to march: min.xy, max.xy (exclusive).
    rect: UVec4,
    /// x: max steps, y: jitter frame, z: the slot a single-smoke march reads.
    params: UVec4,
    /// Faceted. x: coarse divisor, y: max steps, z: shadow steps, w: flags
    /// (one of them for the volumetric composite).
    facet: UVec4,
    /// Faceted. x: hit tolerance (pixels), y: angle of one pixel (radians),
    /// z: shading tolerance.
    facet_f: Vec4,
    /// Faceted coarse rectangle: min.xy, max.xy (exclusive), coarse pixels.
    facet_rect: UVec4,
    /// Faceted rectangle: min.xy, max.xy (exclusive), viewport pixels.
    facet_px: UVec4,
}

// ---------------------------------------------------------------------------
// Render-world resources.
// ---------------------------------------------------------------------------

const NOISE_SEED: u32 = 0x51ce;

/// The detail noise, generated on a background thread started when the
/// plugin is added and joined the first time the GPU texture is created.
#[derive(Resource)]
struct NoiseData {
    pending: Mutex<Option<std::thread::JoinHandle<Vec<u8>>>>,
    ready: std::sync::OnceLock<Arc<Vec<u8>>>,
}

impl NoiseData {
    fn start() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let pending = std::thread::Builder::new()
            .name("smoke_noise".into())
            .spawn(|| noise::generate_noise(NOISE_SEED))
            .ok();
        #[cfg(target_arch = "wasm32")]
        let pending = None;
        Self {
            pending: Mutex::new(pending),
            ready: std::sync::OnceLock::new(),
        }
    }

    fn get(&self) -> &Arc<Vec<u8>> {
        self.ready.get_or_init(|| {
            let handle = self.pending.lock().unwrap().take();
            Arc::new(
                handle
                    .and_then(|h| h.join().ok())
                    .unwrap_or_else(|| noise::generate_noise(NOISE_SEED)),
            )
        })
    }
}

#[derive(Resource)]
struct SmokeGpu {
    source: Texture,
    source_view: TextureView,
    /// Per voxel, 1 where the level is open, 0 where it is solid: the march
    /// divides filtered density by it, so smoke stays dense up to walls.
    open: Texture,
    open_view: TextureView,
    density_view: TextureView,
    lit_view: TextureView,
    noise_view: TextureView,
    linear_clamp: Sampler,
    linear_repeat: Sampler,
    globals: UniformBuffer<GpuGlobals>,
    /// Faceted style: this frame's puffs, and each slot's bins.
    facet_puffs: Buffer,
    facet_cells: Buffer,
    facet_indices: Buffer,
    generation: u32,
}

/// The faceted retrace queue: indirect dispatch arguments, a count, and one
/// word per queued pixel. Shared by all cameras (their passes run in order)
/// and grown to the largest viewport seen.
#[derive(Resource, Default)]
struct FacetWork {
    buffer: Option<Buffer>,
    /// Pixels it can hold.
    capacity: u64,
}

/// Bytes before the pixel list: dispatch x, y, z and the count.
const FACET_WORK_HEADER: u64 = 16;
/// Reduced resolution only pays while puffs are big on screen: a divisor is
/// used only if the nearest visible faceted smoke's puffs (inradius) span at
/// least this many pixels per divisor step. Smaller, and most pixels sit on
/// a silhouette or crease and get traced again anyway.
const MIN_PUFF_PIXELS: f32 = 30.0;

/// A layer's cloud and its lighting.
type UploadedLayer = (Arc<CloudShape>, Option<Lighting>);

#[derive(Resource, Default)]
struct SmokeRenderState {
    pending_uploads: Vec<SlotUpload>,
    /// CPU copy of each layer's data, re-uploaded if the device is recreated.
    uploaded: Vec<Option<UploadedLayer>>,
    generation: u32,
    /// Layers that still need a bake (kept until a bake actually ran).
    pending_bake: HashSet<u32>,
    baking_layers: Vec<u32>,
    bake_count: u32,
}

#[derive(Resource, Default)]
struct SmokeViewUniforms(DynamicUniformBuffer<GpuView>);

#[derive(Resource)]
struct SmokePipelines {
    bake_density_layout: BindGroupLayoutDescriptor,
    bake_light_layout: BindGroupLayoutDescriptor,
    march_layouts: [BindGroupLayoutDescriptor; 2],
    facet_coarse_layouts: [BindGroupLayoutDescriptor; 2],
    facet_classify_layouts: [BindGroupLayoutDescriptor; 2],
    facet_retrace_layouts: [BindGroupLayoutDescriptor; 2],
    bake_density: CachedComputePipelineId,
    bake_light: CachedComputePipelineId,
    /// By `SmokeSettings::noise_within_rule` (off, capped, filled), then MSAA.
    march: [[CachedComputePipelineId; 2]; 3],
    facet_coarse: [CachedComputePipelineId; 2],
    facet_classify: [CachedComputePipelineId; 2],
    facet_retrace: [CachedComputePipelineId; 2],
}

/// The volumetric march's kernels for one smoke per dispatch
/// (`SINGLE_SMOKE`) and for up to 8 smokes along a ray (`RAY_SMOKES_8`),
/// with or without the point lights, per noise mode and MSAA. Unlike
/// `SmokePipelines::march`, which the renderer falls back on, they compile on
/// first use: the noise mode and MSAA in use on the first frame.
#[derive(Resource, Clone)]
struct MarchSpecializer {
    layouts: [BindGroupLayoutDescriptor; 2],
    shader: Handle<Shader>,
}

/// The most smokes one ray may meet in the march kernel with short per-ray
/// arrays (`RAY_SMOKES_8` in smoke_march.wesl).
const OVERLAP8_SMOKES: usize = 8;

/// What a specialised march kernel adds up along a ray.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum MarchKernel {
    /// One smoke, whose slot the dispatch's view uniforms hold.
    Single,
    /// Every smoke a ray crosses, up to `OVERLAP8_SMOKES` of them.
    Overlap8,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MarchKey {
    kernel: MarchKernel,
    rule: NoiseWithinRule,
    msaa: bool,
    /// Whether the point lights are compiled in.
    lights: bool,
}

impl SpecializedComputePipeline for MarchSpecializer {
    type Key = MarchKey;

    fn specialize(&self, key: Self::Key) -> ComputePipelineDescriptor {
        let (kernel, label) = match key.kernel {
            MarchKernel::Single => ("SINGLE_SMOKE", "smoke_march_single"),
            MarchKernel::Overlap8 => ("RAY_SMOKES_8", "smoke_march_overlap8"),
        };
        let mut defs: Vec<ShaderDefVal> = vec![kernel.into()];
        if key.msaa {
            defs.push("MULTISAMPLED".into());
        }
        match key.rule {
            NoiseWithinRule::Off => {}
            NoiseWithinRule::Capped => defs.push("NOISE_CAPPED".into()),
            NoiseWithinRule::Filled => defs.push("NOISE_FILLED".into()),
        }
        if !key.lights {
            defs.push("NO_POINT_LIGHTS".into());
        }
        ComputePipelineDescriptor {
            label: Some(label.into()),
            layout: vec![self.layouts[key.msaa as usize].clone()],
            shader: self.shader.clone(),
            shader_defs: defs,
            entry_point: Some("march".into()),
            ..default()
        }
    }
}

/// Fullscreen pipelines blended over the main texture: the volumetric
/// composite, the full-resolution faceted trace, and the faceted composite.
#[derive(Resource, Clone)]
struct OverlaySpecializer {
    composite_layouts: [BindGroupLayoutDescriptor; 2],
    trace_layouts: [BindGroupLayoutDescriptor; 2],
    facet_composite_layout: BindGroupLayoutDescriptor,
    composite_shader: Handle<Shader>,
    trace_shader: Handle<Shader>,
    facet_composite_shader: Handle<Shader>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum OverlayKind {
    Composite,
    FacetTrace,
    FacetComposite,
}

/// An overlay pipeline for each target it can draw into: the camera's
/// colour target (the multisampled texture with MSAA), then the resolved
/// image. Without MSAA the two are the same pipeline.
type OverlayPipelines = [CachedRenderPipelineId; 2];

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct OverlayKey {
    kind: OverlayKind,
    msaa: bool,
    /// MSAA samples of the colour target: the camera's (the multisampled
    /// texture, which the transparent pass then continues), or 1 for the
    /// resolved image when nothing transparent follows.
    samples: u32,
    hdr: bool,
    format: TextureFormat,
}

impl SpecializedRenderPipeline for OverlaySpecializer {
    type Key = OverlayKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        let mut defs: Vec<ShaderDefVal> = Vec::new();
        if key.msaa {
            defs.push("MULTISAMPLED".into());
        }
        if !key.hdr {
            defs.push("LDR_TARGET".into());
        }
        let (label, layout, shader) = match key.kind {
            OverlayKind::Composite => (
                "smoke_composite_pipeline",
                &self.composite_layouts[key.msaa as usize],
                &self.composite_shader,
            ),
            OverlayKind::FacetTrace => (
                "smoke_facet_trace_pipeline",
                &self.trace_layouts[key.msaa as usize],
                &self.trace_shader,
            ),
            OverlayKind::FacetComposite => (
                "smoke_facet_composite_pipeline",
                &self.facet_composite_layout,
                &self.facet_composite_shader,
            ),
        };
        RenderPipelineDescriptor {
            label: Some(label.into()),
            layout: vec![layout.clone()],
            vertex: VertexState {
                shader: shader.clone(),
                shader_defs: defs.clone(),
                entry_point: Some("vertex".into()),
                ..default()
            },
            fragment: Some(FragmentState {
                shader: shader.clone(),
                shader_defs: defs,
                entry_point: Some("fragment".into()),
                targets: vec![Some(ColorTargetState {
                    format: key.format,
                    // Premultiplied: scene * (1 - alpha) + colour, done by the
                    // blender so the scene is never copied.
                    blend: Some(BlendState {
                        color: BlendComponent {
                            src_factor: BlendFactor::One,
                            dst_factor: BlendFactor::OneMinusSrcAlpha,
                            operation: BlendOperation::Add,
                        },
                        alpha: BlendComponent {
                            src_factor: BlendFactor::Zero,
                            dst_factor: BlendFactor::One,
                            operation: BlendOperation::Add,
                        },
                    }),
                    write_mask: ColorWrites::ALL,
                })],
                ..default()
            }),
            multisample: MultisampleState {
                count: key.samples,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            ..default()
        }
    }
}

/// Volumetric passes of one camera.
struct VolumetricView {
    texture: CachedTexture,
    /// One march per group of smokes whose screen rectangles come close
    /// (`march_groups`): a single-smoke march for a smoke on its own.
    marches: Vec<MarchDispatch>,
    composite: OverlayPipelines,
    /// Composite scissors in render-target pixels: x, y, width, height. One
    /// per march.
    scissors: Vec<UVec4>,
}

struct MarchDispatch {
    pipeline: CachedComputePipelineId,
    /// Smoke-buffer rectangle to march: min.xy, max.xy (exclusive).
    rect: UVec4,
    /// The slot a single-smoke march reads; none for a march that adds up
    /// the smokes a ray crosses.
    slot: Option<u32>,
    /// This march's view uniforms: the camera's, with its rectangle and slot.
    uniform_offset: u32,
}

/// Faceted passes of one camera.
struct FacetView {
    passes: FacetPasses,
    /// Overlay scissor in render-target pixels: x, y, width, height.
    scissor: UVec4,
}

#[allow(clippy::large_enum_variant)]
enum FacetPasses {
    /// One fragment pass traces every pixel.
    Full { trace: OverlayPipelines },
    /// Coarse trace, classify, retrace, composite.
    Reduced {
        coarse_texture: CachedTexture,
        layer: CachedTexture,
        coarse: CachedComputePipelineId,
        classify: CachedComputePipelineId,
        retrace: CachedComputePipelineId,
        composite: OverlayPipelines,
        coarse_dispatch: UVec2,
        classify_dispatch: UVec2,
    },
}

impl FacetPasses {
    /// The pass that draws onto the camera's target.
    fn overlay(&self) -> &OverlayPipelines {
        match self {
            Self::Full { trace } => trace,
            Self::Reduced { composite, .. } => composite,
        }
    }
}

/// Per-camera smoke state for this frame. Absent when no smoke is on screen,
/// which skips the camera's smoke passes entirely.
#[derive(Component)]
struct ViewSmoke {
    uniform_offset: u32,
    msaa: bool,
    volumetric: Option<VolumetricView>,
    facets: Option<FacetView>,
}

// ---------------------------------------------------------------------------
// Plugin wiring.
// ---------------------------------------------------------------------------

pub(crate) fn build(app: &mut App, config: SmokeConfig, uploads: SmokeUploads) {
    load_shader_library!(app, "shaders/smoke_common.wesl");
    load_shader_library!(app, "shaders/smoke_facets.wesl");
    load_shader_library!(app, "shaders/smoke_facet_refine.wesl");
    embedded_asset!(app, "shaders/smoke_bake.wesl");
    embedded_asset!(app, "shaders/smoke_march.wesl");
    embedded_asset!(app, "shaders/smoke_composite.wesl");
    embedded_asset!(app, "shaders/smoke_facet_coarse.wesl");
    embedded_asset!(app, "shaders/smoke_facet_trace.wesl");
    embedded_asset!(app, "shaders/smoke_facet_classify.wesl");
    embedded_asset!(app, "shaders/smoke_facet_retrace.wesl");
    embedded_asset!(app, "shaders/smoke_facet_composite.wesl");

    app.add_plugins((
        ExtractResourcePlugin::<SmokeFrame>::default(),
        ExtractComponentPlugin::<SmokeCamera>::default(),
        ExtractComponentPlugin::<SmokeQuality>::default(),
    ));

    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        // Headless server: simulation and `SmokeVision` still work.
        return;
    };

    render_app
        .insert_resource(config)
        .insert_resource(uploads)
        .insert_resource(NoiseData::start())
        .init_resource::<SmokeRenderState>()
        .init_resource::<FacetWork>()
        .init_gpu_resource::<SmokeViewUniforms>()
        .init_gpu_resource::<SpecializedRenderPipelines<OverlaySpecializer>>()
        .init_gpu_resource::<SpecializedComputePipelines<MarchSpecializer>>()
        .add_systems(RenderStartup, (init_smoke_gpu, init_smoke_pipelines))
        .add_systems(ExtractSchedule, extract_uploads)
        .add_systems(
            Render,
            (
                enable_depth_sampling
                    .in_set(RenderSystems::Prepare)
                    .before(prepare_core_3d_depth_textures),
                (prepare_smoke, prepare_smoke_views)
                    .chain()
                    .in_set(RenderSystems::PrepareResources),
            ),
        )
        .add_systems(RenderGraph, bake_smoke.before(camera_driver))
        .add_systems(
            Core3d,
            smoke_pass
                .after(main_opaque_pass_3d)
                .after(main_transmissive_pass_3d)
                .before(main_transparent_pass_3d)
                .in_set(Core3dSystems::MainPass),
        );
}

pub(crate) fn finish(_app: &mut App) {}

fn init_smoke_gpu(
    mut commands: Commands,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    config: Res<SmokeConfig>,
    noise: Res<NoiseData>,
    mut generation: Local<u32>,
) {
    let dims = config.grid_dims;
    let atlas_size = Extent3d {
        width: dims.x,
        height: dims.y,
        depth_or_array_layers: dims.z * config.max_smokes,
    };
    let atlas = |label: &'static str, format: TextureFormat, usage: TextureUsages| {
        device.create_texture(&TextureDescriptor {
            label: Some(label),
            size: atlas_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D3,
            format,
            usage,
            view_formats: &[],
        })
    };
    let source = atlas(
        "smoke_source_atlas",
        TextureFormat::Rgba8Unorm,
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
    );
    let open = atlas(
        "smoke_open_atlas",
        TextureFormat::R8Unorm,
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
    );
    let density = atlas(
        "smoke_density_atlas",
        TextureFormat::Rgba8Unorm,
        TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING,
    );
    let lit = atlas(
        "smoke_lit_atlas",
        TextureFormat::Rgba8Unorm,
        TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING,
    );
    let noise_texture = device.create_texture_with_data(
        &queue,
        &TextureDescriptor {
            label: Some("smoke_noise"),
            size: Extent3d {
                width: NOISE_SIZE,
                height: NOISE_SIZE,
                depth_or_array_layers: NOISE_SIZE,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D3,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        },
        TextureDataOrder::LayerMajor,
        noise.get(),
    );
    let view = |t: &Texture| t.create_view(&TextureViewDescriptor::default());
    let sampler_with = |label: &'static str, mode: AddressMode| {
        device.create_sampler(&SamplerDescriptor {
            label: Some(label),
            address_mode_u: mode,
            address_mode_v: mode,
            address_mode_w: mode,
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..default()
        })
    };
    let mut globals = UniformBuffer::from(GpuGlobals::zeroed());
    globals.set_label(Some("smoke_globals"));
    globals.write_buffer(&device, &queue);

    let slots = config.max_smokes as u64;
    let storage = |label: &'static str, size: u64| {
        device.create_buffer(&BufferDescriptor {
            label: Some(label),
            size,
            usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    };
    let limits = config.facet_limits;
    let facet_puffs = storage(
        "smoke_facet_puffs",
        slots * limits.max_puffs as u64 * size_of::<GpuPuff>() as u64,
    );
    let facet_cells = storage("smoke_facet_cells", slots * limits.max_cells as u64 * 8);
    let facet_indices = storage("smoke_facet_indices", slots * limits.max_indices as u64 * 4);
    *generation += 1;
    commands.insert_resource(SmokeGpu {
        source_view: view(&source),
        open_view: view(&open),
        open,
        density_view: view(&density),
        lit_view: view(&lit),
        noise_view: view(&noise_texture),
        source,
        linear_clamp: sampler_with("smoke_linear_clamp", AddressMode::ClampToEdge),
        linear_repeat: sampler_with("smoke_linear_repeat", AddressMode::Repeat),
        globals,
        facet_puffs,
        facet_cells,
        facet_indices,
        generation: *generation,
    });
}

fn init_smoke_pipelines(mut commands: Commands, asset_server: Res<AssetServer>, pipeline_cache: Res<PipelineCache>) {
    let bake_density_layout = BindGroupLayoutDescriptor::new(
        "smoke_bake_density_layout",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::COMPUTE,
            (
                (0, uniform_buffer::<GpuGlobals>(false)),
                (1, texture_3d(TextureSampleType::Float { filterable: false })),
                (
                    2,
                    texture_storage_3d(TextureFormat::Rgba8Unorm, StorageTextureAccess::WriteOnly),
                ),
            ),
        ),
    );
    let bake_light_layout = BindGroupLayoutDescriptor::new(
        "smoke_bake_light_layout",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::COMPUTE,
            (
                (0, uniform_buffer::<GpuGlobals>(false)),
                (3, texture_3d(TextureSampleType::Float { filterable: true })),
                (4, sampler(SamplerBindingType::Filtering)),
                (
                    5,
                    texture_storage_3d(TextureFormat::Rgba8Unorm, StorageTextureAccess::WriteOnly),
                ),
                (6, texture_3d(TextureSampleType::Float { filterable: true })),
            ),
        ),
    );
    let depth = |msaa: bool| {
        if msaa {
            texture_depth_2d_multisampled()
        } else {
            texture_depth_2d()
        }
    };
    let march_layout = |msaa: bool| {
        BindGroupLayoutDescriptor::new(
            if msaa {
                "smoke_march_layout_msaa"
            } else {
                "smoke_march_layout"
            },
            &BindGroupLayoutEntries::sequential(
                ShaderStages::COMPUTE,
                (
                    uniform_buffer::<GpuGlobals>(false),
                    uniform_buffer::<GpuView>(true),
                    depth(msaa),
                    texture_3d(TextureSampleType::Float { filterable: true }),
                    sampler(SamplerBindingType::Filtering),
                    texture_3d(TextureSampleType::Float { filterable: true }),
                    sampler(SamplerBindingType::Filtering),
                    texture_storage_2d(TextureFormat::Rgba16Float, StorageTextureAccess::WriteOnly),
                    texture_3d(TextureSampleType::Float { filterable: true }),
                ),
            ),
        )
    };
    let composite_layout = |msaa: bool| {
        BindGroupLayoutDescriptor::new(
            if msaa {
                "smoke_composite_layout_msaa"
            } else {
                "smoke_composite_layout"
            },
            &BindGroupLayoutEntries::sequential(
                ShaderStages::FRAGMENT,
                (
                    uniform_buffer::<GpuView>(true),
                    depth(msaa),
                    texture_2d(TextureSampleType::Float { filterable: false }),
                ),
            ),
        )
    };
    // Faceted passes: bindings 0..=8 are shared (see `smoke_facets.wesl`).
    // The coarse pass adds 9: the coarse texture (first near approach, hit
    // distance, normal, lighting). Classify and retrace read it and add 10:
    // the smoke layer, and 11: the retrace queue.
    #[derive(Clone, Copy, PartialEq)]
    enum FacetPass {
        Trace,
        Coarse,
        Classify,
        Retrace,
    }
    let facet_layout = |label: &'static str, msaa: bool, pass: FacetPass| {
        let stages = if pass == FacetPass::Trace {
            ShaderStages::FRAGMENT
        } else {
            ShaderStages::COMPUTE
        };
        let mut entries = BindGroupLayoutEntries::sequential(
            stages,
            (
                uniform_buffer::<GpuGlobals>(false),
                uniform_buffer::<GpuView>(true),
                depth(msaa),
                texture_3d(TextureSampleType::Float { filterable: true }),
                texture_3d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                storage_buffer_read_only_sized(false, None),
                storage_buffer_read_only_sized(false, None),
                storage_buffer_read_only_sized(false, None),
            ),
        )
        .to_vec();
        let mut push = |binding: u32, ty: BindGroupLayoutEntryBuilder| {
            entries.push(ty.build(binding, stages));
        };
        match pass {
            FacetPass::Trace => {}
            FacetPass::Coarse => push(
                9,
                texture_storage_2d(TextureFormat::Rgba32Uint, StorageTextureAccess::WriteOnly),
            ),
            FacetPass::Classify | FacetPass::Retrace => {
                push(9, texture_2d(TextureSampleType::Uint));
                push(
                    10,
                    texture_storage_2d(TextureFormat::Rgba16Float, StorageTextureAccess::WriteOnly),
                );
                push(
                    11,
                    if pass == FacetPass::Classify {
                        storage_buffer_sized(false, None)
                    } else {
                        storage_buffer_read_only_sized(false, None)
                    },
                );
            }
        }
        BindGroupLayoutDescriptor::new(label, &entries)
    };
    let facet_composite_layout = BindGroupLayoutDescriptor::new(
        "smoke_facet_composite_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                uniform_buffer::<GpuView>(true),
                texture_2d(TextureSampleType::Float { filterable: false }),
            ),
        ),
    );

    let bake_shader: Handle<Shader> = load_embedded_asset!(asset_server.as_ref(), "shaders/smoke_bake.wesl");
    let march_shader: Handle<Shader> = load_embedded_asset!(asset_server.as_ref(), "shaders/smoke_march.wesl");
    let composite_shader: Handle<Shader> = load_embedded_asset!(asset_server.as_ref(), "shaders/smoke_composite.wesl");
    let coarse_shader: Handle<Shader> = load_embedded_asset!(asset_server.as_ref(), "shaders/smoke_facet_coarse.wesl");
    let trace_shader: Handle<Shader> = load_embedded_asset!(asset_server.as_ref(), "shaders/smoke_facet_trace.wesl");
    let classify_shader: Handle<Shader> =
        load_embedded_asset!(asset_server.as_ref(), "shaders/smoke_facet_classify.wesl");
    let retrace_shader: Handle<Shader> =
        load_embedded_asset!(asset_server.as_ref(), "shaders/smoke_facet_retrace.wesl");
    let facet_composite_shader: Handle<Shader> =
        load_embedded_asset!(asset_server.as_ref(), "shaders/smoke_facet_composite.wesl");

    let compute = |label: &'static str,
                   layout: &BindGroupLayoutDescriptor,
                   shader: &Handle<Shader>,
                   entry: &'static str,
                   defs: Vec<ShaderDefVal>| {
        pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some(label.into()),
            layout: vec![layout.clone()],
            shader: shader.clone(),
            shader_defs: defs,
            entry_point: Some(entry.into()),
            ..default()
        })
    };

    let march_layouts = [march_layout(false), march_layout(true)];
    let facet_coarse_layouts = [
        facet_layout("smoke_facet_coarse_layout", false, FacetPass::Coarse),
        facet_layout("smoke_facet_coarse_layout_msaa", true, FacetPass::Coarse),
    ];
    let facet_classify_layouts = [
        facet_layout("smoke_facet_classify_layout", false, FacetPass::Classify),
        facet_layout("smoke_facet_classify_layout_msaa", true, FacetPass::Classify),
    ];
    let facet_retrace_layouts = [
        facet_layout("smoke_facet_retrace_layout", false, FacetPass::Retrace),
        facet_layout("smoke_facet_retrace_layout_msaa", true, FacetPass::Retrace),
    ];
    let bake_density = compute(
        "smoke_bake_density",
        &bake_density_layout,
        &bake_shader,
        "bake_density",
        vec![],
    );
    let bake_light = compute(
        "smoke_bake_lighting",
        &bake_light_layout,
        &bake_shader,
        "bake_lighting",
        vec![],
    );
    // Keeping the noise within the rule is compiled in only where it's used:
    // its bookkeeping costs even when it changes nothing.
    let march_with = |rule: Option<&'static str>, label: &'static str, label_msaa: &'static str| {
        let defs = |msaa: bool| {
            let mut defs: Vec<ShaderDefVal> = Vec::new();
            if msaa {
                defs.push("MULTISAMPLED".into());
            }
            if let Some(rule) = rule {
                defs.push(rule.into());
            }
            defs
        };
        [
            compute(label, &march_layouts[0], &march_shader, "march", defs(false)),
            compute(label_msaa, &march_layouts[1], &march_shader, "march", defs(true)),
        ]
    };
    let march = [
        march_with(None, "smoke_march", "smoke_march_msaa"),
        march_with(Some("NOISE_CAPPED"), "smoke_march_capped", "smoke_march_capped_msaa"),
        march_with(Some("NOISE_FILLED"), "smoke_march_filled", "smoke_march_filled_msaa"),
    ];
    let both = |label: &'static str,
                layouts: &[BindGroupLayoutDescriptor; 2],
                shader: &Handle<Shader>,
                entry: &'static str| {
        [
            compute(label, &layouts[0], shader, entry, vec![]),
            compute(label, &layouts[1], shader, entry, vec!["MULTISAMPLED".into()]),
        ]
    };
    let facet_coarse = both("smoke_facet_coarse", &facet_coarse_layouts, &coarse_shader, "coarse");
    let facet_classify = both(
        "smoke_facet_classify",
        &facet_classify_layouts,
        &classify_shader,
        "classify",
    );
    let facet_retrace = both(
        "smoke_facet_retrace",
        &facet_retrace_layouts,
        &retrace_shader,
        "retrace",
    );

    commands.insert_resource(MarchSpecializer {
        layouts: march_layouts.clone(),
        shader: march_shader.clone(),
    });
    commands.insert_resource(SmokePipelines {
        bake_density_layout,
        bake_light_layout,
        march_layouts,
        facet_coarse_layouts,
        facet_classify_layouts,
        facet_retrace_layouts,
        bake_density,
        bake_light,
        march,
        facet_coarse,
        facet_classify,
        facet_retrace,
    });
    commands.insert_resource(OverlaySpecializer {
        composite_layouts: [composite_layout(false), composite_layout(true)],
        trace_layouts: [
            facet_layout("smoke_facet_trace_layout", false, FacetPass::Trace),
            facet_layout("smoke_facet_trace_layout_msaa", true, FacetPass::Trace),
        ],
        facet_composite_layout,
        composite_shader,
        trace_shader,
        facet_composite_shader,
    });
}

// ---------------------------------------------------------------------------
// Per-frame systems.
// ---------------------------------------------------------------------------

fn extract_uploads(uploads: Res<SmokeUploads>, mut state: ResMut<SmokeRenderState>) {
    let mut queue = uploads.0.lock().unwrap();
    state.pending_uploads.extend(queue.drain(..));
}

/// The smoke passes read the main pass depth buffer, which Bevy only makes
/// sampleable on request.
fn enable_depth_sampling(mut cameras: Query<&mut Camera3d, With<SmokeCamera>>) {
    for mut camera in &mut cameras {
        camera.depth_texture_usages.0 |= TextureUsages::TEXTURE_BINDING.bits();
    }
}

fn upload_grid(
    queue: &RenderQueue,
    gpu: &SmokeGpu,
    config: &SmokeConfig,
    layer: u32,
    shape: &CloudShape,
    lighting: Option<&Lighting>,
) {
    let dims = config.grid_dims;
    let grid = &shape.grid;
    if grid.dims != dims || layer >= config.max_smokes {
        warn!("Smoke grid {:?} doesn't fit atlas layer {layer}", grid.dims);
        return;
    }
    let write = |texture: &Texture, texels: &[u8], bytes_per_texel: u32| {
        queue.write_texture(
            TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: Origin3d {
                    x: 0,
                    y: 0,
                    z: layer * dims.z,
                },
                aspect: TextureAspect::All,
            },
            texels,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(dims.x * bytes_per_texel),
                rows_per_image: Some(dims.y),
            },
            Extent3d {
                width: dims.x,
                height: dims.y,
                depth_or_array_layers: dims.z,
            },
        );
    };
    write(&gpu.source, &grid.rgba_texels(lighting.map(|l| l.as_slice())), 4);
    write(&gpu.open, &grid.open_texels(), 1);
}

/// Copy a faceted cloud's bins into its slot's region of the cell and index
/// buffers. Puff transforms are uploaded every frame instead.
fn upload_facets(queue: &RenderQueue, gpu: &SmokeGpu, config: &SmokeConfig, layer: u32, facets: &Facets) {
    let bins = &facets.bins;
    let limits = config.facet_limits;
    if bins.ranges.len() > limits.max_cells || bins.indices.len() > limits.max_indices {
        warn!("Faceted smoke bins don't fit slot {layer}");
        return;
    }
    let cells: Vec<u32> = bins.ranges.iter().flat_map(|&(start, count)| [start, count]).collect();
    queue.write_buffer(
        &gpu.facet_cells,
        layer as u64 * limits.max_cells as u64 * 8,
        bytemuck::cast_slice(&cells),
    );
    if !bins.indices.is_empty() {
        queue.write_buffer(
            &gpu.facet_indices,
            layer as u64 * limits.max_indices as u64 * 4,
            bytemuck::cast_slice(&bins.indices),
        );
    }
}

/// Upload a layer's grid (and bins, if faceted).
fn upload_layer(queue: &RenderQueue, gpu: &SmokeGpu, config: &SmokeConfig, layer: u32, data: &UploadedLayer) {
    upload_grid(queue, gpu, config, layer, &data.0, data.1.as_ref());
    if let Some(facets) = &data.0.facets {
        upload_facets(queue, gpu, config, layer, facets);
    }
}

fn prepare_smoke(
    frame: Res<SmokeFrame>,
    gpu: Option<ResMut<SmokeGpu>>,
    mut state: ResMut<SmokeRenderState>,
    config: Res<SmokeConfig>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    let Some(mut gpu) = gpu else {
        return;
    };
    let state = state.as_mut();
    let layers = config.max_smokes as usize;
    if state.uploaded.len() < layers {
        state.uploaded.resize(layers, None);
    }

    // Fresh GPU resources (first frame or a recovered device): restore every
    // grid we know about.
    if state.generation != gpu.generation {
        state.generation = gpu.generation;
        for layer in 0..layers {
            if let Some(data) = state.uploaded[layer].clone() {
                upload_layer(&queue, &gpu, &config, layer as u32, &data);
                state.pending_bake.insert(layer as u32);
            }
        }
    }
    for upload in state.pending_uploads.drain(..) {
        let data = (upload.shape, upload.lighting);
        upload_layer(&queue, &gpu, &config, upload.layer, &data);
        if let Some(slot) = state.uploaded.get_mut(upload.layer as usize) {
            *slot = Some(data);
        }
        state.pending_bake.insert(upload.layer);
    }

    // This frame's puff transforms, all faceted smokes back to back.
    let limits = config.facet_limits;
    let puff_capacity = config.max_smokes as usize * limits.max_puffs;
    if !frame.facet_puffs.is_empty() {
        let n = frame.facet_puffs.len().min(puff_capacity);
        queue.write_buffer(&gpu.facet_puffs, 0, bytemuck::cast_slice(&frame.facet_puffs[..n]));
    }

    // Bake what the main world asked for plus anything still pending.
    let active: HashSet<u32> = frame.slots.iter().map(|s| s.layer).collect();
    state.pending_bake.retain(|layer| active.contains(layer));
    let mut bake: Vec<u32> = Vec::new();
    for (i, slot) in frame.slots.iter().enumerate().take(MAX_SLOTS) {
        if frame.bake.contains(&(i as u32)) || state.pending_bake.contains(&slot.layer) {
            bake.push(i as u32);
        }
    }
    state.baking_layers = bake.iter().map(|&i| frame.slots[i as usize].layer).collect();
    state.bake_count = bake.len() as u32;

    let s = &frame.settings;
    let g = gpu.globals.get_mut();
    let slot_count = frame.slots.len().min(MAX_SLOTS);
    for (dst, src) in g.slots.iter_mut().zip(frame.slots.iter()) {
        let mut slot = GpuSlot {
            grid_min: src.grid_min.extend(src.voxel_size),
            bounds_min: src.bounds_min.extend(src.layer as f32),
            bounds_max: src.bounds_max.extend(src.dissolve),
            params: Vec4::new(src.growth, src.density, src.dissolve * 0.6, src.seed),
            tint: src.tint.extend(1.0),
            holes: UVec4::new(src.hole_start, src.hole_count, 0, 0),
            drawn_min: src.drawn_min.extend(0.0),
            drawn_max: src.drawn_max.extend(0.0),
            ..default()
        };
        if let Some(f) = src.facet
            && (f.puff_offset + f.puff_count) as usize <= puff_capacity
        {
            slot.holes.z = STYLE_FACETED;
            slot.facet_grid = f.bins_min.extend(f.cell);
            slot.facet_dims = f.dims.extend(src.layer * limits.max_cells as u32);
            slot.facet_ofs = UVec4::new(src.layer * limits.max_indices as u32, f.puff_offset, f.puff_count, 0);
            slot.facet_shape = Vec4::new(f.blend, f.region_pad, 0.0, 0.0);
        }
        *dst = slot;
    }
    for (dst, src) in g.holes.iter_mut().zip(frame.holes.iter()) {
        *dst = GpuHole {
            a: src.a.extend(src.radius),
            b: src.b.extend(src.strength),
        };
    }
    let light_count = frame.lights.len().min(MAX_LIGHTS);
    for (dst, src) in g.lights.iter_mut().zip(frame.lights.iter()) {
        *dst = GpuLight {
            position_range: src.position.extend(src.range),
            color: src.color.extend(0.0),
        };
    }
    g.bake_slots = [UVec4::ZERO; MAX_SLOTS / 4];
    for (i, slot) in bake.iter().enumerate() {
        g.bake_slots[i / 4][i % 4] = *slot;
    }
    g.grid = config.grid_dims.extend(config.max_smokes);
    g.counts = UVec4::new(slot_count as u32, light_count as u32, state.bake_count, frame.frame);
    g.sun_direction = frame
        .sun_direction
        .normalize_or(Vec3::Y)
        .extend(if frame.has_sun { 1.0 } else { 0.0 });
    g.sun_color = frame.sun_color.extend(0.0);
    g.ambient = frame.ambient.extend(0.0);
    g.medium = s.albedo.to_vec3().extend(s.extinction);
    g.noise = Vec4::new(s.noise_frequency, s.erosion, s.detail_erosion, frame.time);
    g.wind = s.wind.extend(s.anisotropy.clamp(-0.95, 0.95));
    g.lighting = Vec4::new(
        s.multi_scattering,
        s.indoor_ambient.clamp(0.0, 1.0),
        s.ambient_occlusion,
        config.voxel_size,
    );
    // The rule blocks a line through `sight_threshold` of cloud. Capped, the
    // march draws a line opaque from ln(100) / extinction of it and the noise
    // may take the difference from any line; filled, it draws it opaque from
    // ln(20) / extinction (`OPAQUE_BELOW` in smoke_march.wesl) and fills what
    // the noise took back in between the two (on a line the rule blocks, all
    // of it).
    let extinction = s.extinction.max(1e-6);
    let threshold = frame.sight_threshold.max(0.02);
    g.fairness = Vec4::new(
        (frame.sight_threshold - 100f32.ln() / extinction).max(0.0),
        (20f32.ln() / extinction).min(threshold - 0.01),
        threshold,
        0.0,
    );
    gpu.globals.write_buffer(&device, &queue);
}

fn cached_texture(
    cache: &mut TextureCache,
    device: &RenderDevice,
    label: &'static str,
    size: UVec2,
    format: TextureFormat,
) -> CachedTexture {
    cache.get(
        device,
        TextureDescriptor {
            label: Some(label),
            size: Extent3d {
                width: size.x.max(1),
                height: size.y.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format,
            usage: TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
    )
}

/// Groups the screen rectangles (`None` for one out of view, left out) that
/// come within `gap` pixels of each other, directly or through others, and
/// merges groups until no two groups' bounding rectangles come that close:
/// a march covers its group's bounding rectangle, so it may not reach into
/// another group's. Each group lists its members as indices into `rects`.
fn march_groups(rects: &[Option<(Vec2, Vec2)>], gap: f32) -> Vec<Vec<usize>> {
    let near = |a: (Vec2, Vec2), b: (Vec2, Vec2)| {
        a.0.x < b.1.x + gap && b.0.x < a.1.x + gap && a.0.y < b.1.y + gap && b.0.y < a.1.y + gap
    };
    let mut groups: Vec<(Vec<usize>, (Vec2, Vec2))> = rects
        .iter()
        .enumerate()
        .filter_map(|(i, rect)| rect.map(|rect| (vec![i], rect)))
        .collect();
    'merge: loop {
        for i in 0..groups.len() {
            for j in i + 1..groups.len() {
                if near(groups[i].1, groups[j].1) {
                    let (members, rect) = groups.remove(j);
                    let (into, bound) = &mut groups[i];
                    into.extend(members);
                    *bound = (bound.0.min(rect.0), bound.1.max(rect.1));
                    continue 'merge;
                }
            }
        }
        break;
    }
    groups
        .into_iter()
        .map(|(mut members, _)| {
            members.sort_unstable();
            members
        })
        .collect()
}

/// The most of these rectangles that share a point. Rectangles that overlap
/// pairwise share the corner of their overlap at the largest minimum on each
/// axis, so counting at every pair of one's x minimum and another's y
/// minimum finds it.
fn most_overlapping(rects: &[(Vec2, Vec2)]) -> usize {
    rects
        .iter()
        .flat_map(|a| rects.iter().map(move |b| Vec2::new(a.0.x, b.0.y)))
        .map(|p| {
            rects
                .iter()
                .filter(|r| r.0.cmple(p).all() && p.cmple(r.1).all())
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// Whether a point light can light anything in a box: the march adds a
/// light's share only within its range (here with a centimetre to spare).
fn reaches(light: &FrameLight, min: Vec3, max: Vec3) -> bool {
    let range = light.range + 0.01;
    light.position.clamp(min, max).distance_squared(light.position) < range * range
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn prepare_smoke_views(
    mut commands: Commands,
    views: Query<(
        Entity,
        &ExtractedView,
        &ExtractedCamera,
        &SmokeCamera,
        Option<&SmokeQuality>,
        &Msaa,
    )>,
    frame: Res<SmokeFrame>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut texture_cache: ResMut<TextureCache>,
    mut uniforms: ResMut<SmokeViewUniforms>,
    pipelines: Option<Res<SmokePipelines>>,
    specializer: Option<Res<OverlaySpecializer>>,
    mut specialized: ResMut<SpecializedRenderPipelines<OverlaySpecializer>>,
    march_specializer: Option<Res<MarchSpecializer>>,
    mut march_specialized: ResMut<SpecializedComputePipelines<MarchSpecializer>>,
    pipeline_cache: Res<PipelineCache>,
    mut work: ResMut<FacetWork>,
) {
    uniforms.0.clear();
    let mut work_needed = 0_u64;
    for (entity, view, camera, settings, quality, msaa) in &views {
        let prepared = (|| {
            let pipelines = pipelines.as_ref()?;
            let specializer = specializer.as_ref()?;
            let msaa_on = msaa.samples() > 1;
            // The march kernels for this camera's noise mode and MSAA, queued
            // on the first frame (later calls look them up), by
            // `[kernel][lights]`.
            let rule = frame.settings.noise_within_rule;
            let kernel = |march_specialized: &mut SpecializedComputePipelines<MarchSpecializer>, kernel, lights| {
                let spec = march_specializer.as_deref()?;
                let key = MarchKey {
                    kernel,
                    rule,
                    msaa: msaa_on,
                    lights,
                };
                Some(march_specialized.specialize(&pipeline_cache, spec, key))
            };
            let kernels = [MarchKernel::Single, MarchKernel::Overlap8]
                .map(|k| [false, true].map(|lights| kernel(&mut march_specialized, k, lights)));
            if frame.slots.is_empty() {
                return None;
            }
            // Orthographic cameras are not supported.
            if view.clip_from_view.w_axis.w == 1.0 {
                return None;
            }
            // Each overlay for both of its targets (`OverlayPipelines`).
            let overlay = |specialized: &mut SpecializedRenderPipelines<OverlaySpecializer>, kind| {
                [msaa.samples(), 1].map(|samples| {
                    specialized.specialize(
                        &pipeline_cache,
                        specializer,
                        OverlayKey {
                            kind,
                            msaa: msaa_on,
                            samples,
                            hdr: camera.hdr,
                            format: view.target_format,
                        },
                    )
                })
            };
            let tier = quality.map_or_else(|| crate::quality::tier_from_quality(settings.quality), |q| q.tier);
            let vol_q = settings.volumetric_quality(tier);
            let facet_q = settings.facet_quality(tier);

            let viewport = view.viewport;
            let size = viewport.zw().max(UVec2::ONE);
            let world_from_view = view.world_from_view.to_matrix();
            let clip_from_world = view
                .clip_from_world
                .unwrap_or_else(|| view.clip_from_view * world_from_view.inverse());
            // From here on everything is in the core's internal frame (metres,
            // Y up), which is where the smoke data lives.
            let internal_from_world = frame.internal_from_world;
            let clip_from_internal = clip_from_world * internal_from_world.inverse();
            let camera_pos = internal_from_world.transform_point3(view.world_from_view.translation());
            let near = view.clip_from_view.w_axis.z * internal_from_world.x_axis.truncate().length();
            let rect_of = |faceted: bool| {
                screen_rect(
                    clip_from_internal,
                    camera_pos,
                    near,
                    size.as_vec2(),
                    frame
                        .slots
                        .iter()
                        .filter(|s| s.facet.is_some() == faceted)
                        .map(|s| (s.drawn_min, s.drawn_max)),
                )
            };
            let vol_rect = rect_of(false);
            let facet_rect = rect_of(true);
            if vol_rect.is_none() && facet_rect.is_none() {
                return None;
            }
            let scissor = |(lo, hi): (Vec2, Vec2)| {
                let (lo, hi) = (lo.as_uvec2(), hi.as_uvec2());
                UVec4::new(viewport.x + lo.x, viewport.y + lo.y, hi.x - lo.x, hi.y - lo.y)
            };

            // Volumetric.
            // The step's resolution, or coarser close up (picked with the
            // tier: `quality::march_resolution`).
            let vol_divisor = quality
                .map_or(vol_q.resolution, |q| q.volumetric_resolution)
                .divisor()
                .max(vol_q.resolution.divisor());
            let smoke_size = (size.as_vec2() / vol_divisor).ceil().as_uvec2().max(UVec2::ONE);
            // March a one-texel margin around a composite area so the bilinear
            // footprint at the edges is always freshly written.
            let march_rect = |rect: (Vec2, Vec2)| {
                let (lo, hi) = (rect.0.as_uvec2(), rect.1.as_uvec2());
                let march_min = (lo.as_vec2() / vol_divisor)
                    .floor()
                    .as_uvec2()
                    .saturating_sub(UVec2::ONE);
                let march_max = ((hi.as_vec2() / vol_divisor).ceil().as_uvec2() + UVec2::ONE).min(smoke_size);
                UVec4::new(march_min.x, march_min.y, march_max.x, march_max.y)
            };
            // One march per group of smokes whose screen rectangles come within
            // three smoke-buffer texels and two pixels of each other, directly
            // or through others (`march_groups`), each over its own rectangle:
            // no march, margin included, reaches into another group's
            // rectangle or writes another's texels, and no ray in it meets
            // another group's smoke. A smoke on its own gets the single-smoke
            // kernel, with its slot in a uniform: a ray takes the march's
            // one-smoke path either way, so the picture is the same, and the
            // kernel needs fewer registers. A group adds up the smokes a ray
            // crosses, with room for 8 of them wherever no point of the screen
            // lies in more than 8 of the group's rectangles (a ray meets only
            // smokes whose rectangle holds its pixel); its short arrays made
            // two overlapping smokes 32-39 % cheaper close up at the top four
            // dial steps on an Apple M4. A kernel no point light reaches is
            // compiled without the light code. Past 8 smokes on a pixel, and
            // until a kernel has compiled, a group falls back on the march
            // with room for every slot, which draws the same picture.
            let volumetric_slots: Vec<_> = frame
                .slots
                .iter()
                .enumerate()
                .take(MAX_SLOTS)
                .filter(|(_, s)| s.facet.is_none())
                .collect();
            let bounds: Vec<_> = volumetric_slots
                .iter()
                .map(|(_, s)| {
                    screen_rect(
                        clip_from_internal,
                        camera_pos,
                        near,
                        size.as_vec2(),
                        [(s.bounds_min, s.bounds_max)],
                    )
                })
                .collect();
            let lights = &frame.lights[..frame.lights.len().min(MAX_LIGHTS)];
            let (marches, scissors): (Vec<_>, Vec<_>) = march_groups(&bounds, 3.0 * vol_divisor + 2.0)
                .into_iter()
                .filter_map(|members| {
                    let smokes = || members.iter().map(|&i| volumetric_slots[i].1);
                    let rect = screen_rect(
                        clip_from_internal,
                        camera_pos,
                        near,
                        size.as_vec2(),
                        smokes().map(|s| (s.drawn_min, s.drawn_max)),
                    )?;
                    let lit = lights
                        .iter()
                        .any(|l| smokes().any(|s| reaches(l, s.bounds_min, s.bounds_max)));
                    let (kernel, slot) = match members[..] {
                        [i] => (Some(MarchKernel::Single), Some(volumetric_slots[i].0 as u32)),
                        _ => {
                            let rects: Vec<_> = members.iter().filter_map(|&i| bounds[i]).collect();
                            let fits = most_overlapping(&rects) <= OVERLAP8_SMOKES;
                            (fits.then_some(MarchKernel::Overlap8), None)
                        }
                    };
                    let pipeline = kernel
                        .and_then(|k| kernels[k as usize][lit as usize])
                        .filter(|&id| pipeline_cache.get_compute_pipeline(id).is_some())
                        .unwrap_or(pipelines.march[rule as usize][msaa_on as usize]);
                    let march = MarchDispatch {
                        pipeline,
                        rect: march_rect(rect),
                        slot,
                        uniform_offset: 0,
                    };
                    Some((march, scissor(rect)))
                })
                .unzip();
            let union_rect = vol_rect.map_or(UVec4::ZERO, march_rect);
            let mut volumetric = vol_rect.map(|_| VolumetricView {
                texture: cached_texture(
                    &mut texture_cache,
                    &device,
                    "smoke_buffer",
                    smoke_size,
                    TextureFormat::Rgba16Float,
                ),
                marches,
                composite: overlay(&mut specialized, OverlayKind::Composite),
                scissors,
            });

            // Faceted.
            // Angle of one pixel: 2 tan(fov/2) / height.
            let pixel_angle = 2.0 / (view.clip_from_view.y_axis.y * size.y as f32);
            let puff_px = frame
                .slots
                .iter()
                .filter_map(|s| s.facet.map(|f| (s, f)))
                .filter(|(s, _)| {
                    screen_rect(
                        clip_from_internal,
                        camera_pos,
                        near,
                        size.as_vec2(),
                        [(s.bounds_min, s.bounds_max)],
                    )
                    .is_some()
                })
                .map(|(s, f)| {
                    let nearest = camera_pos.clamp(s.bounds_min, s.bounds_max);
                    f.puff_size / (camera_pos.distance(nearest).max(near) * pixel_angle)
                })
                .fold(0.0, f32::max);
            let mut facet_divisor = facet_q.divisor.clamp(1, 8);
            while facet_divisor > 1 && puff_px < MIN_PUFF_PIXELS * facet_divisor as f32 {
                facet_divisor /= 2;
            }
            let mut coarse_rect = UVec4::ZERO;
            let mut facet_px = UVec4::ZERO;
            let facets = facet_rect.map(|rect| {
                let (lo, hi) = (rect.0.as_uvec2(), rect.1.as_uvec2());
                facet_px = UVec4::new(lo.x, lo.y, hi.x, hi.y);
                let passes = if facet_divisor == 1 {
                    FacetPasses::Full {
                        trace: overlay(&mut specialized, OverlayKind::FacetTrace),
                    }
                } else {
                    let coarse_size = (size + facet_divisor - 1) / facet_divisor;
                    // One texel of margin: every pixel reads its 2×2 neighbours.
                    let cmin = (lo / facet_divisor).saturating_sub(UVec2::ONE);
                    let cmax = ((hi + facet_divisor - 1) / facet_divisor + 1).min(coarse_size);
                    coarse_rect = UVec4::new(cmin.x, cmin.y, cmax.x, cmax.y);
                    work_needed = work_needed.max(size.x as u64 * size.y as u64);
                    let m = msaa_on as usize;
                    FacetPasses::Reduced {
                        coarse_texture: cached_texture(
                            &mut texture_cache,
                            &device,
                            "smoke_facet_coarse",
                            coarse_size,
                            TextureFormat::Rgba32Uint,
                        ),
                        layer: cached_texture(
                            &mut texture_cache,
                            &device,
                            "smoke_facet_layer",
                            size,
                            TextureFormat::Rgba16Float,
                        ),
                        coarse: pipelines.facet_coarse[m],
                        classify: pipelines.facet_classify[m],
                        retrace: pipelines.facet_retrace[m],
                        composite: overlay(&mut specialized, OverlayKind::FacetComposite),
                        coarse_dispatch: (cmax - cmin + MARCH_WORKGROUP - 1) / MARCH_WORKGROUP,
                        classify_dispatch: (hi - lo + MARCH_WORKGROUP - 1) / MARCH_WORKGROUP,
                    }
                };
                FacetView {
                    passes,
                    scissor: scissor(rect),
                }
            });

            let mut flags = 0;
            if facet_q.shadows == FacetShadows::Traced {
                flags |= FLAG_TRACED_SHADOWS;
            }
            if facet_q.occlusion == FacetOcclusion::Traced {
                flags |= FLAG_TRACED_OCCLUSION;
            }
            if frame.settings.debug_silhouette {
                flags |= FLAG_DEBUG_SILHOUETTE;
            }
            if frame.settings.debug_transmittance {
                flags |= FLAG_DEBUG_TRANSMITTANCE;
            }
            let view_uniforms = GpuView {
                world_from_clip: internal_from_world * world_from_view * view.clip_from_view.inverse(),
                camera_position: camera_pos.extend(near),
                target_size: Vec4::new(size.x as f32, size.y as f32, smoke_size.x as f32, smoke_size.y as f32),
                viewport: Vec4::new(viewport.x as f32, viewport.y as f32, 0.0, 0.0),
                march: Vec4::new(
                    camera.exposure,
                    vol_q.step_size.max(0.01),
                    vol_q.step_growth.max(0.0),
                    vol_divisor,
                ),
                rect: union_rect,
                params: UVec4::new(
                    vol_q.max_steps.max(1),
                    if settings.temporal_jitter { frame.frame } else { 0 },
                    0,
                    0,
                ),
                facet: UVec4::new(facet_divisor, facet_q.max_steps.max(1), facet_q.shadow_steps, flags),
                facet_f: Vec4::new(
                    facet_q.tolerance_px.max(0.05),
                    pixel_angle,
                    facet_q.shading_tolerance.max(0.0),
                    0.0,
                ),
                facet_rect: coarse_rect,
                facet_px,
            };
            let uniform_offset = uniforms.0.push(&view_uniforms);
            // Each march reads its own rectangle, and a single-smoke march its
            // slot.
            for march in volumetric.iter_mut().flat_map(|v| &mut v.marches) {
                march.uniform_offset = uniforms.0.push(&GpuView {
                    rect: march.rect,
                    params: UVec4 {
                        z: march.slot.unwrap_or(0),
                        ..view_uniforms.params
                    },
                    ..view_uniforms
                });
            }

            Some(ViewSmoke {
                uniform_offset,
                msaa: msaa_on,
                volumetric,
                facets,
            })
        })();

        match prepared {
            Some(view_smoke) => {
                commands.entity(entity).insert(view_smoke);
            }
            None => {
                commands.entity(entity).remove::<ViewSmoke>();
            }
        }
    }
    uniforms.0.write_buffer(&device, &queue);

    // Room for every pixel of the largest viewport that queues work.
    if work_needed > work.capacity || work.buffer.is_none() && work_needed > 0 {
        let capacity = work_needed.max(work.capacity);
        work.buffer = Some(device.create_buffer(&BufferDescriptor {
            label: Some("smoke_facet_work"),
            size: FACET_WORK_HEADER + capacity * 4,
            usage: BufferUsages::STORAGE | BufferUsages::INDIRECT | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        work.capacity = capacity;
    }
}

/// Re-bake density and lighting for the slots that changed this frame.
fn bake_smoke(
    mut ctx: RenderContext,
    gpu: Option<Res<SmokeGpu>>,
    pipelines: Option<Res<SmokePipelines>>,
    pipeline_cache: Res<PipelineCache>,
    config: Res<SmokeConfig>,
    mut state: ResMut<SmokeRenderState>,
) {
    if state.bake_count == 0 {
        return;
    }
    let (Some(gpu), Some(pipelines)) = (gpu, pipelines) else {
        return;
    };
    let (Some(density_pipeline), Some(light_pipeline), Some(globals)) = (
        pipeline_cache.get_compute_pipeline(pipelines.bake_density),
        pipeline_cache.get_compute_pipeline(pipelines.bake_light),
        gpu.globals.binding(),
    ) else {
        // Shaders still compiling; the slots stay pending.
        return;
    };

    let device = ctx.render_device().clone();
    let density_group = device.create_bind_group(
        "smoke_bake_density",
        &pipeline_cache.get_bind_group_layout(&pipelines.bake_density_layout),
        &BindGroupEntries::with_indices(((0, globals.clone()), (1, &gpu.source_view), (2, &gpu.density_view))),
    );
    let light_group = device.create_bind_group(
        "smoke_bake_light",
        &pipeline_cache.get_bind_group_layout(&pipelines.bake_light_layout),
        &BindGroupEntries::with_indices((
            (0, globals),
            (3, &gpu.density_view),
            (4, &gpu.linear_clamp),
            (5, &gpu.lit_view),
            (6, &gpu.open_view),
        )),
    );

    let dims = config.grid_dims;
    let groups = UVec3::new(
        dims.x / BAKE_WORKGROUP,
        dims.y / BAKE_WORKGROUP,
        dims.z / BAKE_WORKGROUP * state.bake_count,
    );

    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let span = diagnostics.time_span(ctx.command_encoder(), "smoke_bake");
    {
        let mut pass = ctx.command_encoder().begin_compute_pass(&ComputePassDescriptor {
            label: Some("smoke_bake_density"),
            timestamp_writes: None,
        });
        pass.set_pipeline(density_pipeline);
        pass.set_bind_group(0, &density_group, &[]);
        pass.dispatch_workgroups(groups.x, groups.y, groups.z);
    }
    {
        let mut pass = ctx.command_encoder().begin_compute_pass(&ComputePassDescriptor {
            label: Some("smoke_bake_lighting"),
            timestamp_writes: None,
        });
        pass.set_pipeline(light_pipeline);
        pass.set_bind_group(0, &light_group, &[]);
        pass.dispatch_workgroups(groups.x, groups.y, groups.z);
    }
    span.end(ctx.command_encoder());

    let state = state.as_mut();
    for layer in &state.baking_layers {
        state.pending_bake.remove(layer);
    }
    state.bake_count = 0;
}

/// A render pass that blends onto the main texture: the multisampled one
/// (resolved at the end) when MSAA is on, so the transparent pass that
/// follows continues from it, or the resolved image (`resolved`) when nothing
/// transparent follows.
macro_rules! overlay_pass {
    ($ctx:expr, $target:expr, $resolved:expr, $label:expr) => {
        $ctx.command_encoder().begin_render_pass(&RenderPassDescriptor {
            label: Some($label),
            color_attachments: &[Some(if $resolved {
                $target.get_unsampled_color_attachment()
            } else {
                $target.get_color_attachment()
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    };
}

/// Faceted, then volumetric passes for one camera.
#[allow(clippy::too_many_arguments)]
fn smoke_pass(
    view: ViewQuery<(&ViewTarget, &ViewDepthStencilTexture, &ViewSmoke, &ExtractedView)>,
    transparent_phases: Res<ViewSortedRenderPhases<Transparent3d>>,
    gpu: Option<Res<SmokeGpu>>,
    pipelines: Option<Res<SmokePipelines>>,
    specializer: Option<Res<OverlaySpecializer>>,
    view_uniforms: Res<SmokeViewUniforms>,
    pipeline_cache: Res<PipelineCache>,
    work: Res<FacetWork>,
    mut ctx: RenderContext,
) {
    let (Some(gpu), Some(pipelines), Some(specializer)) = (gpu, pipelines, specializer) else {
        return;
    };
    let (target, depth, smoke, extracted_view) = view.into_inner();
    let (Some(globals), Some(view_binding)) = (gpu.globals.binding(), view_uniforms.0.binding()) else {
        return;
    };
    let Some(depth_view) = depth.attachment.depth_stencil_views().depth_only_view() else {
        return;
    };
    let device = ctx.render_device().clone();
    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let msaa = smoke.msaa as usize;
    let offsets = [smoke.uniform_offset];
    // With MSAA, the overlays draw into the resolved image when nothing
    // transparent draws after them this frame: Bevy runs its transparent pass
    // only when something is queued in it, and nothing else reads the
    // multisampled target again. That skips loading, storing and resolving it
    // (about 0.2 ms a frame at 1080p on an Apple M4, whatever the smoke's
    // size). Otherwise, and until the resolved pipelines have compiled, they
    // draw into the multisampled target as the transparent pass needs.
    let resolved_ready = |p: &OverlayPipelines| pipeline_cache.get_render_pipeline(p[1]).is_some();
    let resolved = smoke.msaa
        && transparent_phases
            .get(&extracted_view.retained_view_entity)
            .is_none_or(|phase| phase.items.is_empty())
        && smoke.facets.as_ref().is_none_or(|f| resolved_ready(f.passes.overlay()))
        && smoke.volumetric.as_ref().is_none_or(|v| resolved_ready(&v.composite));
    let to = resolved as usize;

    // Faceted: opaque surfaces, drawn first like geometry.
    if let Some(facets) = &smoke.facets {
        let facet_entries = || {
            (
                globals.clone(),
                view_binding.clone(),
                depth_view,
                &gpu.source_view,
                &gpu.lit_view,
                &gpu.linear_clamp,
                gpu.facet_puffs.as_entire_binding(),
                gpu.facet_cells.as_entire_binding(),
                gpu.facet_indices.as_entire_binding(),
            )
        };
        let s = facets.scissor;
        match &facets.passes {
            FacetPasses::Full { trace } => {
                if let Some(trace_pipeline) = pipeline_cache.get_render_pipeline(trace[to]) {
                    let group = device.create_bind_group(
                        "smoke_facet_trace",
                        &pipeline_cache.get_bind_group_layout(&specializer.trace_layouts[msaa]),
                        &BindGroupEntries::sequential(facet_entries()),
                    );
                    let span = diagnostics.time_span(ctx.command_encoder(), "smoke_facet_trace");
                    {
                        let mut pass = overlay_pass!(ctx, target, resolved, "smoke_facet_trace");
                        pass.set_scissor_rect(s.x, s.y, s.z.max(1), s.w.max(1));
                        pass.set_pipeline(trace_pipeline);
                        pass.set_bind_group(0, &group, &offsets);
                        pass.draw(0..3, 0..1);
                    }
                    span.end(ctx.command_encoder());
                }
            }
            FacetPasses::Reduced {
                coarse_texture,
                layer,
                coarse,
                classify,
                retrace,
                composite,
                coarse_dispatch,
                classify_dispatch,
            } => {
                if let (
                    Some(coarse_pipeline),
                    Some(classify_pipeline),
                    Some(retrace_pipeline),
                    Some(composite_pipeline),
                    Some(work_buffer),
                ) = (
                    pipeline_cache.get_compute_pipeline(*coarse),
                    pipeline_cache.get_compute_pipeline(*classify),
                    pipeline_cache.get_compute_pipeline(*retrace),
                    pipeline_cache.get_render_pipeline(composite[to]),
                    work.buffer.as_ref(),
                ) {
                    fn with_extra<'a>(
                        mut entries: Vec<BindGroupEntry<'a>>,
                        extra: &[(u32, BindingResource<'a>)],
                    ) -> Vec<BindGroupEntry<'a>> {
                        entries.extend(extra.iter().map(|(binding, resource)| BindGroupEntry {
                            binding: *binding,
                            resource: resource.clone(),
                        }));
                        entries
                    }
                    let coarse_group = device.create_bind_group(
                        "smoke_facet_coarse",
                        &pipeline_cache.get_bind_group_layout(&pipelines.facet_coarse_layouts[msaa]),
                        &with_extra(
                            BindGroupEntries::sequential(facet_entries()).to_vec(),
                            &[(9, coarse_texture.default_view.into_binding())],
                        ),
                    );
                    let refine = [
                        (9, coarse_texture.default_view.into_binding()),
                        (10, layer.default_view.into_binding()),
                        (11, work_buffer.as_entire_binding()),
                    ];
                    let classify_group = device.create_bind_group(
                        "smoke_facet_classify",
                        &pipeline_cache.get_bind_group_layout(&pipelines.facet_classify_layouts[msaa]),
                        &with_extra(BindGroupEntries::sequential(facet_entries()).to_vec(), &refine),
                    );
                    let retrace_group = device.create_bind_group(
                        "smoke_facet_retrace",
                        &pipeline_cache.get_bind_group_layout(&pipelines.facet_retrace_layouts[msaa]),
                        &with_extra(BindGroupEntries::sequential(facet_entries()).to_vec(), &refine),
                    );
                    let composite_group = device.create_bind_group(
                        "smoke_facet_composite",
                        &pipeline_cache.get_bind_group_layout(&specializer.facet_composite_layout),
                        &BindGroupEntries::sequential((view_binding.clone(), &layer.default_view)),
                    );

                    let span = diagnostics.time_span(ctx.command_encoder(), "smoke_facet_coarse");
                    {
                        let mut pass = ctx.command_encoder().begin_compute_pass(&ComputePassDescriptor {
                            label: Some("smoke_facet_coarse"),
                            timestamp_writes: None,
                        });
                        pass.set_pipeline(coarse_pipeline);
                        pass.set_bind_group(0, &coarse_group, &offsets);
                        pass.dispatch_workgroups(coarse_dispatch.x.max(1), coarse_dispatch.y.max(1), 1);
                    }
                    span.end(ctx.command_encoder());

                    let span = diagnostics.time_span(ctx.command_encoder(), "smoke_facet_classify");
                    ctx.command_encoder()
                        .clear_buffer(work_buffer, 0, Some(FACET_WORK_HEADER));
                    {
                        let mut pass = ctx.command_encoder().begin_compute_pass(&ComputePassDescriptor {
                            label: Some("smoke_facet_classify"),
                            timestamp_writes: None,
                        });
                        pass.set_pipeline(classify_pipeline);
                        pass.set_bind_group(0, &classify_group, &offsets);
                        pass.dispatch_workgroups(classify_dispatch.x.max(1), classify_dispatch.y.max(1), 1);
                    }
                    span.end(ctx.command_encoder());

                    let span = diagnostics.time_span(ctx.command_encoder(), "smoke_facet_retrace");
                    {
                        let mut pass = ctx.command_encoder().begin_compute_pass(&ComputePassDescriptor {
                            label: Some("smoke_facet_retrace"),
                            timestamp_writes: None,
                        });
                        pass.set_pipeline(retrace_pipeline);
                        pass.set_bind_group(0, &retrace_group, &offsets);
                        pass.dispatch_workgroups_indirect(work_buffer, 0);
                    }
                    span.end(ctx.command_encoder());

                    let span = diagnostics.time_span(ctx.command_encoder(), "smoke_facet_composite");
                    {
                        let mut pass = overlay_pass!(ctx, target, resolved, "smoke_facet_composite");
                        pass.set_scissor_rect(s.x, s.y, s.z.max(1), s.w.max(1));
                        pass.set_pipeline(composite_pipeline);
                        pass.set_bind_group(0, &composite_group, &offsets);
                        pass.draw(0..3, 0..1);
                    }
                    span.end(ctx.command_encoder());
                }
            }
        }
    }

    // Volumetric: raymarch at reduced resolution, then composite.
    if let Some(vol) = &smoke.volumetric
        && let Some(march_pipelines) = vol
            .marches
            .iter()
            .map(|m| pipeline_cache.get_compute_pipeline(m.pipeline))
            .collect::<Option<Vec<_>>>()
        && let Some(composite_pipeline) = pipeline_cache.get_render_pipeline(vol.composite[to])
    {
        let march_group = device.create_bind_group(
            "smoke_march",
            &pipeline_cache.get_bind_group_layout(&pipelines.march_layouts[msaa]),
            &BindGroupEntries::sequential((
                globals.clone(),
                view_binding.clone(),
                depth_view,
                &gpu.lit_view,
                &gpu.linear_clamp,
                &gpu.noise_view,
                &gpu.linear_repeat,
                &vol.texture.default_view,
                &gpu.open_view,
            )),
        );
        let composite_group = device.create_bind_group(
            "smoke_composite",
            &pipeline_cache.get_bind_group_layout(&specializer.composite_layouts[msaa]),
            &BindGroupEntries::sequential((view_binding.clone(), depth_view, &vol.texture.default_view)),
        );

        let span = diagnostics.time_span(ctx.command_encoder(), "smoke_march");
        {
            let mut pass = ctx.command_encoder().begin_compute_pass(&ComputePassDescriptor {
                label: Some("smoke_march"),
                timestamp_writes: None,
            });
            for (march, pipeline) in vol.marches.iter().zip(march_pipelines) {
                let r = march.rect;
                let dispatch = (UVec2::new(r.z, r.w) - UVec2::new(r.x, r.y) + MARCH_WORKGROUP - 1) / MARCH_WORKGROUP;
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &march_group, &[march.uniform_offset]);
                pass.dispatch_workgroups(dispatch.x.max(1), dispatch.y.max(1), 1);
            }
        }
        span.end(ctx.command_encoder());

        let span = diagnostics.time_span(ctx.command_encoder(), "smoke_composite");
        {
            let mut pass = overlay_pass!(ctx, target, resolved, "smoke_composite");
            pass.set_pipeline(composite_pipeline);
            pass.set_bind_group(0, &composite_group, &offsets);
            for s in &vol.scissors {
                pass.set_scissor_rect(s.x, s.y, s.z.max(1), s.w.max(1));
                pass.draw(0..3, 0..1);
            }
        }
        span.end(ctx.command_encoder());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(r: [f32; 4]) -> (Vec2, Vec2) {
        (Vec2::new(r[0], r[1]), Vec2::new(r[2], r[3]))
    }

    /// `march_groups` for rectangles given as [x0, y0, x1, y1].
    fn groups_of(rects: &[[f32; 4]], gap: f32) -> Vec<Vec<usize>> {
        let rects: Vec<_> = rects.iter().map(|&r| Some(rect(r))).collect();
        march_groups(&rects, gap)
    }

    #[test]
    fn march_groups_need_the_gap_on_one_axis() {
        let a = [0.0, 0.0, 100.0, 100.0];
        // Side by side: apart only once the gap between them reaches `gap`.
        assert_eq!(groups_of(&[a, [110.0, 0.0, 200.0, 100.0]], 10.0), [vec![0], vec![1]]);
        assert_eq!(groups_of(&[a, [109.0, 0.0, 200.0, 100.0]], 10.0), [vec![0, 1]]);
        // Overlapping, or one inside the other.
        assert_eq!(groups_of(&[a, [50.0, 50.0, 150.0, 150.0]], 0.0), [vec![0, 1]]);
        assert_eq!(groups_of(&[a, [40.0, 40.0, 60.0, 60.0]], 0.0), [vec![0, 1]]);
        // Diagonal neighbours are apart if either axis has the gap.
        assert_eq!(groups_of(&[a, [105.0, 120.0, 200.0, 200.0]], 10.0), [vec![0], vec![1]]);
        // One rectangle, or none.
        assert_eq!(groups_of(&[[0.0, 0.0, 1920.0, 1080.0]], 10.0), [vec![0]]);
        assert!(groups_of(&[], 10.0).is_empty());
        // A close pair goes together; the rest stay on their own.
        assert_eq!(
            groups_of(&[a, [500.0, 0.0, 600.0, 100.0], [595.0, 50.0, 700.0, 150.0]], 10.0),
            [vec![0], vec![1, 2]]
        );
        // Through a third, rectangles far apart join.
        assert_eq!(
            groups_of(&[a, [500.0, 0.0, 600.0, 100.0], [95.0, 50.0, 505.0, 60.0]], 10.0),
            [vec![0, 1, 2]]
        );
        // So does one near none of a group's members but inside the group's
        // bounding rectangle, which the group's march covers.
        assert_eq!(
            groups_of(&[a, [100.0, 100.0, 200.0, 200.0], [150.0, 0.0, 200.0, 40.0]], 10.0),
            [vec![0, 1, 2]]
        );
        // Out of view: left out.
        let b = Some(rect([0.0, 200.0, 100.0, 300.0]));
        assert_eq!(march_groups(&[Some(rect(a)), None, b], 10.0), [vec![0], vec![2]]);
    }

    #[test]
    fn most_overlapping_counts_rectangles_that_share_a_point() {
        assert_eq!(most_overlapping(&[]), 0);
        assert_eq!(most_overlapping(&[rect([0.0, 0.0, 10.0, 10.0])]), 1);
        // A chain: each overlaps the next, but no point lies in all three.
        let chain = [[0.0, 0.0, 10.0, 10.0], [8.0, 0.0, 18.0, 10.0], [16.0, 0.0, 26.0, 10.0]].map(rect);
        assert_eq!(most_overlapping(&chain), 2);
        // Rectangles that overlap pairwise share a point.
        let star = [[0.0, 0.0, 10.0, 10.0], [5.0, -5.0, 15.0, 5.0], [-5.0, 3.0, 7.0, 20.0]].map(rect);
        assert_eq!(most_overlapping(&star), 3);
        // Edges count: a ray through the shared edge may meet both.
        let edge = [[0.0, 0.0, 10.0, 10.0], [10.0, 0.0, 20.0, 10.0]].map(rect);
        assert_eq!(most_overlapping(&edge), 2);
    }

    #[test]
    fn reaches_within_range_of_the_box() {
        let light = |x: f32, range: f32| FrameLight {
            position: Vec3::new(x, 0.5, 0.5),
            range,
            color: Vec3::ONE,
        };
        let (min, max) = (Vec3::ZERO, Vec3::ONE);
        // Inside the box, or near enough to its face.
        assert!(reaches(&light(0.5, 0.1), min, max));
        assert!(reaches(&light(3.0, 2.5), min, max));
        // Farther from the face than the range (and the centimetre spare).
        assert!(!reaches(&light(3.0, 1.95), min, max));
        assert!(!reaches(&light(-2.0, 1.95), min, max));
    }

    #[test]
    fn limits_match_the_shaders() {
        let common = include_str!("shaders/smoke_common.wesl");
        for line in [
            format!("const MAX_SLOTS: u32 = {MAX_SLOTS}u;"),
            format!("const MAX_HOLES: u32 = {MAX_HOLES}u;"),
            format!("const MAX_LIGHTS: u32 = {MAX_LIGHTS}u;"),
            format!("bake_slots: array<vec4<u32>, {}>,", MAX_SLOTS / 4),
        ] {
            assert!(common.contains(&line), "smoke_common.wesl should say `{line}`");
        }
        let march = include_str!("shaders/smoke_march.wesl");
        let line = format!("const RAY_SMOKES: u32 = {OVERLAP8_SMOKES}u;");
        assert!(march.contains(&line), "smoke_march.wesl should say `{line}`");
        // One uniform buffer: WebGPU allows 64 KiB.
        let size = GpuGlobals::min_size().get();
        assert!(size <= 64 << 10, "GpuGlobals is {size} bytes");
    }
}
