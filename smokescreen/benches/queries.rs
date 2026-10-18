//! Timing probe for the brief's budgets (`cargo bench -p smokescreen`):
//!
//! - fill: at most about 0.2 ms of work per tick, the same cloud however the
//!   work is split (more when a fill must hurry to be ready when its cloud
//!   starts);
//! - queries: 250 per tick against 32 clouds, about 4 in the way of each, in
//!   at most 0.2 ms, for every style and rule, while the clouds grow, once
//!   they are grown and while they fade.
//!
//! Prints the best of several runs of each measurement.

#[path = "../tests/common/mod.rs"]
mod common;

use std::hint::black_box;
use std::time::{Duration, Instant};

use smokescreen::glam::Vec3;
use smokescreen::*;

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let out = f();
    (out, t.elapsed())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn main() {
    let level = common::level();
    let voxels = VoxelObstacles::from_level(Vec3::new(-30.0, -1.0, -30.0), Vec3::new(30.0, 8.0, 30.0), 0.3, &level);
    let config = Config::new(Units::METRES_Y_UP, 128.0);

    println!("Fill, all at once (8 clouds in the test level):");
    for (name, lvl) in [("boxes", &level as &dyn Level), ("voxels", &voxels)] {
        for style in [Style::Volumetric, Style::Faceted, Style::Crystal] {
            let mut times = Vec::new();
            for (origin, seed) in common::pops() {
                let spec = CloudSpec::new(origin, 0, seed, style);
                let best = (0..3)
                    .map(|_| time(|| black_box(fill_cloud(&config, &spec, lvl))).1)
                    .min()
                    .unwrap_or_default();
                times.push(best);
            }
            let mean = times.iter().sum::<Duration>() / times.len() as u32;
            let worst = times.iter().max().copied().unwrap_or_default();
            println!(
                "  {name:6} {style:10?} mean {:.2} ms, worst {:.2} ms",
                ms(mean),
                ms(worst)
            );
        }
    }

    println!("Fill spread over ticks ({} units per tick):", config.fill_budget);
    for (name, lvl) in [("boxes", &level as &dyn Level), ("voxels", &voxels)] {
        for style in [Style::Volumetric, Style::Faceted, Style::Crystal] {
            let mut ticks = Vec::new();
            let mut per_tick = Vec::new();
            for (origin, seed) in common::pops() {
                let mut cfg = config.clone();
                // Never forced: measure the budgeted work alone.
                cfg.tuning.timeline.start = 100.0;
                let mut smokes = Smokes::new(cfg, lvl);
                smokes.pop(CloudSpec::new(origin, 0, seed, style));
                let mut tick = 0;
                while smokes.pending_fills() > 0 {
                    let (_, d) = time(|| smokes.advance(tick));
                    per_tick.push(d);
                    tick += 1;
                }
                ticks.push(tick);
            }
            per_tick.sort();
            let p99 = per_tick[per_tick.len() * 99 / 100];
            println!(
                "  {name:6} {style:10?} {}..{} ticks; per tick: median {:.3} ms, 99th percentile {:.3} ms, worst {:.3} ms",
                ticks.iter().min().unwrap_or(&0),
                ticks.iter().max().unwrap_or(&0),
                ms(per_tick[per_tick.len() / 2]),
                ms(p99),
                ms(per_tick[per_tick.len() - 1])
            );
        }
    }

    println!("Fill with the default timeline (the cloud starts 0.1 s after the pop):");
    for style in [Style::Volumetric, Style::Faceted, Style::Crystal] {
        let mut per_tick = Vec::new();
        let mut ticks = Vec::new();
        for (origin, seed) in common::pops() {
            let mut smokes = Smokes::new(config.clone(), &voxels);
            let id = smokes.pop(CloudSpec::new(origin, 0, seed, style));
            let start = smokes.cloud(id).map(|c| c.start_tick()).unwrap_or(0);
            // Up to and including the start tick, which finishes anything left.
            for tick in 0..=start {
                if smokes.pending_fills() == 0 {
                    break;
                }
                let (_, d) = time(|| smokes.advance(tick));
                per_tick.push(d);
                ticks.push(tick + 1);
            }
        }
        per_tick.sort();
        println!(
            "  {style:10?} up to {} ticks; per tick: median {:.3} ms, 99th percentile {:.3} ms, worst {:.3} ms",
            ticks.iter().max().unwrap_or(&0),
            ms(per_tick[per_tick.len() / 2]),
            ms(per_tick[per_tick.len() * 99 / 100]),
            ms(per_tick[per_tick.len() - 1])
        );
    }

    // Seconds after the pop: growing, grown, and three points in the fade
    // (the default timeline fades from 15 s to 18 s).
    const PHASES: [f32; 5] = [0.8, 5.0, 15.5, 16.5, 17.5];
    println!("250 sight checks, 32 clouds (an 8 × 4 grid 12 m apart), each segment crossing 4.");
    println!("Milliseconds (lines blocked) at seconds after the pop:");
    println!("{:19}{}", "", PHASES.map(|s| format!("{s:>13}")).join(""));
    let half = VolumeSteps::HalfVoxel;
    let rules = [
        ("volumetric", Style::Volumetric, FacetRule::Exact, half, 0.8),
        (
            "volumetric, voxel",
            Style::Volumetric,
            FacetRule::Exact,
            VolumeSteps::Voxel,
            0.8,
        ),
        ("faceted, exact", Style::Faceted, FacetRule::Exact, half, 0.8),
        ("faceted, core", Style::Faceted, FacetRule::Core, half, 0.8),
        ("crystal, 0.8 m", Style::Crystal, FacetRule::Exact, half, 0.8),
        ("crystal, touch", Style::Crystal, FacetRule::Exact, half, 0.0),
    ];
    for (name, style, rule, steps, threshold) in rules {
        let mut cfg = config.clone();
        cfg.tuning.facet_rule = rule;
        cfg.tuning.volume_steps = steps;
        cfg.tuning.sight_threshold = threshold;
        let mut smokes = Smokes::new(cfg, FlatGround::default());
        for i in 0..32 {
            let origin = Vec3::new((i % 8) as f32 * 12.0, 0.0, (i / 8) as f32 * 12.0);
            smokes.pop(CloudSpec::new(origin, 0, i as u32 + 1, style));
        }
        // Along a row, from one gap to another four clouds on, at head
        // height or so.
        let mut rng = common::Rng(0x9e37_79b9_7f4a_7c15);
        let through: Vec<(Vec3, Vec3)> = (0..250)
            .map(|_| {
                let row = (rng.next_u32() % 4) as f32 * 12.0;
                let start = (rng.next_u32() % 4) as f32;
                let a = Vec3::new(start * 12.0 - 6.0, rng.range(0.3, 2.5), row + rng.range(-3.0, 3.0));
                let b = Vec3::new(a.x + 48.0, rng.range(0.3, 2.5), row + rng.range(-3.0, 3.0));
                (a, b)
            })
            .collect();
        // The same segments raised over the clouds: their boxes, no cloud.
        let over: Vec<(Vec3, Vec3)> = through
            .iter()
            .map(|(a, b)| (*a + Vec3::new(0.0, 6.0, 0.0), *b + Vec3::new(0.0, 6.0, 0.0)))
            .collect();

        let mut line = format!("  {name:17}");
        let mut extra = String::new();
        let mut next = 0;
        for seconds in PHASES {
            let tick = (seconds * 128.0) as u64;
            while next <= tick {
                smokes.advance(next);
                next += 1;
            }
            let (_, state) = time(|| {
                smokes.view(tick);
            });
            let view = smokes.view(tick);
            let best = |f: &dyn Fn() -> usize| {
                let mut best = Duration::MAX;
                let mut out = 0;
                for _ in 0..20 {
                    let (o, d) = time(f);
                    best = best.min(d);
                    out = o;
                }
                (out, best)
            };
            let (blocked, blocks) = best(&|| through.iter().filter(|(a, b)| view.blocks_sight(*a, *b)).count());
            line += &format!(" {:7.3} ({blocked:3})", ms(blocks));
            if seconds == 5.0 {
                let (_, clear) = best(&|| over.iter().filter(|(a, b)| view.blocks_sight(*a, *b)).count());
                extra = format!(
                    "{:19}grown: per-tick state {:.3} ms, the same checks over the clouds {:.3} ms",
                    "",
                    ms(state),
                    ms(clear)
                );
            }
        }
        println!("{line}");
        println!("{extra}");
    }
}
