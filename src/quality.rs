//! The performance dial: one `quality` number per camera, or a millisecond
//! budget that turns the dial automatically.
//!
//! Every tier changes only *how precisely* smoke is drawn: the resolution the
//! smoke is first traced and lit at, how shadows and occlusion are computed,
//! step budgets. It never changes the smoke's shape (puff layout, blend
//! radius, holes), because the shape decides who can see whom: a player on the
//! lowest tier must not see through smoke that a player on the highest tier
//! can't. Where a tier cuts corners it errs towards drawing *more* smoke:
//!
//! - faceted silhouettes, and anything that might be one, are always traced
//!   again at full resolution; only pixels the coarse samples prove are inside
//!   a facet (or on a convex edge between two) are shaded from them;
//! - a larger hit tolerance can only thicken a silhouette, by at most that
//!   many pixels, never thin it;
//! - a ray that runs out of steps is drawn as smoke ("fail closed").
//!
//! The shape knobs ([`FacetSettings::blend`](crate::FacetSettings), puff
//! spacing and size) are the biggest cost levers of the faceted style, but
//! because they change the shape they belong in the simulation's
//! [`Tuning`](crate::Tuning), the same for every player, not on this dial.

use bevy::{
    diagnostic::{DiagnosticPath, DiagnosticsStore},
    platform::time::Instant,
    prelude::*,
    render::{RenderApp, extract_component::ExtractComponent},
};

use crate::{SmokeCamera, SmokeResolution, render::SmokeFrame};

/// Number of steps on the dial.
pub const TIERS: usize = 6;

/// Names of the tiers, cheapest first.
pub const TIER_NAMES: [&str; TIERS] = ["minimum", "lower", "low", "medium", "high", "ultra"];

/// The tier a `quality` value (0..=1) selects.
pub fn tier_from_quality(quality: f32) -> usize {
    ((quality.clamp(0.0, 1.0) * (TIERS - 1) as f32) + 0.5) as usize
}

/// The `quality` value that selects a tier.
pub fn quality_of_tier(tier: usize) -> f32 {
    tier.min(TIERS - 1) as f32 / (TIERS - 1) as f32
}

/// How the faceted style shades sunlight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FacetShadows {
    /// A shadow ray per pixel through the puffs: crisp facet-on-facet shadows.
    Traced,
    /// One fetch of the per-voxel sun visibility baked for the volumetric
    /// style: soft, nearly free.
    Baked,
}

/// How the faceted style darkens creases and the underside of the cloud.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FacetOcclusion {
    /// Three distance-field taps along the normal: crisp creases.
    Traced,
    /// One fetch of the baked per-voxel occlusion: soft.
    Baked,
}

/// Render knobs of the faceted style for one camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FacetQuality {
    /// The first trace, with its shadows and occlusion, runs at 1/divisor
    /// resolution (1, 2 or 4). Silhouettes and creases between puffs are
    /// traced again at full resolution; facet interiors and convex edges are
    /// shaded from the coarse samples (see `shading_tolerance`). Reduced
    /// resolution only pays while puffs are large on screen, so the divisor
    /// steps down automatically for smokes whose puffs span fewer than about
    /// 30 pixels per divisor step (distant smokes trace at full resolution).
    pub divisor: u32,
    pub shadows: FacetShadows,
    /// Step budget of a traced shadow ray.
    pub shadow_steps: u32,
    pub occlusion: FacetOcclusion,
    /// Step budget of a camera ray per smoke. Running out draws smoke.
    pub max_steps: u32,
    /// Surface hit tolerance in pixels. Larger converges in fewer steps; the
    /// silhouette grows by at most this much.
    pub tolerance_px: f32,
    /// How far apart (0..1) the coarse samples on one facet may be in sun or
    /// sky visibility for a pixel between them to reuse their lighting
    /// instead of computing its own shadow ray and occlusion. Lower keeps
    /// shadow edges sharper and costs more; 0 lights and traces every pixel
    /// itself. Unused at divisor 1.
    pub shading_tolerance: f32,
}

/// Render knobs of the volumetric style for one camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumetricQuality {
    /// Raymarch resolution relative to the camera's.
    pub resolution: SmokeResolution,
    /// Upper bound on raymarch steps per pixel.
    pub max_steps: u32,
    /// Step length in metres near the camera.
    pub step_size: f32,
    /// Extra step length per metre of distance.
    pub step_growth: f32,
}

impl FacetQuality {
    /// The faceted knobs for each tier, cheapest first.
    pub const TIERS: [Self; TIERS] = [
        Self::new(4, FacetShadows::Baked, 0, FacetOcclusion::Baked, 48, 1.0, 0.25),
        Self::new(4, FacetShadows::Baked, 0, FacetOcclusion::Baked, 64, 0.75, 0.12),
        Self::new(2, FacetShadows::Baked, 0, FacetOcclusion::Baked, 80, 0.5, 0.08),
        Self::new(2, FacetShadows::Baked, 0, FacetOcclusion::Traced, 96, 0.4, 0.05),
        Self::new(2, FacetShadows::Traced, 32, FacetOcclusion::Traced, 112, 0.3, 0.03),
        Self::new(1, FacetShadows::Traced, 48, FacetOcclusion::Traced, 128, 0.25, 0.0),
    ];

    #[allow(clippy::too_many_arguments)]
    const fn new(
        divisor: u32,
        shadows: FacetShadows,
        shadow_steps: u32,
        occlusion: FacetOcclusion,
        max_steps: u32,
        tolerance_px: f32,
        shading_tolerance: f32,
    ) -> Self {
        Self {
            divisor,
            shadows,
            shadow_steps,
            occlusion,
            max_steps,
            tolerance_px,
            shading_tolerance,
        }
    }

    pub fn tier(tier: usize) -> Self {
        Self::TIERS[tier.min(TIERS - 1)]
    }
}

impl VolumetricQuality {
    /// The volumetric knobs for each tier, cheapest first.
    pub const TIERS: [Self; TIERS] = [
        Self::new(SmokeResolution::Quarter, 28, 0.26, 0.06),
        Self::new(SmokeResolution::Quarter, 40, 0.2, 0.05),
        Self::new(SmokeResolution::Half, 48, 0.16, 0.04),
        Self::new(SmokeResolution::Half, 72, 0.11, 0.03),
        Self::new(SmokeResolution::TwoThirds, 96, 0.09, 0.025),
        Self::new(SmokeResolution::Full, 128, 0.07, 0.02),
    ];

    const fn new(resolution: SmokeResolution, max_steps: u32, step_size: f32, step_growth: f32) -> Self {
        Self {
            resolution,
            max_steps,
            step_size,
            step_growth,
        }
    }

    pub fn tier(tier: usize) -> Self {
        Self::TIERS[tier.min(TIERS - 1)]
    }
}

/// Cost of each tier relative to the top tier, used by the budget controller
/// to predict what a tier switch will save. Measured with the demo's scripted
/// tier sweep with the smoke close up (where budgets bind; at a distance the
/// faceted tiers save less, about 0.5× at the bottom); the controller learns
/// the absolute scale on the player's GPU.
pub const FACET_RELATIVE_COST: [f32; TIERS] = [0.27, 0.29, 0.36, 0.42, 0.52, 1.0];
pub const VOLUMETRIC_RELATIVE_COST: [f32; TIERS] = [0.09, 0.10, 0.18, 0.22, 0.38, 1.0];

/// What the dial is doing on a camera this frame. Added automatically with
/// [`SmokeCamera`]; read it for HUDs and logs.
#[derive(Component, Clone, Debug, Default, ExtractComponent)]
#[extract_app(RenderApp)]
pub struct SmokeQuality {
    /// Tier in use, 0 (cheapest) ..= 5.
    pub tier: usize,
    /// Most recent GPU time of this camera's smoke passes, if measured.
    pub measured_ms: Option<f32>,
    /// What the controller expects the current tier to cost.
    pub predicted_ms: f32,
    /// Fraction of the screen the smokes cover (their screen rectangles).
    pub coverage: f32,
    pub(crate) controller: BudgetController,
}

impl SmokeQuality {
    pub fn tier_name(&self) -> &'static str {
        TIER_NAMES[self.tier.min(TIERS - 1)]
    }
}

/// Exponential moving average.
#[derive(Clone, Copy, Debug, Default)]
struct Ema {
    value: f32,
    primed: bool,
}

impl Ema {
    fn push(&mut self, x: f32, alpha: f32) -> f32 {
        if self.primed {
            self.value += (x - self.value) * alpha;
        } else {
            self.value = x;
            self.primed = true;
        }
        self.value
    }
}

/// Frames between a frame being rendered and its GPU timing arriving in the
/// main world (pipelined rendering plus the timestamp readback).
const MEASUREMENT_LAG: usize = 2;
/// Fixed cost assumed for having any smoke pass at all (dispatch overhead).
const BASE_MS: f32 = 0.03;

/// Picks a tier that fits a millisecond budget.
///
/// The cost of smoke is close to proportional to the screen area it covers
/// times a per-tier factor, so the controller learns one scale per style
/// (milliseconds for a full screen at the top tier) from GPU timings, then
/// each frame predicts every tier's cost from the *current* coverage. That
/// makes it react the moment a smoke fills the screen instead of a few
/// frames later when the slow frame is measured. It drops tiers immediately
/// and climbs back one tier at a time once the next tier has fit with room
/// to spare for a while, so it doesn't oscillate.
#[derive(Clone, Debug)]
pub struct BudgetController {
    /// Learned milliseconds for full-screen coverage at the top tier.
    pub scale_volumetric: f32,
    pub scale_facets: f32,
    calibrated_volumetric: bool,
    calibrated_facets: bool,
    /// Recent per-frame loads (coverage × relative cost), newest last.
    history: [[f32; 2]; MEASUREMENT_LAG + 2],
    load: [Ema; 2],
    tier: Option<usize>,
    climb_frames: u32,
    last_seen: [Option<Instant>; PASS_COUNT],
    pub(crate) warned: bool,
}

impl Default for BudgetController {
    fn default() -> Self {
        Self {
            // A cautious first guess until the first measurement arrives.
            scale_volumetric: 4.0,
            scale_facets: 4.0,
            calibrated_volumetric: false,
            calibrated_facets: false,
            history: [[0.0; 2]; MEASUREMENT_LAG + 2],
            load: [Ema::default(); 2],
            tier: None,
            climb_frames: 0,
            last_seen: [None; PASS_COUNT],
            warned: false,
        }
    }
}

/// One frame of input to [`BudgetController::update`].
#[derive(Clone, Copy, Debug, Default)]
pub struct BudgetInputs {
    pub budget_ms: f32,
    /// The highest tier allowed (from `SmokeCamera::quality`).
    pub max_tier: usize,
    /// Screen coverage (0..1) of volumetric and of faceted smoke this frame.
    pub coverage: [f32; 2],
    /// GPU milliseconds of each style's passes, when a new timing arrived.
    pub measured_ms: [Option<f32>; 2],
}

const CLIMB_FRAMES: u32 = 30;
const HEADROOM: f32 = 0.9;
const CLIMB_HEADROOM: f32 = 0.75;

impl BudgetController {
    fn relative(style: usize, tier: usize) -> f32 {
        if style == 0 {
            VOLUMETRIC_RELATIVE_COST[tier]
        } else {
            FACET_RELATIVE_COST[tier]
        }
    }

    /// Predicted milliseconds of `tier` at the given coverage.
    pub fn predict(&self, coverage: [f32; 2], tier: usize) -> f32 {
        let mut ms = 0.0;
        let scales = [self.scale_volumetric, self.scale_facets];
        for style in 0..2 {
            if coverage[style] > 0.0 {
                ms += BASE_MS + scales[style] * coverage[style] * Self::relative(style, tier);
            }
        }
        ms
    }

    /// Feed one frame; returns the tier to render with.
    pub fn update(&mut self, input: &BudgetInputs) -> usize {
        let max_tier = input.max_tier.min(TIERS - 1);
        let mut tier = self.tier.unwrap_or(max_tier.min(3)).min(max_tier);

        // Learn the scale from timings, matched against the load of the frame
        // they were measured on.
        // history[len - 1] is the previous frame, so a timing that arrives now
        // (MEASUREMENT_LAG frames late) belongs to history[len - LAG].
        let lagged = self.history[self.history.len() - MEASUREMENT_LAG];
        #[allow(clippy::needless_range_loop)]
        for style in 0..2 {
            let load = self.load[style].push(lagged[style], 0.5);
            if let Some(ms) = input.measured_ms[style]
                && load > 0.01
            {
                let observed = ((ms - BASE_MS).max(0.0) / load).clamp(0.01, 1000.0);
                let (scale, calibrated) = if style == 0 {
                    (&mut self.scale_volumetric, &mut self.calibrated_volumetric)
                } else {
                    (&mut self.scale_facets, &mut self.calibrated_facets)
                };
                *scale = if *calibrated {
                    *scale + (observed - *scale) * 0.25
                } else {
                    observed
                };
                *calibrated = true;
            }
        }

        // Highest tier predicted to fit.
        let budget = input.budget_ms.max(0.01);
        let mut best = 0;
        for t in (0..=max_tier).rev() {
            if self.predict(input.coverage, t) <= budget * HEADROOM {
                best = t;
                break;
            }
        }
        if best < tier {
            tier = best;
            self.climb_frames = 0;
        } else if best > tier {
            if self.predict(input.coverage, tier + 1) <= budget * CLIMB_HEADROOM {
                self.climb_frames += 1;
                if self.climb_frames >= CLIMB_FRAMES {
                    tier += 1;
                    self.climb_frames = 0;
                }
            } else {
                self.climb_frames = 0;
            }
        } else {
            self.climb_frames = 0;
        }
        self.tier = Some(tier);

        self.history.rotate_left(1);
        let newest = self.history.len() - 1;
        for style in 0..2 {
            self.history[newest][style] = input.coverage[style] * Self::relative(style, tier);
        }
        tier
    }
}

/// Screen-space rectangle (viewport pixels, min..max) covered by a set of
/// world-space boxes, or `None` if none is in view. Conservative: a box that
/// straddles the camera plane, or contains the camera, counts as full screen.
pub(crate) fn screen_rect(
    clip_from_world: Mat4,
    camera: Vec3,
    near: f32,
    size: Vec2,
    boxes: impl IntoIterator<Item = (Vec3, Vec3)>,
) -> Option<(Vec2, Vec2)> {
    let mut min = Vec2::splat(f32::MAX);
    let mut max = Vec2::splat(f32::MIN);
    let mut any = false;
    for (bmin, bmax) in boxes {
        let pad = Vec3::splat(near * 4.0 + 0.05);
        if camera.cmpge(bmin - pad).all() && camera.cmple(bmax + pad).all() {
            return Some((Vec2::ZERO, size));
        }
        let mut lo = Vec2::splat(f32::MAX);
        let mut hi = Vec2::splat(f32::MIN);
        let mut behind = 0;
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { bmin.x } else { bmax.x },
                if i & 2 == 0 { bmin.y } else { bmax.y },
                if i & 4 == 0 { bmin.z } else { bmax.z },
            );
            let clip = clip_from_world * corner.extend(1.0);
            if clip.w <= 1e-4 {
                behind += 1;
                continue;
            }
            let ndc = clip.truncate().truncate() / clip.w;
            lo = lo.min(ndc);
            hi = hi.max(ndc);
        }
        if behind == 8 {
            continue; // entirely behind the camera
        }
        if behind > 0 {
            // Straddles the camera plane: its projection is unbounded.
            return Some((Vec2::ZERO, size));
        }
        if hi.x < -1.0 || hi.y < -1.0 || lo.x > 1.0 || lo.y > 1.0 {
            continue; // outside the frustum
        }
        // NDC y points up, pixels go down.
        let p0 = Vec2::new((lo.x * 0.5 + 0.5) * size.x, (0.5 - hi.y * 0.5) * size.y);
        let p1 = Vec2::new((hi.x * 0.5 + 0.5) * size.x, (0.5 - lo.y * 0.5) * size.y);
        min = min.min(p0);
        max = max.max(p1);
        any = true;
    }
    if !any {
        return None;
    }
    let min = min.clamp(Vec2::ZERO, size).floor();
    let max = max.clamp(Vec2::ZERO, size).ceil();
    (max.x > min.x && max.y > min.y).then_some((min, max))
}

/// GPU timing spans of the volumetric style's per-camera passes, recorded by
/// `RenderDiagnosticsPlugin` as `render/<name>/elapsed_gpu`.
pub const VOLUMETRIC_PASSES: [&str; 2] = ["smoke_march", "smoke_composite"];
/// GPU timing spans of the faceted style's per-camera passes: the
/// full-resolution trace, or coarse, classify, retrace and composite.
pub const FACET_PASSES: [&str; 5] = [
    "smoke_facet_trace",
    "smoke_facet_coarse",
    "smoke_facet_classify",
    "smoke_facet_retrace",
    "smoke_facet_composite",
];
const PASS_COUNT: usize = VOLUMETRIC_PASSES.len() + FACET_PASSES.len();

fn pass_path(name: &str) -> DiagnosticPath {
    DiagnosticPath::new(format!("render/{name}/elapsed_gpu"))
}

/// Picks every smoke camera's tier for this frame.
pub(crate) fn update_smoke_quality(
    mut cameras: Query<(&SmokeCamera, &mut SmokeQuality, &Camera, &GlobalTransform)>,
    frame: Res<SmokeFrame>,
    diagnostics: Option<Res<DiagnosticsStore>>,
) {
    for (settings, mut quality, camera, transform) in &mut cameras {
        let quality = quality.as_mut();
        let max_tier = tier_from_quality(settings.quality);

        // Coverage of each style on this camera (smoke data is in the core's
        // internal frame).
        let mut coverage = [0.0_f32; 2];
        if let Some(size) = camera.physical_viewport_size() {
            let clip_from_view = camera.clip_from_view();
            let size = size.max(UVec2::ONE).as_vec2();
            let internal_from_world = frame.internal_from_world;
            let clip_from_internal = clip_from_view * transform.to_matrix().inverse() * internal_from_world.inverse();
            let eye = internal_from_world.transform_point3(transform.translation());
            let near = clip_from_view.w_axis.z * internal_from_world.x_axis.truncate().length();
            for (style, faceted) in [false, true].into_iter().enumerate() {
                let boxes = frame
                    .slots
                    .iter()
                    .filter(|s| s.facet.is_some() == faceted)
                    .map(|s| (s.bounds_min, s.bounds_max));
                if let Some((lo, hi)) = screen_rect(clip_from_internal, eye, near, size, boxes) {
                    let area = (hi - lo).x * (hi - lo).y;
                    coverage[style] = area / (size.x * size.y);
                }
            }
        }
        quality.coverage = coverage[0] + coverage[1];

        // Newest GPU timings.
        let mut measured = [None; 2];
        let mut any_path = false;
        if let Some(store) = diagnostics.as_deref() {
            let passes = VOLUMETRIC_PASSES
                .iter()
                .map(|p| (0, p))
                .chain(FACET_PASSES.iter().map(|p| (1, p)));
            for (k, (style, name)) in passes.enumerate() {
                {
                    let Some(m) = store.get(&pass_path(name)).and_then(|d| d.measurement()) else {
                        continue;
                    };
                    any_path = true;
                    let seen = &mut quality.controller.last_seen[k];
                    if seen.is_some_and(|t| t >= m.time) {
                        continue;
                    }
                    *seen = Some(m.time);
                    *measured[style].get_or_insert(0.0) += m.value as f32;
                }
            }
        }
        if measured.iter().any(Option::is_some) {
            quality.measured_ms = Some(measured.iter().flatten().sum());
        }

        match settings.budget_ms {
            None => {
                quality.tier = max_tier;
                quality.predicted_ms = quality.controller.predict(coverage, max_tier);
            }
            Some(budget) => {
                if !any_path && quality.coverage > 0.0 && !quality.controller.warned {
                    quality.controller.warned = true;
                    warn!(
                        "SmokeCamera::budget_ms needs GPU timings: add bevy::render::diagnostic::RenderDiagnosticsPlugin (and a GPU with timestamp queries). Until timings arrive the dial holds a cautious tier."
                    );
                }
                quality.tier = quality.controller.update(&BudgetInputs {
                    budget_ms: budget,
                    max_tier,
                    coverage,
                    measured_ms: measured,
                });
                quality.predicted_ms = quality.controller.predict(coverage, quality.tier);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake GPU: cost proportional to coverage × tier cost, with the
    /// timing of frame N arriving at the start of frame N + MEASUREMENT_LAG.
    struct FakeGpu {
        scale: [f32; 2],
        queue: Vec<[Option<f32>; 2]>,
    }

    impl FakeGpu {
        fn cost(&self, coverage: [f32; 2], tier: usize) -> [Option<f32>; 2] {
            [
                (coverage[0] > 0.0).then(|| self.scale[0] * coverage[0] * VOLUMETRIC_RELATIVE_COST[tier] + BASE_MS),
                (coverage[1] > 0.0).then(|| self.scale[1] * coverage[1] * FACET_RELATIVE_COST[tier] + BASE_MS),
            ]
        }

        fn deliver(&mut self) -> [Option<f32>; 2] {
            if self.queue.len() >= MEASUREMENT_LAG {
                self.queue.remove(0)
            } else {
                [None, None]
            }
        }

        fn render(&mut self, coverage: [f32; 2], tier: usize) {
            let ms = self.cost(coverage, tier);
            self.queue.push(ms);
        }
    }

    #[test]
    fn quality_maps_to_tiers() {
        assert_eq!(tier_from_quality(0.0), 0);
        assert_eq!(tier_from_quality(1.0), TIERS - 1);
        assert_eq!(tier_from_quality(0.8), 4);
        for t in 0..TIERS {
            assert_eq!(tier_from_quality(quality_of_tier(t)), t);
        }
        // Higher tiers never cost less.
        for w in FACET_RELATIVE_COST
            .windows(2)
            .chain(VOLUMETRIC_RELATIVE_COST.windows(2))
        {
            assert!(w[0] <= w[1]);
        }
    }

    #[test]
    fn budget_holds_when_smoke_fills_the_screen() {
        let mut gpu = FakeGpu {
            scale: [3.0, 6.0],
            queue: Vec::new(),
        };
        let mut c = BudgetController::default();
        let budget = 1.5;
        let mut over_budget_frames = 0;
        let mut tiers = Vec::new();
        for frame in 0..600 {
            // Small smoke in the distance, then it fills 70 % of the screen.
            let cov = if frame < 200 { 0.08 } else { 0.7 };
            let measured = gpu.deliver();
            let tier = c.update(&BudgetInputs {
                budget_ms: budget,
                max_tier: TIERS - 1,
                coverage: [0.0, cov],
                measured_ms: measured,
            });
            gpu.render([0.0, cov], tier);
            tiers.push(tier);
            let actual = gpu.cost([0.0, cov], tier)[1].unwrap();
            if frame > 10 && actual > budget {
                over_budget_frames += 1;
            }
        }
        // Far away it climbs to the top tier; up close it drops to one that fits.
        assert_eq!(tiers[190], TIERS - 1, "{:?}", &tiers[150..200]);
        let settled = *tiers.last().unwrap();
        assert!(settled < TIERS - 1);
        assert!(gpu.scale[1] * 0.7 * FACET_RELATIVE_COST[settled] + BASE_MS <= budget);
        // At most a couple of frames over budget around the switch.
        assert!(over_budget_frames <= 3, "{over_budget_frames} frames over budget");
        // No oscillation once settled.
        assert!(tiers[400..].iter().all(|&t| t == settled), "{:?}", &tiers[400..]);
    }

    #[test]
    fn respects_the_quality_ceiling() {
        let mut c = BudgetController::default();
        for _ in 0..200 {
            let t = c.update(&BudgetInputs {
                budget_ms: 100.0,
                max_tier: 2,
                coverage: [0.5, 0.5],
                measured_ms: [Some(0.1), Some(0.1)],
            });
            assert!(t <= 2);
        }
    }

    #[test]
    fn screen_rect_covers_projected_box() {
        use bevy::camera::CameraProjection;
        // Bevy's perspective projection (reverse-Z, infinite far plane).
        let proj = bevy::camera::PerspectiveProjection {
            fov: 1.0,
            aspect_ratio: 16.0 / 9.0,
            near: 0.1,
            ..Default::default()
        }
        .get_clip_from_view();
        let view = Transform::from_xyz(0.0, 1.0, 10.0)
            .looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y)
            .to_matrix()
            .inverse();
        let size = Vec2::new(1920.0, 1080.0);
        let r = screen_rect(
            proj * view,
            Vec3::new(0.0, 1.0, 10.0),
            0.1,
            size,
            [(Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 2.0, 1.0))],
        )
        .unwrap();
        // Centred box, smaller than the screen.
        assert!(
            r.0.x > 600.0 && r.1.x < 1320.0 && r.0.y > 200.0 && r.1.y < 880.0,
            "{r:?}"
        );
        // Behind the camera: nothing.
        assert!(
            screen_rect(
                proj * view,
                Vec3::new(0.0, 1.0, 10.0),
                0.1,
                size,
                [(Vec3::new(-1.0, 0.0, 20.0), Vec3::new(1.0, 2.0, 22.0))],
            )
            .is_none()
        );
    }
}
