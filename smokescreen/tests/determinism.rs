//! The brief's determinism test: fill 8 clouds in a fixed test level (all
//! three styles, a smoke wall and a filled box among them), run 10,000 seeded
//! segment queries over their lifetimes (with bullets and explosions along
//! the way) and hash every result; in metres, in Z-up inches, and once more
//! with the faceted core rule. The hash must be the
//! same on every platform the game ships on: run this in CI on each.

mod common;

use smokescreen::glam::Vec3;
use smokescreen::*;

/// Recorded on x86_64-unknown-linux-gnu. Any platform (or optimisation level,
/// or CPU feature set) that differs breaks lockstep.
const EXPECTED: u64 = 0xf5b2_932d_9a1a_1419;

struct Fnv(u64);

impl Fnv {
    fn word(&mut self, w: u32) {
        for b in w.to_le_bytes() {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn vec(&mut self, v: Vec3) {
        self.word(v.x.to_bits());
        self.word(v.y.to_bits());
        self.word(v.z.to_bits());
    }
}

fn run(units: Units, rule: FacetRule, hash: &mut Fnv) -> u32 {
    let mut config = Config::new(units, 128.0);
    config.tuning.facet_rule = rule;
    let mut smokes = Smokes::new(config, common::level_in(units));
    let pops = common::pops();
    let mut ids = Vec::new();
    for (i, (origin, seed)) in pops.iter().enumerate() {
        let style = [Style::Faceted, Style::Volumetric, Style::Crystal][i % 3];
        let mut spec = CloudSpec::new(units.from_internal(*origin), 37 * i as u64, *seed, style);
        // A smoke wall and a filled box among the grenades.
        if i == 4 {
            spec.source = CloudSource::Line {
                end: units.from_internal(*origin + Vec3::new(6.0, 0.5, 2.0)),
            };
        }
        if i == 6 {
            let half = units.dir_from_internal(Vec3::new(2.5, 1.0, 1.5)) / units.metres_per_unit;
            spec.source = CloudSource::Box { half_extents: half };
        }
        ids.push(smokes.pop(spec));
    }
    let mut rng = common::Rng(0x5eed_1234_abcd_ef01);
    let lo = Vec3::new(-18.0, 0.0, -12.0);
    let hi = Vec3::new(18.0, 4.5, 16.0);
    let mut queries = 0;
    let end = 21 * 128;
    for tick in 0..end {
        smokes.advance(tick);
        if tick % 61 == 0 {
            let a = units.from_internal(rng.point(lo, hi));
            let b = units.from_internal(rng.point(lo, hi));
            smokes.bullet(a, b, tick);
        }
        if tick % 397 == 100 {
            let c = units.from_internal(rng.point(lo, hi));
            smokes.explosion(c, None, tick);
        }
        if tick % 8 == 0 {
            // 10,000 queries over the run: about 30 every 8 ticks.
            for _ in 0..30 {
                if queries == 10_000 {
                    break;
                }
                let a = units.from_internal(rng.point(lo, hi));
                let b = units.from_internal(rng.point(lo, hi));
                hash.word(smokes.occlusion(a, b, tick).to_bits());
                hash.word(smokes.blocks_sight(a, b, tick) as u32);
                hash.word(smokes.density_at(a, tick).to_bits());
                queries += 1;
            }
        }
    }
    // The clouds themselves.
    for id in ids {
        let Some(cloud) = smokes.cloud(id) else {
            continue;
        };
        let Some(shape) = cloud.shape() else {
            continue;
        };
        for &b in shape.grid.distance.iter().chain(&shape.grid.region) {
            hash.word(b as u32);
        }
        if let Some(f) = &shape.facets {
            for p in &f.puffs {
                hash.vec(p.center);
                hash.vec(p.axis);
                hash.word(p.radius.to_bits());
                hash.word(p.room.to_bits());
                hash.word(p.spin.to_bits());
                hash.word(p.phase.to_bits());
                hash.word(p.shape as u32);
            }
        }
    }
    queries
}

#[test]
fn determinism_hash() {
    let mut hash = Fnv(0xcbf2_9ce4_8422_2325);
    let n = run(Units::METRES_Y_UP, FacetRule::Exact, &mut hash);
    let m = run(common::z_up_inches(), FacetRule::Exact, &mut hash);
    let c = run(Units::METRES_Y_UP, FacetRule::Core, &mut hash);
    assert_eq!((n, m, c), (10_000, 10_000, 10_000));
    println!("determinism hash: {:#018x}", hash.0);
    if EXPECTED != 0 {
        assert_eq!(hash.0, EXPECTED, "results differ from the recorded platform");
    }
}

/// Recorded on x86_64-unknown-linux-gnu, like [`EXPECTED`].
const EXPECTED_MESHES: u64 = 0x5eef_51a1_bce5_3bc4;

/// Voxelising triangles (what `SmokeObstacle` does on every machine that
/// doesn't load shipped bytes) gives the same voxels everywhere, and so the
/// same clouds: the test level's boxes as meshes facing outwards, plus a
/// few turned to an angle, voxelised and filled.
#[test]
fn voxelised_meshes_are_the_same_everywhere() {
    let mut triangles = Vec::new();
    // Turned about y by an angle with an exact cosine (0.8) and sine (0.6).
    let turn = |v: Vec3| Vec3::new(0.8 * v.x - 0.6 * v.z, v.y, 0.6 * v.x + 0.8 * v.z);
    for (k, (lo, hi)) in common::boxes().into_iter().enumerate() {
        let p = |i: usize| {
            let v = Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            );
            if k % 4 == 3 { turn(v) } else { v }
        };
        // Faces as corner indices, counter-clockwise seen from outside.
        for f in [
            [0, 4, 6, 2],
            [1, 3, 7, 5],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 2, 3, 1],
            [4, 5, 7, 6],
        ] {
            triangles.push([p(f[0]), p(f[1]), p(f[2])]);
            triangles.push([p(f[0]), p(f[2]), p(f[3])]);
        }
    }
    // A floor 3 cm above the voxel lattice.
    for t in [
        [
            Vec3::new(-30.0, 0.03, -30.0),
            Vec3::new(-30.0, 0.03, 30.0),
            Vec3::new(30.0, 0.03, 30.0),
        ],
        [
            Vec3::new(-30.0, 0.03, -30.0),
            Vec3::new(30.0, 0.03, 30.0),
            Vec3::new(30.0, 0.03, -30.0),
        ],
    ] {
        triangles.push(t);
    }
    let voxels = VoxelObstacles::from_triangles(&triangles, 0.3, 1.2, UpAxis::Y);
    let mut hash = Fnv(0xcbf2_9ce4_8422_2325);
    for chunk in voxels.to_bytes().chunks(4) {
        let mut w = [0u8; 4];
        w[..chunk.len()].copy_from_slice(chunk);
        hash.word(u32::from_le_bytes(w));
    }
    let config = Config::new(Units::METRES_Y_UP, 128.0);
    let mut padded = 0;
    for (origin, seed) in common::pops() {
        let shape = fill_cloud(&config, &CloudSpec::new(origin, 0, seed, Style::Volumetric), &voxels);
        for (&d, &o) in shape.grid.distance.iter().zip(&shape.grid.occupancy) {
            hash.word(d as u32 | (o as u32) << 8);
            padded += (o == grid::occupancy::PADDED) as u32;
        }
    }
    println!("mesh determinism hash: {:#018x} ({padded} padded voxels)", hash.0);
    assert!(padded > 100, "only {padded} padded voxels");
    if EXPECTED_MESHES != 0 {
        assert_eq!(
            hash.0, EXPECTED_MESHES,
            "voxels or clouds differ from the recorded platform"
        );
    }
}
