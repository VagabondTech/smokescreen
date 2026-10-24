//! The crystal style: every puff is a plain flat-shaded mesh, drawn by Bevy's
//! own renderer like any other object (so it is lit, casts and receives
//! shadows, and costs a few triangles). Each frame, before Bevy works out
//! what is visible, the puffs are moved to where the core says they are at
//! the drawn tick, so the picture is exactly what the core's rule checks.

use bevy::{
    asset::RenderAssetUsages,
    light::NotShadowCaster,
    mesh::{Mesh, PrimitiveTopology},
    platform::collections::HashMap,
    prelude::*,
};
use smokescreen::{CloudId, LivePuff, Polyhedron, Style, Units};

use crate::{Smoke, SmokeClock, SmokeConfig, SmokeSettings, SmokeWorld};

/// Marks the entities that draw crystal puffs. They belong to the plugin:
/// don't change or despawn them, but you can filter on this (to keep them
/// out of your own raycasts, say).
#[derive(Component, Clone, Copy, Debug)]
pub struct CrystalPuff {
    /// The cloud this puff belongs to.
    pub cloud: CloudId,
}

/// One mesh per polyhedron, inradius 1.
#[derive(Resource)]
pub(crate) struct CrystalMeshes([Handle<Mesh>; 4]);

/// The puff entities of every crystal cloud being drawn.
#[derive(Resource, Default)]
pub(crate) struct CrystalClouds(HashMap<CloudId, CrystalCloud>);

pub(crate) struct CrystalCloud {
    puffs: Vec<Entity>,
    material: Handle<StandardMaterial>,
    /// What the material was made from: tint, albedo, debug flag.
    look: (Vec3, Vec3, bool),
    shadows: bool,
}

pub(crate) fn setup_meshes(mut commands: Commands, meshes: Option<ResMut<Assets<Mesh>>>) {
    let Some(mut meshes) = meshes else {
        return;
    };
    let shapes = [
        Polyhedron::Cube,
        Polyhedron::Octahedron,
        Polyhedron::Dodecahedron,
        Polyhedron::Icosahedron,
    ];
    commands.insert_resource(CrystalMeshes(shapes.map(|s| meshes.add(polyhedron_mesh(s)))));
}

/// A flat-shaded mesh of the polyhedron with inradius 1, built from the same
/// face planes the core checks against, so the two agree exactly.
pub(crate) fn polyhedron_mesh(shape: Polyhedron) -> Mesh {
    let planes: Vec<Vec3> = shape.face_normals().iter().flat_map(|n| [*n, -*n]).collect();
    // Corners: where three faces meet, inside all the others.
    let mut corners: Vec<Vec3> = Vec::new();
    for i in 0..planes.len() {
        for j in i + 1..planes.len() {
            for k in j + 1..planes.len() {
                let (a, b, c) = (planes[i], planes[j], planes[k]);
                let det = a.dot(b.cross(c));
                if det.abs() < 1e-6 {
                    continue;
                }
                let x = (b.cross(c) + c.cross(a) + a.cross(b)) / det;
                if planes.iter().all(|n| n.dot(x) <= 1.0 + 1e-4) && !corners.iter().any(|v| v.distance(x) < 1e-4) {
                    corners.push(x);
                }
            }
        }
    }
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    for n in &planes {
        let mut face: Vec<Vec3> = corners
            .iter()
            .copied()
            .filter(|v| (n.dot(*v) - 1.0).abs() < 1e-4)
            .collect();
        let centre = face.iter().copied().sum::<Vec3>() / face.len() as f32;
        let (u, w) = n.any_orthonormal_pair();
        face.sort_by(|p, q| {
            let angle = |v: &Vec3| (*v - centre).dot(w).atan2((*v - centre).dot(u));
            angle(p).total_cmp(&angle(q))
        });
        for t in 1..face.len().saturating_sub(1) {
            let (a, b, c) = (face[0], face[t], face[t + 1]);
            // Counter-clockwise seen from outside.
            let (b, c) = if (b - a).cross(c - a).dot(*n) >= 0.0 {
                (b, c)
            } else {
                (c, b)
            };
            positions.extend([a, b, c].map(|v| v.to_array()));
            normals.extend([n.to_array(); 3]);
        }
    }
    // No UVs: nothing samples a texture, and Safari 26 can't compile Bevy
    // 0.20's PBR shader for meshes with them.
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
}

fn crystal_material(tint: Vec3, albedo: Vec3, debug: bool) -> StandardMaterial {
    if debug {
        return StandardMaterial {
            base_color: Color::srgb(1.0, 0.0, 1.0),
            unlit: true,
            double_sided: true,
            cull_mode: None,
            ..default()
        };
    }
    let c = albedo * tint;
    StandardMaterial {
        base_color: Color::linear_rgb(c.x, c.y, c.z),
        perceptual_roughness: 0.92,
        reflectance: 0.2,
        // From inside a puff its faces must still hide what's beyond, as the
        // rule says: draw both sides.
        double_sided: true,
        cull_mode: None,
        ..default()
    }
}

/// The world's axes in the core's frame, as a rotation.
pub(crate) fn frame_rotation(units: &Units) -> Quat {
    Quat::from_mat3(&Mat3::from_cols(
        units.dir_from_internal(Vec3::X),
        units.dir_from_internal(Vec3::Y),
        units.dir_from_internal(Vec3::Z),
    ))
}

/// Where a puff's mesh (inradius 1) is drawn: at the puff, turned as it is
/// and scaled to its inradius. `frame` is [`frame_rotation`].
pub(crate) fn puff_transform(units: &Units, frame: Quat, p: &LivePuff) -> Transform {
    Transform {
        translation: units.from_internal(p.center),
        rotation: frame * Quat::from_mat3(&p.to_local.transpose()),
        scale: Vec3::splat(p.radius / units.metres_per_unit),
    }
}

#[cfg(test)]
impl CrystalClouds {
    /// A cloud's puff entities, in the order of `CloudState::live`.
    pub(crate) fn puffs(&self, id: CloudId) -> &[Entity] {
        self.0.get(&id).map_or(&[], |c| &c.puffs)
    }
}

/// Spawn, move, show and hide the puffs of every crystal cloud at the drawn
/// tick, and despawn those of clouds that are gone.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_crystals(
    mut commands: Commands,
    mut world: ResMut<SmokeWorld>,
    clock: Res<SmokeClock>,
    config: Res<SmokeConfig>,
    settings: Res<SmokeSettings>,
    smokes: Query<&Smoke>,
    meshes: Option<Res<CrystalMeshes>>,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
    mut clouds: ResMut<CrystalClouds>,
    mut puffs: Query<(&mut Transform, &mut GlobalTransform, &mut Visibility), With<CrystalPuff>>,
) {
    let (Some(meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    // No crystal smoke: leave the snapshot to `build_frame`, which runs
    // alongside Bevy's visibility systems.
    if clouds.0.is_empty() && !world.clouds().any(|c| c.spec().style == Style::Crystal) {
        return;
    }
    let units = config.units;
    let frame = frame_rotation(&units);
    let albedo = settings.albedo.to_vec3();
    let tints: HashMap<CloudId, Vec3> = smokes
        .iter()
        .filter_map(|s| s.cloud().map(|id| (id, s.tint.to_linear().to_vec3())))
        .collect();

    let view = world.view(clock.drawn());
    let mut seen: Vec<CloudId> = Vec::new();
    for cloud in view.clouds().filter(|c| c.spec.style == Style::Crystal) {
        let id = cloud.state.id;
        seen.push(id);
        let live = &cloud.state.live;
        let tint = tints.get(&id).copied().unwrap_or(Vec3::ONE);
        let look = (tint, albedo, settings.debug_silhouette);
        let entry = clouds.0.entry(id).or_insert_with(|| CrystalCloud {
            puffs: Vec::new(),
            material: materials.add(crystal_material(look.0, look.1, look.2)),
            look,
            shadows: true,
        });
        if entry.look != look {
            if let Some(mut material) = materials.get_mut(&entry.material) {
                *material = crystal_material(look.0, look.1, look.2);
            }
            entry.look = look;
        }
        if entry.puffs.len() != live.len() {
            for e in entry.puffs.drain(..) {
                commands.entity(e).despawn();
            }
            // Spawned in place, and shown if the rule counts them: these
            // commands land before Bevy's visibility systems run.
            entry.puffs = live
                .iter()
                .map(|p| {
                    let (transform, visibility) = if p.radius > 0.0 {
                        (puff_transform(&units, frame, p), Visibility::Inherited)
                    } else {
                        (Transform::default(), Visibility::Hidden)
                    };
                    let mut e = commands.spawn((
                        CrystalPuff { cloud: id },
                        Mesh3d(meshes.0[p.shape as usize].clone()),
                        MeshMaterial3d(entry.material.clone()),
                        transform,
                        GlobalTransform::from(transform),
                        visibility,
                    ));
                    if !settings.crystal_shadows {
                        e.insert(NotShadowCaster);
                    }
                    e.id()
                })
                .collect();
            entry.shadows = settings.crystal_shadows;
        }
        if entry.shadows != settings.crystal_shadows {
            for &e in &entry.puffs {
                if settings.crystal_shadows {
                    commands.entity(e).remove::<NotShadowCaster>();
                } else {
                    commands.entity(e).insert(NotShadowCaster);
                }
            }
            entry.shadows = settings.crystal_shadows;
        }
        for (p, &e) in live.iter().zip(&entry.puffs) {
            let Ok((mut transform, mut global, mut visibility)) = puffs.get_mut(e) else {
                // Spawned this frame, already in place.
                continue;
            };
            if p.radius <= 0.0 {
                visibility.set_if_neq(Visibility::Hidden);
                continue;
            }
            visibility.set_if_neq(Visibility::Inherited);
            let t = puff_transform(&units, frame, p);
            if *transform != t {
                *transform = t;
                // The plugin runs after transform propagation: keep the
                // global transform in step so the puff draws where it is now.
                *global = GlobalTransform::from(t);
            }
        }
    }
    clouds.0.retain(|id, c| {
        let keep = seen.contains(id);
        if !keep {
            for &e in &c.puffs {
                commands.entity(e).despawn();
            }
            materials.remove(&c.material);
        }
        keep
    });
}
