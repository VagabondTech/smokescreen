//! A fixed test level shared by the tests and the benchmark: the demo's
//! range (a building with a doorway, a corridor, crates and a pillar) on a
//! ground plane.

#![allow(dead_code)]

use smokescreen::glam::Vec3;
use smokescreen::{BoxObstacles, FlatGround, ObstacleBox, Units, UpAxis};

/// Boxes as (min, max), metres, Y up.
pub fn boxes() -> Vec<(Vec3, Vec3)> {
    let h = 4.0;
    vec![
        // Front wall of a building at z = 6, with a 2 m doorway and lintel.
        (Vec3::new(-9.0, 0.0, 5.8), Vec3::new(-1.0, h, 6.2)),
        (Vec3::new(1.0, 0.0, 5.8), Vec3::new(9.0, h, 6.2)),
        (Vec3::new(-1.0, 2.6, 5.8), Vec3::new(1.0, h, 6.2)),
        // Side walls, back wall and roof.
        (Vec3::new(-9.0, 0.0, 6.2), Vec3::new(-8.6, h, 15.0)),
        (Vec3::new(8.6, 0.0, 6.2), Vec3::new(9.0, h, 15.0)),
        (Vec3::new(-9.0, 0.0, 14.6), Vec3::new(9.0, h, 15.0)),
        (Vec3::new(-9.0, h, 5.8), Vec3::new(9.0, h + 0.3, 15.0)),
        // A corridor open at both ends.
        (Vec3::new(11.0, 0.0, -10.0), Vec3::new(11.4, 3.2, 4.0)),
        (Vec3::new(13.8, 0.0, -10.0), Vec3::new(14.2, 3.2, 4.0)),
        // Cover.
        (Vec3::new(-4.6, 0.0, 0.4), Vec3::new(-3.4, 1.2, 1.6)),
        (Vec3::new(-4.4, 1.2, 0.6), Vec3::new(-3.6, 2.0, 1.4)),
        (Vec3::new(3.0, 0.0, -2.5), Vec3::new(4.2, 1.2, -1.3)),
        (Vec3::new(5.5, 0.0, 2.0), Vec3::new(6.3, 3.5, 2.8)),
        (Vec3::new(-0.4, 0.0, 9.6), Vec3::new(0.4, 1.8, 10.4)),
    ]
}

/// The level in metres, Y up.
pub fn level() -> BoxObstacles {
    BoxObstacles::new(boxes().into_iter().map(|(min, max)| ObstacleBox { min, max }).collect())
}

/// The same level in another unit system.
pub fn level_in(units: Units) -> BoxObstacles {
    let corner = |p: Vec3| units.from_internal(p);
    BoxObstacles {
        boxes: boxes()
            .into_iter()
            .map(|(min, max)| {
                let (a, b) = (corner(min), corner(max));
                ObstacleBox {
                    min: a.min(b),
                    max: a.max(b),
                }
            })
            .collect(),
        ground: Some(FlatGround::new(0.0, units.up)),
    }
}

/// Where the test clouds pop (metres, Y up), with seeds.
pub fn pops() -> Vec<(Vec3, u32)> {
    vec![
        (Vec3::new(0.0, 0.0, 4.6), 7),     // in front of the doorway
        (Vec3::new(12.6, 0.0, -2.0), 3),   // in the corridor
        (Vec3::new(-4.0, 0.0, -2.0), 11),  // by the crates
        (Vec3::new(3.0, 0.0, 9.0), 5),     // inside the room
        (Vec3::new(-14.0, 0.0, -8.0), 13), // in the open
        (Vec3::new(5.9, 3.5, 2.4), 17),    // on top of the pillar
        (Vec3::new(0.3, 0.0, -9.0), 19),   // open, south
        (Vec3::new(14.5, 0.0, 6.0), 23),   // by the corridor's end
    ]
}

pub fn z_up_inches() -> Units {
    Units::new(UpAxis::Z, 0.0254)
}

/// A small deterministic random generator (xorshift), for test segments.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 32) as u32
    }

    /// Uniform in lo..hi.
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    pub fn point(&mut self, lo: Vec3, hi: Vec3) -> Vec3 {
        Vec3::new(self.range(lo.x, hi.x), self.range(lo.y, hi.y), self.range(lo.z, hi.z))
    }
}
