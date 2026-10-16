//! A headless server loop: Z-up inches, a 128 Hz tick, the game's own
//! collision behind the `Level` trait, smokes popped and shot through as
//! events with their tick, and bots asking who can see whom.
//!
//! `cargo run --release -p smokescreen --example server`

use std::time::Instant;

use smokescreen::glam::Vec3;
use smokescreen::*;

/// Stand-in for the game's brush collision: axis-aligned boxes in inches,
/// Z up, plus a floor at z = 0.
struct Brushes(Vec<(Vec3, Vec3)>);

impl Level for Brushes {
    fn is_blocked(&self, center: Vec3, half: f32) -> bool {
        center.z < half * 0.25
            || self
                .0
                .iter()
                .any(|(lo, hi)| (center + half * 0.8).cmpgt(*lo).all() && (center - half * 0.8).cmplt(*hi).all())
    }

    fn ray_hit(&self, origin: Vec3, dir: Vec3, max: f32) -> bool {
        // Only renderers ask this (for lighting); a server never does.
        let _ = (origin, dir, max);
        false
    }
}

fn main() {
    let units = Units::INCHES_Z_UP;
    let mut config = Config::new(units, 128.0);
    // The game's rules, in its own units: 32 u of cloud blocks sight.
    config.tuning.sight_threshold = 32.0;
    // A wall 200 u in front of the spawn with a doorway, as boxes (min, max).
    let level = Brushes(vec![
        (Vec3::new(-400.0, 190.0, 0.0), Vec3::new(-40.0, 210.0, 160.0)),
        (Vec3::new(40.0, 190.0, 0.0), Vec3::new(400.0, 210.0, 160.0)),
        (Vec3::new(-40.0, 190.0, 100.0), Vec3::new(40.0, 210.0, 160.0)),
    ]);
    let mut smokes = Smokes::new(config, level);

    // Bots: eyes at 64 u, looking at enemies across the map.
    let bots: Vec<(Vec3, Vec3)> = (0..250)
        .map(|i| {
            let x = (i % 25) as f32 * 30.0 - 360.0;
            let y = (i / 25) as f32 * 40.0 - 400.0;
            (Vec3::new(x, -600.0, 64.0), Vec3::new(-x * 0.5, y + 900.0, 60.0))
        })
        .collect();

    let mut advance_ms = Vec::new();
    let mut query_ms = Vec::new();
    let mut blocked_counts = Vec::new();
    for tick in 0..(25 * 128) {
        // Events the game simulation produced this tick.
        if tick == 64 {
            smokes.pop(CloudSpec::new(
                Vec3::new(0.0, 150.0, 0.0),
                tick,
                1234,
                Style::Volumetric,
            ));
        }
        if tick == 700 {
            smokes.pop(CloudSpec::new(
                Vec3::new(-200.0, -100.0, 0.0),
                tick,
                99,
                Style::Volumetric,
            ));
        }
        if tick == 1500 {
            smokes.bullet(Vec3::new(0.0, -600.0, 64.0), Vec3::new(0.0, 900.0, 60.0), tick);
        }

        let t = Instant::now();
        smokes.advance(tick);
        advance_ms.push(t.elapsed().as_secs_f64() * 1e3);

        let t = Instant::now();
        let view = smokes.view(tick);
        let blocked = bots
            .iter()
            .filter(|(eye, enemy)| view.blocks_sight(*eye, *enemy))
            .count();
        query_ms.push(t.elapsed().as_secs_f64() * 1e3);
        if tick % 256 == 0 {
            blocked_counts.push((tick, blocked));
        }
    }
    for (tick, blocked) in blocked_counts {
        println!("tick {tick:5}: {blocked:3} of 250 sight lines blocked");
    }
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        (v[v.len() / 2], v[v.len() * 99 / 100], v[v.len() - 1])
    };
    let (a50, a99, amax) = stats(&mut advance_ms);
    let (q50, q99, qmax) = stats(&mut query_ms);
    println!("advance per tick: median {a50:.3} ms, 99th percentile {a99:.3} ms, worst {amax:.3} ms");
    println!("250 queries per tick: median {q50:.3} ms, 99th percentile {q99:.3} ms, worst {qmax:.3} ms");
}
