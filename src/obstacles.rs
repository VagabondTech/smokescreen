//! Level geometry from meshes: mark the level's entities with
//! [`SmokeObstacle`] and the plugin voxelises their meshes into the
//! [`SmokeLevel`](crate::SmokeLevel) on a background thread, whenever one is
//! added, moved or removed.

use std::sync::Arc;

use bevy::{
    mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues},
    prelude::*,
    tasks::{AsyncComputeTaskPool, Task, futures::check_ready},
};
use smokescreen::VoxelObstacles;

use crate::{
    SmokeConfig, SmokeLevel,
    setup::{Mistake, SetupWarnings},
};

/// Marks static level geometry smoke flows around: walls, floors, crates.
/// Put it on entities with a `Mesh3d` (the mesh data must stay in the main
/// world, which is the default). The plugin voxelises every marked mesh into
/// the [`SmokeLevel`](crate::SmokeLevel), so no collision code is needed.
///
/// It marks the entity's own mesh, not its children's. For a glTF level,
/// add it to the scene's mesh entities once the scene has spawned (an
/// observer for `WorldInstanceReady` on its `WorldAssetRoot`).
///
/// Include the floor: without it smoke has nothing to rest on. For
/// multiplayer, voxelise once and ship the result
/// ([`VoxelObstacles::to_bytes`]) so every machine has the same level.
///
/// Meshes are voxelised by their surfaces, and the way their triangles
/// face says which side is solid: smoke then fills right up to them. Bevy's
/// own shapes face outwards, as do most exported meshes. A single sheet
/// facing up (a `Plane3d`) counts as ground, solid all the way down, so use
/// a box for a raised floor with open air below it. Where triangles face
/// inwards (a room made of one box seen from inside), or a wall is a single
/// upright sheet, smoke stops up to half a voxel short of the surface.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct SmokeObstacle;

#[derive(Resource, Default)]
pub(crate) struct ObstacleBuild {
    task: Option<Task<Option<VoxelObstacles>>>,
    dirty: bool,
}

impl ObstacleBuild {
    /// Marked meshes are loading or being voxelised. Until the first level
    /// exists, new smokes wait for it, so they don't spread through walls
    /// that are about to.
    pub(crate) fn pending(&self) -> bool {
        self.task.is_some() || self.dirty
    }
}

/// Rebuild the level when marked meshes change; install it when done.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) fn voxelise_obstacles(
    mut commands: Commands,
    mut build: ResMut<ObstacleBuild>,
    changed: Query<(), (With<SmokeObstacle>, Or<(Changed<GlobalTransform>, Changed<Mesh3d>)>)>,
    mut removed: RemovedComponents<SmokeObstacle>,
    obstacles: Query<(Entity, &Mesh3d, &GlobalTransform), With<SmokeObstacle>>,
    unmeshed: Query<Entity, (With<SmokeObstacle>, Without<Mesh3d>)>,
    meshes: Option<Res<Assets<Mesh>>>,
    asset_server: Res<AssetServer>,
    config: Res<SmokeConfig>,
    mut warnings: ResMut<SetupWarnings>,
) {
    if !changed.is_empty() || removed.read().count() > 0 {
        build.dirty = true;
    }
    if let Some(task) = build.task.as_mut() {
        let Some(done) = check_ready(task) else {
            return;
        };
        build.task = None;
        if let Some(level) = done {
            commands.insert_resource(SmokeLevel(Arc::new(level)));
        }
    }
    if !build.dirty {
        return;
    }
    if !warnings.warned(Mistake::ObstacleWithoutMesh)
        && let Some(first) = unmeshed.iter().next()
    {
        let n = unmeshed.iter().len();
        warnings.warn(Mistake::ObstacleWithoutMesh, || {
            let which = if n == 1 {
                format!("an entity without a Mesh3d ({first}), so it adds")
            } else {
                format!("{n} entities without a Mesh3d (the first: {first}), so they add")
            };
            format!(
                "SmokeObstacle is on {which} nothing to the level. It marks an entity's own mesh, not its children's: for a glTF level, add it to the scene's mesh entities once the scene has spawned (an observer for WorldInstanceReady on its WorldAssetRoot)."
            )
        });
    }
    let Some(meshes) = meshes else {
        return;
    };
    let mut triangles: Vec<[Vec3; 3]> = Vec::new();
    for (entity, handle, transform) in &obstacles {
        let Some(mesh) = meshes.get(&handle.0) else {
            // Still loading: try again next frame. One that failed never
            // will, and keeps the level from being built.
            if asset_server.load_state(handle.id()).is_failed() {
                warnings.warn(Mistake::MeshFailed, || {
                    let path = asset_server
                        .get_path(handle.id())
                        .map_or_else(|| "no path".into(), |p| p.to_string());
                    format!(
                        "A SmokeObstacle mesh failed to load ({path}, on {entity}), and the level is built only once every marked mesh has loaded: until then smokes wait for it (or keep the last level). Fix the asset, or take SmokeObstacle off that entity."
                    )
                });
            }
            return;
        };
        if !append_triangles(mesh, &transform.affine(), &mut triangles) {
            warnings.warn(Mistake::MeshWithoutTriangles, || {
                format!(
                    "A SmokeObstacle mesh (on {entity}) has no triangle data in the main world (is it a triangle list, loaded with RenderAssetUsages::MAIN_WORLD?); smoke ignores it."
                )
            });
        }
    }
    build.dirty = false;
    if triangles.is_empty() {
        return;
    }
    // The fills' voxels, in world units.
    let (voxel, up) = (config.world_voxel_size, config.units.up);
    build.task = Some(
        AsyncComputeTaskPool::get()
            .spawn(async move { Some(VoxelObstacles::from_triangles(&triangles, voxel, voxel * 4.0, up)) }),
    );
}

/// Add a mesh's triangles, transformed to the world. `false` if it has none
/// we can read. Their winding says which side is solid (see
/// [`VoxelObstacles::from_triangles`]), so a mirroring transform turns them
/// round to keep them facing outwards.
fn append_triangles(mesh: &Mesh, transform: &bevy::math::Affine3A, out: &mut Vec<[Vec3; 3]>) -> bool {
    if mesh.primitive_topology() != PrimitiveTopology::TriangleList {
        return false;
    }
    let Ok(Some(VertexAttributeValues::Float32x3(positions))) = mesh.try_attribute_option(Mesh::ATTRIBUTE_POSITION)
    else {
        return false;
    };
    let p = |i: usize| transform.transform_point3(Vec3::from(positions[i]));
    let mirrored = transform.matrix3.determinant() < 0.0;
    let mut push = |a: usize, b: usize, c: usize| {
        out.push(if mirrored {
            [p(a), p(c), p(b)]
        } else {
            [p(a), p(b), p(c)]
        });
    };
    match mesh.try_indices_option() {
        Ok(Some(indices)) => {
            let idx: Vec<usize> = match indices {
                Indices::U16(v) => v.iter().map(|&i| i as usize).collect(),
                Indices::U32(v) => v.iter().map(|&i| i as usize).collect(),
            };
            for &[a, b, c] in idx.as_chunks::<3>().0 {
                if [a, b, c].iter().all(|&i| i < positions.len()) {
                    push(a, b, c);
                }
            }
        }
        Ok(None) => {
            for i in (0..positions.len() - positions.len() % 3).step_by(3) {
                push(i, i + 1, i + 2);
            }
        }
        Err(_) => return false,
    }
    true
}
