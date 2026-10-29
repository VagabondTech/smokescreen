//! Warnings for common setup mistakes. Each is logged once per app and says
//! what to do; the systems that can see a mistake check for it where they
//! already look, so a check costs nothing per frame.

use std::sync::atomic::{AtomicU32, Ordering};

use bevy::prelude::*;
use smokescreen::{CloudSource, CloudSpec, Smokes};

use crate::SharedLevel;

/// A setup mistake the plugin warns about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mistake {
    /// No active camera has `SmokeCamera`.
    NoSmokeCamera,
    /// `SmokeCamera` on an orthographic camera.
    Orthographic,
    /// A smoke camera lit by its own ambient light or an environment map,
    /// while smoke takes `GlobalAmbientLight`.
    CameraAmbient,
    /// `SmokeCamera::budget_ms` without GPU timings.
    NoTimings,
    /// Several directional lights, and not exactly one `SmokeSun`.
    Sun,
    /// A smoke popped with no level.
    NoLevel,
    /// A source or a volume bigger than the fill grid.
    TooBig,
    /// A smoke popped inside solid level geometry: an empty cloud.
    InsideSolid,
    /// More smokes alive than `Tuning::max_clouds`.
    TooManyClouds,
    /// Every slot taken (`Tuning::max_clouds` above 32).
    SlotsFull,
    /// More holes open in the smokes drawn than the renderer has room for.
    TooManyHoles,
    /// `SmokeObstacle` on an entity without a `Mesh3d`.
    ObstacleWithoutMesh,
    /// A `SmokeObstacle` mesh without triangles in the main world.
    MeshWithoutTriangles,
    /// A `SmokeObstacle` mesh that failed to load.
    MeshFailed,
    /// No `TransformPlugin`: nothing updates `GlobalTransform`.
    NoTransforms,
    /// `SmokeVision` asked after the clock moved past the tick its views
    /// were prepared for.
    VisionTick,
}

/// The mistakes warned about so far. Atomic, so that systems that only read
/// it (`SmokeVision`'s) can warn too.
#[derive(Resource, Default)]
pub(crate) struct SetupWarnings(AtomicU32);

impl SetupWarnings {
    pub(crate) fn warned(&self, mistake: Mistake) -> bool {
        self.0.load(Ordering::Relaxed) & (1 << mistake as u32) != 0
    }

    /// Log `message` the first time `mistake` is seen.
    pub(crate) fn warn(&self, mistake: Mistake, message: impl FnOnce() -> String) {
        let bit = 1 << mistake as u32;
        if self.0.load(Ordering::Relaxed) & bit == 0 && self.0.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
            warn!("{}", message());
        }
    }

    /// Nothing warned about yet.
    #[cfg(test)]
    pub(crate) fn none(&self) -> bool {
        self.0.load(Ordering::Relaxed) == 0
    }
}

/// A position, an extent or a size for a message, to a hundredth.
pub(crate) fn shown(v: Vec3) -> String {
    format!("({}, {}, {})", round(v.x), round(v.y), round(v.z))
}

fn round(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}

/// What of a smoke doesn't fit its fill grid, as a message: a line or box
/// source reaching past the grid's faces, or more volume than it holds.
pub(crate) fn misfit(spec: &CloudSpec, world: &Smokes<SharedLevel>) -> Option<String> {
    // In the core's frame: metres, Y up.
    let t = world.internal_tuning();
    let units = world.units();
    let v = t.voxel_size;
    let dims = t.grid_dims.as_vec3();
    // Room each way from the grid's centre (the source's).
    let below = dims.y * t.floor_fraction * v;
    let above = dims.y * (1.0 - t.floor_fraction) * v;
    let across = dims * 0.5 * v;
    let fits = |half: Vec3| half.x <= across.x && half.z <= across.z && half.y <= below.min(above);
    let world_voxel = world.config().tuning.voxel_size;
    let grid = format!(
        "{} × {} across and {} high, {} of it below the source: Tuning::grid_dims × voxel_size",
        round(dims.x * world_voxel),
        round(dims.z * world_voxel),
        round(dims.y * world_voxel),
        round(dims.y * t.floor_fraction * world_voxel),
    );
    match spec.source {
        CloudSource::Box { half_extents } => {
            let half = (units.dir_to_internal(half_extents) * units.metres_per_unit).abs();
            if !fits(half) {
                return Some(format!(
                    "A smoke's box source (half extents {}) doesn't fit its fill grid ({grid}), so the cloud is cut off at the grid's faces. Use a smaller box or several smokes, or a larger grid_dims or voxel_size.",
                    shown(half_extents)
                ));
            }
        }
        CloudSource::Line { end } => {
            let half = (units.to_internal(end) - units.to_internal(spec.origin)).abs() * 0.5;
            if !fits(half) {
                return Some(format!(
                    "A smoke's line source ({} long) doesn't fit its fill grid ({grid}), so the wall is cut off at the grid's faces. Use several shorter walls, or a larger grid_dims or voxel_size.",
                    round(end.distance(spec.origin))
                ));
            }
        }
        // A point fits anywhere (and the core has no other sources yet).
        _ => {}
    }
    let volume = spec.volume.map_or(t.volume, |vol| units.volume_to_internal(vol));
    let holds = dims.x * dims.y * dims.z * v * v * v;
    (volume > holds).then(|| {
        format!(
            "A smoke asks for a volume of {}, more than its fill grid holds ({}; {grid}), so it fills the grid and stops. Use a smaller volume or several smokes, or a larger grid_dims or voxel_size.",
            round(units.volume_from_internal(volume)),
            round(units.volume_from_internal(holds)),
        )
    })
}
