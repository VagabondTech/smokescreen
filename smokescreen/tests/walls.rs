//! Smoke against walls and floors: dense right up to them wherever they sit
//! on the voxel lattice, no more than half a voxel of smoke past a wall
//! thinner than a voxel (at any angle), and a voxelised level that answers
//! exactly like the level it was built from.

mod common;

use smokescreen::glam::Vec3;
use smokescreen::*;

fn config() -> Config {
    Config::new(Units::METRES_Y_UP, 128.0)
}

/// A corridor along z: a thick wall on the right whose face is at `face`
/// (voxels are 0.3 m, on a lattice from the origin, so 2.1 is a voxel face
/// and 2.07 clips 3 cm off a voxel), a 10 cm wall on the left whose near face
/// is at `thin_x`, and a floor at `floor` (0 is a voxel face).
fn corridor(face: f32, thin_x: f32, floor: f32) -> BoxObstacles {
    let mut boxes = vec![
        ObstacleBox {
            min: Vec3::new(face, -1.0, -9.0),
            max: Vec3::new(face + 0.6, 3.0, 9.0),
        },
        ObstacleBox {
            min: Vec3::new(thin_x - 0.1, -1.0, -9.0),
            max: Vec3::new(thin_x, 3.0, 9.0),
        },
    ];
    if floor > 0.0 {
        boxes.push(ObstacleBox {
            min: Vec3::new(-9.0, -1.0, -9.0),
            max: Vec3::new(9.0, floor, 9.0),
        });
    }
    BoxObstacles::new(boxes)
}

/// Smoke pressed against a wall or floor used to fade out over the last
/// voxel before it, then stop at the middle of the first solid voxel, which
/// could be up to 12 cm short of a wall that only clips it: a see-through
/// layer along the surface. Samples skip solid voxels, and the fill pads
/// clipped ones, so a sight line 1 cm off the surface sees as much smoke as
/// one well inside the cloud, along walls, floors and the edge where they
/// meet, wherever they sit on the lattice.
#[test]
fn smoke_is_dense_right_up_to_walls_and_floors() {
    for (face, floor) in [
        (2.1, 0.0),
        (2.07, 0.0),
        (2.04, 0.0),
        (2.0, 0.0),
        (1.96, 0.0),
        (2.07, 0.04),
        (2.04, 0.1),
        (2.0, 0.14),
    ] {
        let level = corridor(face, -2.1, floor);
        let mut smokes = Smokes::new(config(), &level);
        smokes.pop(CloudSpec::new(Vec3::new(0.0, floor, 0.0), 0, 7, Style::Volumetric));
        let view = smokes.view(5 * 128);
        let along = |x: f32, y: f32| (Vec3::new(x, floor + y, -6.0), Vec3::new(x, floor + y, 6.0));
        for (name, near, inside) in [
            ("wall", along(face - 0.01, 1.0), along(face - 0.3, 1.0)),
            ("thin wall", along(-2.09, 1.0), along(-1.8, 1.0)),
            ("floor", along(0.0, 0.01), along(0.0, 0.3)),
            ("wall and floor", along(face - 0.01, 0.01), along(face - 0.3, 0.3)),
        ] {
            let (n, i) = (view.occlusion(near.0, near.1), view.occlusion(inside.0, inside.1));
            assert!(
                n > 2.0 && n >= 0.95 * i,
                "wall at {face}, floor at {floor}, {name}: {n} m of cloud 1 cm off the surface, {i} m further in"
            );
            assert!(
                view.blocks_sight(near.0, near.1),
                "wall at {face}, floor at {floor}, {name}: sight line along the surface not blocked"
            );
        }
    }
}

/// A doorway through a 40 cm wall, its jambs and lintel off the lattice, a
/// cloud in front of it: the smoke in the doorway is dense right up to the
/// jambs and the lintel. Next to the door frame's edges the air past them
/// is empty (nothing on the far side), which the padding must tell apart
/// from air past a thin wall.
#[test]
fn smoke_fills_a_doorway_right_up_to_its_frame() {
    let (z0, z1) = (5.83, 6.23);
    let wall = |min: Vec3, max: Vec3| ObstacleBox { min, max };
    let level = BoxObstacles::new(vec![
        wall(Vec3::new(-9.0, 0.0, z0), Vec3::new(-0.97, 4.0, z1)),
        wall(Vec3::new(1.03, 0.0, z0), Vec3::new(9.0, 4.0, z1)),
        wall(Vec3::new(-0.97, 2.03, z0), Vec3::new(1.03, 4.0, z1)),
    ]);
    let mut smokes = Smokes::new(config(), &level);
    smokes.pop(CloudSpec::new(Vec3::new(0.0, 0.0, 4.6), 0, 7, Style::Volumetric));
    let view = smokes.view(5 * 128);
    // Through the doorway, from one face of the wall to the other.
    let through = |x: f32, y: f32| view.occlusion(Vec3::new(x, y, z0 + 0.01), Vec3::new(x, y, z1 - 0.01));
    let middle = through(0.03, 1.0);
    assert!(middle > 0.35, "{middle} m of cloud through the middle of the doorway");
    for (name, amount) in [
        ("left jamb", through(-0.96, 1.0)),
        ("right jamb", through(1.02, 1.0)),
        ("lintel", through(0.03, 2.02)),
    ] {
        assert!(
            amount >= 0.95 * middle,
            "{amount} m of cloud through the doorway 1 cm from its {name}, {middle} m through the middle"
        );
    }
}

/// The smoke stops at the middle of the first solid voxel, so past a wall
/// thinner than a voxel there is never more than half a voxel of it,
/// wherever the wall sits on the voxel lattice. (Clipped voxels are only
/// padded when the voxel behind them is inside the wall.)
#[test]
fn smoke_goes_at_most_half_a_voxel_past_a_thin_wall() {
    for k in 0..12 {
        let thin_x = -2.05 - 0.025 * k as f32;
        let level = corridor(2.1, thin_x, 0.0);
        let mut smokes = Smokes::new(config(), &level);
        smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 7, Style::Volumetric));
        let view = smokes.view(5 * 128);
        let far_face = thin_x - 0.1;
        let mut worst = 0.0f32;
        for i in 0..40 {
            for j in 0..12 {
                let p = Vec3::new(
                    far_face - 0.15 - 0.02 * i as f32,
                    0.1 + 0.2 * j as f32,
                    -2.0 + 0.1 * i as f32,
                );
                worst = worst.max(view.density_at(p));
            }
        }
        assert_eq!(worst, 0.0, "smoke more than half a voxel past the wall at x = {thin_x}");
        // And the near side is full.
        let (a, b) = (Vec3::new(thin_x + 0.3, 1.0, -6.0), Vec3::new(thin_x + 0.3, 1.0, 6.0));
        assert!(view.blocks_sight(a, b), "wall at {thin_x}: no smoke in front of it");
    }
}

/// A wall `thick` thick at an angle to the grid: everything with
/// `d0 < n·p < d0 + thick` (n unit length), on a floor at 0. Blocks a
/// voxel like `BoxObstacles` does (a cube shrunk to 80 %).
struct Slab {
    n: Vec3,
    d0: f32,
    thick: f32,
}

impl Level for Slab {
    fn is_blocked(&self, c: Vec3, half: f32) -> bool {
        let reach = half * 0.8 * (self.n.x.abs() + self.n.y.abs() + self.n.z.abs());
        let s = self.n.dot(c);
        (s + reach > self.d0 && s - reach < self.d0 + self.thick) || c.y < half * 0.25
    }

    fn is_inside(&self, p: Vec3) -> bool {
        let s = self.n.dot(p);
        (s > self.d0 && s < self.d0 + self.thick) || p.y < 0.0
    }

    fn ray_hit(&self, _: Vec3, _: Vec3, _: f32) -> bool {
        false
    }
}

/// The same for thin walls at an angle to the grid, where a padded voxel's
/// smoke could otherwise reach diagonally past the wall; and thick ones at
/// the same angles are dense right up to their face.
#[test]
fn walls_at_an_angle_to_the_grid() {
    // Exact unit normals: 0°, about 16°, 37°, 45°, 53°.
    let normals = [
        Vec3::X,
        Vec3::new(0.96, 0.0, 0.28),
        Vec3::new(0.8, 0.0, 0.6),
        Vec3::new(core::f32::consts::FRAC_1_SQRT_2, 0.0, core::f32::consts::FRAC_1_SQRT_2),
        Vec3::new(0.6, 0.0, 0.8),
    ];
    for n in normals {
        let along = Vec3::new(-n.z, 0.0, n.x);
        for k in 0..12 {
            let d0 = 1.9 + 0.025 * k as f32;
            // Thin: nothing half a voxel or more past its far face.
            let thin = Slab { n, d0, thick: 0.1 };
            let mut smokes = Smokes::new(config(), &thin);
            smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 7, Style::Volumetric));
            let view = smokes.view(5 * 128);
            let mut worst = 0.0f32;
            for i in 0..80 {
                for j in 0..8 {
                    let p = n * (d0 + 0.1 + 0.15 + 0.01 * (i % 4) as f32)
                        + along * (-2.0 + 0.05 * i as f32)
                        + Vec3::Y * (0.2 + 0.3 * j as f32);
                    worst = worst.max(view.density_at(p));
                }
            }
            assert_eq!(worst, 0.0, "smoke half a voxel past a thin wall, normal {n}, at {d0}");
            // Thick: a 3 m sight line 1 cm off its face sees about 3 m of cloud.
            let thick = Slab { n, d0, thick: 1.0 };
            let mut smokes = Smokes::new(config(), &thick);
            smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 7, Style::Volumetric));
            let view = smokes.view(5 * 128);
            let base = n * (d0 - 0.01) + Vec3::Y * 1.0;
            let amount = view.occlusion(base - along * 1.5, base + along * 1.5);
            assert!(amount > 2.7, "{amount} m of cloud along a wall, normal {n}, at {d0}");
        }
    }
}

/// Thin walls leaning over a floor, smoke on the side they lean away from:
/// where the wall meets the floor, a voxel past the wall can touch a filled
/// voxel along an edge. Still nothing half a voxel past the wall.
#[test]
fn thin_walls_leaning_over_a_floor() {
    let normals = [
        Vec3::new(0.6, -0.8, 0.0),
        Vec3::new(0.8, -0.6, 0.0),
        Vec3::new(0.48, -0.6, 0.64),
        Vec3::new(0.64, -0.6, 0.48),
        Vec3::new(0.36, -0.48, 0.8),
    ];
    for n in normals {
        for k in 0..12 {
            let d0 = 1.3 + 0.025 * k as f32;
            let wall = Slab { n, d0, thick: 0.1 };
            let mut smokes = Smokes::new(config(), &wall);
            smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 7, Style::Volumetric));
            let view = smokes.view(5 * 128);
            for i in 0..45 {
                for j in 0..20 {
                    for l in 0..45 {
                        let p = Vec3::new(-4.5 + 0.2 * i as f32, 0.01 + 0.05 * j as f32, -4.5 + 0.2 * l as f32);
                        if n.dot(p) - (d0 + 0.1) >= 0.15 {
                            let d = view.density_at(p);
                            assert_eq!(
                                d, 0.0,
                                "smoke half a voxel past a wall, normal {n}, at {d0}: {d} at {p}"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Smoke along a wall that only clips voxels isn't lit as if inside the
/// wall: a voxelised level answers rays from a padded voxel's own cell as
/// blocked, so padded voxels take their neighbours' light instead.
#[test]
fn padded_voxels_are_lit_like_their_neighbours() {
    let wall = BoxObstacles::new(vec![ObstacleBox {
        min: Vec3::new(2.07, 0.0, -9.0),
        max: Vec3::new(2.67, 6.0, 9.0),
    }]);
    let voxels = VoxelObstacles::from_level(Vec3::splat(-12.0), Vec3::splat(12.0), 0.3, &wall);
    let shape = fill_cloud(&config(), &CloudSpec::new(Vec3::ZERO, 0, 7, Style::Volumetric), &voxels);
    let grid = &shape.grid;
    // Sun from above, on the smoke's side of the wall: nothing shades it.
    let rays = light::LightRays {
        sun: Some(Vec3::new(-0.3, 0.9, 0.2).normalize()),
        sun_length: 30.0,
        sky_length: 0.0,
    };
    let lit = light::visibility(grid, &voxels, &Units::METRES_Y_UP, &rays);
    let mut near = 0;
    for (i, sun) in lit.iter().map(|l| l[0]).enumerate() {
        let d = grid.dims;
        let c = smokescreen::glam::UVec3::new(i as u32 % d.x, (i as u32 / d.x) % d.y, i as u32 / (d.x * d.y));
        let centre = grid.min + (c.as_vec3() + 0.5) * grid.voxel_size;
        let filled = grid.distance[i] != grid::EMPTY_VOXEL;
        if filled && centre.y > 0.0 && centre.x > 2.07 - 0.7 {
            near += 1;
            assert_eq!(sun, 255, "smoke at {centre} by the wall in shade");
        }
    }
    assert!(near > 100, "only {near} voxels by the wall");
}

/// A box as triangles facing outwards (counter-clockwise seen from
/// outside, like Bevy's `Cuboid`), or inwards.
fn cuboid(lo: Vec3, hi: Vec3, outwards: bool) -> Vec<[Vec3; 3]> {
    let p = |x: bool, y: bool, z: bool| {
        Vec3::new(
            if x { hi.x } else { lo.x },
            if y { hi.y } else { lo.y },
            if z { hi.z } else { lo.z },
        )
    };
    let (f, t) = (false, true);
    let faces = [
        [p(f, f, f), p(f, f, t), p(f, t, t), p(f, t, f)], // -x
        [p(t, f, f), p(t, t, f), p(t, t, t), p(t, f, t)], // +x
        [p(f, f, f), p(t, f, f), p(t, f, t), p(f, f, t)], // -y
        [p(f, t, f), p(f, t, t), p(t, t, t), p(t, t, f)], // +y
        [p(f, f, f), p(f, t, f), p(t, t, f), p(t, f, f)], // -z
        [p(f, f, t), p(t, f, t), p(t, t, t), p(f, t, t)], // +z
    ];
    let mut out = Vec::new();
    for [a, b, c, d] in faces {
        if outwards {
            out.extend([[a, b, c], [a, c, d]]);
        } else {
            out.extend([[a, c, b], [a, d, c]]);
        }
    }
    out
}

/// Swap x and z (a mirror, so the triangles' winding turns round too).
fn swapped(tris: Vec<[Vec3; 3]>) -> Vec<[Vec3; 3]> {
    let s = |v: Vec3| Vec3::new(v.z, v.y, v.x);
    tris.into_iter().map(|[a, b, c]| [s(a), s(c), s(b)]).collect()
}

/// The same for a level made of meshes (`VoxelObstacles::from_triangles`,
/// which is what `SmokeObstacle` builds): it voxelises only surfaces, so
/// the triangles' facing says which side is solid. Walls facing every way,
/// on and off the lattice, and a floor.
#[test]
fn smoke_is_dense_right_up_to_mesh_walls() {
    for swap in [false, true] {
        for (face, floor) in [(2.1, 0.0), (2.07, 0.0), (2.0, 0.05), (1.96, 0.12)] {
            let mut tris = cuboid(Vec3::new(face, -0.5, -9.0), Vec3::new(face + 0.6, 3.0, 9.0), true);
            tris.extend(cuboid(
                Vec3::new(-face - 0.6, -0.5, -9.0),
                Vec3::new(-face, 3.0, 9.0),
                true,
            ));
            tris.extend(cuboid(Vec3::new(-9.0, -0.5, -9.0), Vec3::new(9.0, floor, 9.0), true));
            let at = |v: Vec3| if swap { Vec3::new(v.z, v.y, v.x) } else { v };
            let level = VoxelObstacles::from_triangles(&if swap { swapped(tris) } else { tris }, 0.3, 1.2, UpAxis::Y);
            let mut smokes = Smokes::new(config(), &level);
            smokes.pop(CloudSpec::new(Vec3::new(0.0, floor, 0.0), 0, 7, Style::Volumetric));
            let view = smokes.view(5 * 128);
            let along = |x: f32, y: f32| (at(Vec3::new(x, floor + y, -6.0)), at(Vec3::new(x, floor + y, 6.0)));
            for (name, near, inside) in [
                ("wall facing -x", along(face - 0.01, 1.0), along(face - 0.3, 1.0)),
                ("wall facing +x", along(-face + 0.01, 1.0), along(-face + 0.3, 1.0)),
                ("floor", along(0.0, 0.01), along(0.0, 0.3)),
                ("wall and floor", along(face - 0.01, 0.01), along(face - 0.3, 0.3)),
            ] {
                let (n, i) = (view.occlusion(near.0, near.1), view.occlusion(inside.0, inside.1));
                assert!(
                    n > 2.0 && n >= 0.95 * i,
                    "swapped {swap}, walls at ±{face}, floor at {floor}, {name}: {n} m of cloud 1 cm off it, {i} m further in"
                );
            }
        }
    }
}

/// Mesh walls thinner than a voxel let no more smoke past them than box
/// walls do; neither do boxes whose triangles face inwards (they just aren't
/// filled up to), nor single sheets, facing the smoke or away from it (a
/// partition made of a plane: no inside).
#[test]
fn smoke_goes_at_most_half_a_voxel_past_thin_or_inside_out_mesh_walls() {
    for (thick, outwards) in [(0.1, true), (0.6, false), (0.1, false), (0.0, true), (0.0, false)] {
        for k in 0..12 {
            let near_face = -2.05 - 0.025 * k as f32;
            let far_face = near_face - thick;
            let mut tris = if thick > 0.0 {
                cuboid(
                    Vec3::new(far_face, -0.5, -9.0),
                    Vec3::new(near_face, 3.0, 9.0),
                    outwards,
                )
            } else {
                // Facing the smoke (+x), or away.
                let (a, b, c, d) = (
                    Vec3::new(near_face, -0.5, -9.0),
                    Vec3::new(near_face, -0.5, 9.0),
                    Vec3::new(near_face, 3.0, 9.0),
                    Vec3::new(near_face, 3.0, -9.0),
                );
                if outwards {
                    vec![[a, c, b], [a, d, c]]
                } else {
                    vec![[a, b, c], [a, c, d]]
                }
            };
            tris.extend(cuboid(Vec3::new(-9.0, -0.5, -9.0), Vec3::new(9.0, 0.0, 9.0), true));
            let level = VoxelObstacles::from_triangles(&tris, 0.3, 1.2, UpAxis::Y);
            let mut smokes = Smokes::new(config(), &level);
            smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 7, Style::Volumetric));
            let view = smokes.view(5 * 128);
            let mut worst = 0.0f32;
            for i in 0..40 {
                for j in 0..12 {
                    let p = Vec3::new(
                        far_face - 0.15 - 0.02 * i as f32,
                        0.1 + 0.2 * j as f32,
                        -2.0 + 0.1 * i as f32,
                    );
                    worst = worst.max(view.density_at(p));
                }
            }
            assert_eq!(
                worst,
                0.0,
                "{thick} m wall facing {}: smoke half a voxel past it at x = {near_face}",
                if outwards { "out" } else { "in" }
            );
        }
    }
}

/// The voxelised level is exact even where a wall's face lies exactly on
/// a voxel's centre, or on the edge of the shrunk cube `BoxObstacles` tests
/// (a voxel centre plus 0.4 voxels), where the slightest difference in how
/// the two compute a voxel's centre would flip the answer. Faces across
/// both horizontal axes, in metres and in Z-up inches (where one of them
/// runs backwards in the internal frame).
#[test]
fn voxelised_level_is_exact_on_voxel_boundaries() {
    for units in [Units::METRES_Y_UP, common::z_up_inches()] {
        let config = Config::new(units, 128.0);
        let v = config.tuning.voxel_size;
        let up = units.up_vector();
        let metre = 1.0 / units.metres_per_unit;
        let k = (2.0 * metre / v).round();
        let centre = (k + 0.5) * v;
        let edge = v * 0.5 * 0.8;
        for across in [Vec3::X, up.cross(Vec3::X).abs()] {
            let along = Vec3::ONE - across - up;
            for face in [
                centre,
                centre + edge,
                centre - edge,
                -centre,
                -centre + edge,
                -centre - edge,
            ] {
                // A wall 1 m thick from the face outwards, on a floor.
                let (lo, hi) = if face > 0.0 {
                    (face, face + metre)
                } else {
                    (face - metre, face)
                };
                let wall = ObstacleBox {
                    min: across * lo - along * 9.0 * metre,
                    max: across * hi + along * 9.0 * metre + up * 3.0 * metre,
                };
                let source = BoxObstacles {
                    boxes: vec![wall],
                    ground: Some(FlatGround::new(0.0, units.up)),
                };
                let reach = 12.0 * metre;
                let voxels = VoxelObstacles::from_level(Vec3::splat(-reach), Vec3::splat(reach), v, &source);
                for seed in 0..4u32 {
                    let side = if face > 0.0 { 1.0 } else { -1.0 };
                    let pop = across * (face - side * (1.2 + 0.1 * seed as f32) * metre)
                        + along * (0.3 * seed as f32 * metre);
                    let spec = CloudSpec::new(pop, 0, seed, Style::Volumetric);
                    assert!(
                        fill_cloud(&config, &spec, &source) == fill_cloud(&config, &spec, &voxels),
                        "{units:?}: face at {face} across {across}, cloud {seed} differs with the voxelised level"
                    );
                }
            }
        }
    }
}

/// Cloud grids lie on the world voxel lattice (multiples of the voxel size
/// from the origin, in the caller's units and frame), so with the tuning's
/// voxel size every cloud voxel is exactly one cell of a `VoxelObstacles`,
/// which asked its source level the same questions when it was built: the
/// clouds come out identical, padding against walls included. In metres,
/// and in Z-up inches.
#[test]
fn voxelised_level_gives_the_same_clouds_as_its_source() {
    let mut padded = 0;
    for units in [Units::METRES_Y_UP, common::z_up_inches()] {
        let config = Config::new(units, 128.0);
        let source = common::level_in(units);
        let (a, b) = (
            units.from_internal(Vec3::new(-30.0, -2.0, -30.0)),
            units.from_internal(Vec3::new(30.0, 10.0, 30.0)),
        );
        let voxels = VoxelObstacles::from_level(a.min(b), a.max(b), config.tuning.voxel_size, &source);
        for (i, (origin, seed)) in common::pops().into_iter().enumerate() {
            let style = [Style::Volumetric, Style::Faceted, Style::Crystal][i % 3];
            let spec = CloudSpec::new(units.from_internal(origin), 0, seed, style);
            let direct = fill_cloud(&config, &spec, &source);
            let voxelised = fill_cloud(&config, &spec, &voxels);
            assert!(
                direct == voxelised,
                "{units:?}: cloud {i} differs with the voxelised level"
            );
            padded += direct
                .grid
                .occupancy
                .iter()
                .filter(|&&o| o == grid::occupancy::PADDED)
                .count();
        }
    }
    // The test level's walls clip voxels, so this compared padded clouds.
    assert!(padded > 50, "only {padded} padded voxels");
}
