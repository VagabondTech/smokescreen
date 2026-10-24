//! The crystal style, the faceted core rule, cloud sources, the cloud limit
//! and pop order.

mod common;

use smokescreen::glam::Vec3;
use smokescreen::*;

fn config() -> Config {
    Config::new(Units::METRES_Y_UP, 128.0)
}

/// Sampled length of `a..b` where `density_at` says there is cloud.
fn sampled_amount(view: &View, a: Vec3, b: Vec3, step: f32) -> f32 {
    let len = a.distance(b);
    let n = (len / step).ceil() as usize;
    let dt = len / n as f32;
    (0..n)
        .map(|i| {
            let p = a.lerp(b, (i as f32 + 0.5) / n as f32);
            view.density_at(p).min(1.0) * dt
        })
        .sum()
}

fn segments(seed: u64, n: usize, lo: Vec3, hi: Vec3) -> Vec<(Vec3, Vec3)> {
    let mut rng = common::Rng(seed);
    (0..n).map(|_| (rng.point(lo, hi), rng.point(lo, hi))).collect()
}

/// A crystal cloud's amount is exactly the length inside its polyhedra.
#[test]
fn crystal_amount_is_the_chord_length() {
    let mut smokes = Smokes::new(config(), FlatGround::default());
    smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 21, Style::Crystal));
    // Growing, full and fading: puffs pop in and out.
    for tick in [100, 640, 2100] {
        let view = smokes.view(tick);
        for (a, b) in segments(5, 60, Vec3::new(-7.0, 0.0, -7.0), Vec3::new(7.0, 4.0, 7.0)) {
            let exact = view.occlusion(a, b);
            let sampled = sampled_amount(&view, a, b, 0.004);
            assert!(
                (exact - sampled).abs() < 0.02 + exact * 0.002,
                "tick {tick}: {exact} vs sampled {sampled} on {a} .. {b}"
            );
        }
    }
}

/// Crystals are drawn unclipped, so even at their biggest (popping in) they
/// must stay clear of walls.
#[test]
fn crystals_never_poke_through_walls() {
    let boxes = common::boxes();
    let level = common::level();
    let config = config();
    let mut checked = 0;
    for (origin, seed) in common::pops() {
        let shape = fill_cloud(&config, &CloudSpec::new(origin, 0, seed, Style::Crystal), &level);
        let Some(f) = &shape.facets else {
            continue;
        };
        for p in &f.puffs {
            let reach = p.radius * p.shape.corner_ratio() * POP_OVERSHOOT;
            // Points on the sphere that holds the puff in any orientation.
            for i in 0..64 {
                let z = 1.0 - 2.0 * (i as f32 + 0.5) / 64.0;
                let (s, c) = math::sin_cos(i as f32 * 2.399_963);
                let r = (1.0 - z * z).sqrt();
                let q = p.center + Vec3::new(r * c, z, r * s) * reach;
                for (lo, hi) in &boxes {
                    assert!(
                        !(q.cmpgt(*lo).all() && q.cmplt(*hi).all()),
                        "puff at {} (reach {reach}) pokes into the box {lo} .. {hi}",
                        p.center
                    );
                }
                assert!(q.y > -1e-3, "puff at {} pokes through the ground", p.center);
            }
            checked += 1;
        }
    }
    assert!(checked > 500, "only {checked} puffs");
}

/// A bullet through a crystal cloud clears a line of sight along it, and the
/// crystals grow back.
#[test]
fn crystal_bullets_clear_a_tunnel() {
    let mut cfg = config();
    cfg.tuning.sight_threshold = 0.0;
    let mut smokes = Smokes::new(cfg, FlatGround::default());
    smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 9, Style::Crystal));
    let (a, b) = (Vec3::new(-10.0, 1.0, 0.3), Vec3::new(10.0, 1.2, -0.2));
    let shot = 5 * 128;
    assert!(smokes.blocks_sight(a, b, shot - 1));
    smokes.bullet(a, b, shot);
    // Open while the tunnel holds, closed again once it has.
    assert!(!smokes.blocks_sight(a, b, shot + 20), "the tunnel should be clear");
    assert!(smokes.blocks_sight(a, b, shot + 3 * 128), "the crystals should be back");
    // Off the tunnel's axis, still blocked.
    let side = Vec3::new(0.0, 0.0, 1.2);
    assert!(smokes.blocks_sight(a + side, b + side, shot + 20));
}

/// The faceted core rule only counts what is inside the drawn surface: never
/// more than the exact rule, and it never blocks where the exact rule
/// doesn't. With holes too.
#[test]
fn core_rule_is_conservative() {
    let level = common::level();
    let mut exact_cfg = config();
    exact_cfg.tuning.facet_rule = FacetRule::Exact;
    let mut core_cfg = config();
    core_cfg.tuning.facet_rule = FacetRule::Core;
    let mut exact = Smokes::new(exact_cfg, &level);
    let mut core = Smokes::new(core_cfg, &level);
    for (origin, seed) in common::pops().into_iter().take(4) {
        exact.pop(CloudSpec::new(origin, 0, seed, Style::Faceted));
        core.pop(CloudSpec::new(origin, 0, seed, Style::Faceted));
    }
    let shots = segments(77, 6, Vec3::new(-12.0, 0.5, -6.0), Vec3::new(14.0, 2.5, 12.0));
    for (i, (a, b)) in shots.iter().enumerate() {
        exact.bullet(*a, *b, 500 + i as u64 * 40);
        core.bullet(*a, *b, 500 + i as u64 * 40);
    }
    exact.explosion(Vec3::new(0.0, 1.0, 4.0), None, 700);
    core.explosion(Vec3::new(0.0, 1.0, 4.0), None, 700);
    let mut blocked = 0;
    for tick in [300, 640, 760, 1200, 2100] {
        for (a, b) in segments(tick, 150, Vec3::new(-12.0, 0.0, -6.0), Vec3::new(14.0, 4.0, 12.0)) {
            let e = exact.occlusion(a, b, tick);
            let c = core.occlusion(a, b, tick);
            assert!(c <= e + 0.01, "tick {tick}: core {c} > exact {e} on {a} .. {b}");
            let cb = core.blocks_sight(a, b, tick);
            assert!(
                !cb || exact.blocks_sight(a, b, tick),
                "tick {tick}: core blocks, exact doesn't"
            );
            blocked += cb as u32;
        }
    }
    assert!(blocked > 50, "the core rule should still block plenty: {blocked}");
}

/// The core rule counts every hole, however many a cloud has: the newest
/// past the 32nd too (a cloud lists its holes oldest first), and more
/// separate stretches of hole along one line than a check keeps on the
/// stack, which it then joins, counting less cloud, never more. The exact
/// rule's chord bounds count them all as well.
#[test]
fn core_rule_counts_every_hole() {
    let world = |rule: FacetRule| {
        let mut cfg = config();
        cfg.tuning.facet_rule = rule;
        cfg.tuning.max_holes_per_cloud = 64;
        let mut smokes = Smokes::new(cfg, FlatGround::default());
        smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 7, Style::Faceted));
        smokes
    };
    let (mut core, mut exact) = (world(FacetRule::Core), world(FacetRule::Exact));
    // 32 shots straight down, clear of the lines below, then one along
    // each line: the 33rd to 40th holes.
    let shot = 5 * 128;
    let lines: Vec<(Vec3, Vec3)> = (0..8)
        .map(|j| {
            let z = -1.75 + j as f32 * 0.5;
            (Vec3::new(-9.0, 1.0, z), Vec3::new(9.0, 1.0, z))
        })
        .collect();
    for smokes in [&mut core, &mut exact] {
        for i in 0..32 {
            let x = -4.0 + i as f32 * 0.25;
            smokes.bullet(Vec3::new(x, 9.0, 3.5), Vec3::new(x, -1.0, 3.5), shot + i);
        }
        for (j, (a, b)) in lines.iter().enumerate() {
            smokes.bullet(*a - Vec3::X, *b + Vec3::X, shot + 32 + j as u64);
        }
    }
    let tick = shot + 44;
    assert_eq!(core.view(tick).clouds().next().map(|c| c.state.holes.len()), Some(40));
    for (j, (a, b)) in lines.iter().enumerate() {
        assert!(
            core.blocks_sight(*a, *b, shot + 31),
            "line {j} should be blocked before its shot"
        );
        let amount = core.occlusion(*a, *b, tick);
        assert!(amount < 1e-3, "line {j}, along hole {}: {amount}", 33 + j);
        assert!(!core.blocks_sight(*a, *b, tick));
    }
    // With all of them, the core rule never counts more than the exact
    // one, and the exact rule's `blocks_sight` says what its `occlusion`
    // implies.
    let threshold = exact.config().tuning.sight_threshold;
    for (a, b) in segments(41, 300, Vec3::new(-8.0, 0.0, -6.0), Vec3::new(8.0, 3.5, 6.0)) {
        let (c, e) = (core.occlusion(a, b, tick), exact.occlusion(a, b, tick));
        assert!(c <= e + 0.01, "core {c} > exact {e} on {a} .. {b}");
        assert!(!core.blocks_sight(a, b, tick) || exact.blocks_sight(a, b, tick));
        assert_eq!(
            exact.blocks_sight(a, b, tick),
            e > 0.0 && e >= threshold,
            "{a} .. {b}: {e}"
        );
    }

    // Forty small blasts along one line: more separate stretches of hole
    // than fit, so the nearest are joined. Never more cloud than sampling
    // the rule's own `density_at` finds.
    let mut smokes = world(FacetRule::Core);
    for i in 0..40 {
        smokes.explosion(Vec3::new(-4.0 + i as f32 * 0.2, 1.2, 0.0), Some(0.08), shot);
    }
    let view = smokes.view(shot + 20);
    assert_eq!(view.clouds().next().map(|c| c.state.holes.len()), Some(40));
    let (a, b) = (Vec3::new(-6.0, 1.2, 0.0), Vec3::new(6.0, 1.2, 0.0));
    let amount = view.occlusion(a, b);
    let sampled = sampled_amount(&view, a, b, 0.0005);
    assert!(
        amount <= sampled + 0.01 && amount > sampled - 0.8,
        "{amount} vs sampled {sampled}"
    );
}

/// Faceted clouds in the test level, with bullets and a blast.
fn faceted_level(rule: FacetRule, level: &BoxObstacles) -> Smokes<&BoxObstacles> {
    let mut cfg = config();
    cfg.tuning.facet_rule = rule;
    let mut smokes = Smokes::new(cfg, level);
    for (origin, seed) in common::pops().into_iter().take(4) {
        smokes.pop(CloudSpec::new(origin, 0, seed, Style::Faceted));
    }
    let shots = segments(77, 6, Vec3::new(-12.0, 0.5, -6.0), Vec3::new(14.0, 2.5, 12.0));
    for (i, (a, b)) in shots.iter().enumerate() {
        smokes.bullet(*a, *b, 500 + i as u64 * 40);
    }
    smokes.explosion(Vec3::new(0.0, 1.0, 4.0), None, 700);
    smokes
}

/// The chord rule counts at least what the core rule counts and at most
/// what the drawn surface holds, and blocks accordingly: it blocks every
/// line the core rule blocks, more lines besides, and none that the exact
/// rule sees through (bar lines the exact rule's trace puts within a few
/// millimetres of the threshold).
#[test]
fn chord_rule_lies_between_core_and_exact() {
    let level = common::level();
    let mut core = faceted_level(FacetRule::Core, &level);
    let mut chords = faceted_level(FacetRule::Chords, &level);
    let mut exact = faceted_level(FacetRule::Exact, &level);
    let threshold = exact.config().tuning.sight_threshold;
    let (mut by_core, mut by_chords) = (0, 0);
    for tick in [300, 640, 760, 1200, 2000, 2100, 2200] {
        for (a, b) in segments(tick, 150, Vec3::new(-12.0, 0.0, -6.0), Vec3::new(14.0, 4.0, 12.0)) {
            let c = core.occlusion(a, b, tick);
            let h = chords.occlusion(a, b, tick);
            let e = exact.occlusion(a, b, tick);
            assert!(c <= h + 1e-4, "tick {tick}: core {c} > chords {h} on {a} .. {b}");
            assert!(h <= e + 0.01, "tick {tick}: chords {h} > exact {e} on {a} .. {b}");
            let (cb, hb) = (core.blocks_sight(a, b, tick), chords.blocks_sight(a, b, tick));
            assert!(!cb || hb, "tick {tick}: core blocks, chords don't, on {a} .. {b}");
            assert!(
                !hb || exact.blocks_sight(a, b, tick) || e > threshold - 0.02,
                "tick {tick}: chords block ({h}), exact doesn't ({e}), on {a} .. {b}"
            );
            by_core += cb as u32;
            by_chords += hb as u32;
        }
    }
    assert!(by_chords > by_core, "chords block {by_chords} lines, core {by_core}");
}

/// The chord rule's amount is the length inside what it counts: compare
/// with sampling its own `density_at`, through walls, holes and the fade.
#[test]
fn chord_amount_is_the_chord_length() {
    let level = common::level();
    let mut smokes = faceted_level(FacetRule::Chords, &level);
    for tick in [140, 640, 760, 2100] {
        let view = smokes.view(tick);
        for (a, b) in segments(tick + 5, 60, Vec3::new(-12.0, 0.0, -6.0), Vec3::new(14.0, 4.0, 12.0)) {
            let exact = view.occlusion(a, b);
            // Every cloud adds its own amount, so no clamping where they
            // overlap.
            let n = (a.distance(b) / 0.004).ceil() as usize;
            let dt = a.distance(b) / n as f32;
            let sampled: f32 = (0..n)
                .map(|i| view.density_at(a.lerp(b, (i as f32 + 0.5) / n as f32)) * dt)
                .sum();
            assert!(
                (exact - sampled).abs() < 0.02 + exact * 0.002,
                "tick {tick}: {exact} vs sampled {sampled} on {a} .. {b}"
            );
        }
    }
}

/// Everything the chord rule counts is inside the drawn surface (the exact
/// rule's distance field), by walls, in holes and through the fade.
#[test]
fn chord_rule_counts_only_what_is_drawn() {
    let level = common::level();
    let mut smokes = faceted_level(FacetRule::Chords, &level);
    let mut rng = common::Rng(31);
    let mut counted = 0;
    for tick in [100, 200, 640, 760, 1950, 2100, 2250] {
        let view = smokes.view(tick);
        for cloud in view.clouds() {
            let (Some(chords), Some(drawn)) = (cloud.chord_field(), cloud.facet_field()) else {
                continue;
            };
            let (lo, hi) = cloud.state.bounds;
            for _ in 0..4000 {
                let p = rng.point(lo, hi);
                if chords.inside(p) {
                    let d = drawn.sdf(p);
                    assert!(
                        d < 1e-4,
                        "tick {tick}: {p} is counted, but {d} outside the drawn surface"
                    );
                    counted += 1;
                }
            }
        }
    }
    assert!(counted > 2000, "only {counted} points counted");
}

/// A line source makes a wall of smoke along it; a box source fills a box.
#[test]
fn line_and_box_sources() {
    let mut smokes = Smokes::new(config(), FlatGround::default());
    let wall = CloudSpec::new(Vec3::new(-5.0, 0.5, 0.0), 0, 3, Style::Volumetric).with_source(CloudSource::Line {
        end: Vec3::new(5.0, 0.5, 0.0),
    });
    smokes.pop(wall);
    let tick = 5 * 128;
    for x in [-4.5, -2.0, 0.0, 2.0, 4.5] {
        assert!(
            smokes.density_at(Vec3::new(x, 0.8, 0.0), tick) > 0.5,
            "no wall at x = {x}"
        );
    }
    // A wall, not a dome: thin across, long along.
    assert!(smokes.occlusion(Vec3::new(-7.0, 1.0, 0.0), Vec3::new(7.0, 1.0, 0.0), tick) > 9.0);
    assert!(smokes.occlusion(Vec3::new(0.0, 1.0, -7.0), Vec3::new(0.0, 1.0, 7.0), tick) < 6.0);

    let mut smokes = Smokes::new(config(), FlatGround::default());
    let half = Vec3::new(3.0, 1.0, 2.0);
    let mut bank = CloudSpec::new(Vec3::new(20.0, 1.0, 0.0), 0, 4, Style::Volumetric)
        .with_source(CloudSource::Box { half_extents: half });
    bank.volume = Some(8.0 * half.x * half.y * half.z);
    smokes.pop(bank);
    // Filled out along each axis (the edges feather, the corners round off),
    // and nothing far past the box.
    let center = Vec3::new(20.0, 1.0, 0.0);
    for dir in [Vec3::X, -Vec3::X, Vec3::Z, -Vec3::Z] {
        let p = center + half * dir * 0.6;
        assert!(smokes.density_at(p, tick) > 0.3, "the box isn't filled at {p}");
        let far = center + half * dir * 1.8;
        assert_eq!(smokes.density_at(far, tick), 0.0, "smoke far past the box at {far}");
    }
    assert!(smokes.density_at(center, tick) > 0.9);
}

/// Sources work in other units and up axes too.
#[test]
fn sources_follow_units() {
    let units = common::z_up_inches();
    let m = |v: Vec3| units.from_internal(v);
    let mut smokes = Smokes::new(Config::new(units, 128.0), FlatGround::new(0.0, units.up));
    let wall = CloudSpec::new(m(Vec3::new(-5.0, 0.5, 0.0)), 0, 3, Style::Volumetric).with_source(CloudSource::Line {
        end: m(Vec3::new(5.0, 0.5, 0.0)),
    });
    smokes.pop(wall);
    let tick = 5 * 128;
    for x in [-4.5, 0.0, 4.5] {
        assert!(
            smokes.density_at(m(Vec3::new(x, 0.8, 0.0)), tick) > 0.5,
            "no wall at x = {x}"
        );
    }
}

/// Past `max_clouds`, the oldest clouds stop counting.
#[test]
fn max_clouds_keeps_the_newest() {
    let mut cfg = config();
    cfg.tuning.max_clouds = 3;
    let mut smokes = Smokes::new(cfg, FlatGround::default());
    let mut ids = Vec::new();
    for i in 0..5u64 {
        ids.push(smokes.pop(CloudSpec::new(
            Vec3::new(i as f32 * 20.0, 0.0, 0.0),
            i * 10,
            i as u32,
            Style::Volumetric,
        )));
    }
    let view = smokes.view(5 * 128);
    let counted: Vec<CloudId> = view.clouds().map(|c| c.state.id).collect();
    assert_eq!(counted, ids[2..].to_vec());
    // The oldest is there, but no longer blocks.
    let (a, b) = (Vec3::new(0.0, 1.0, -9.0), Vec3::new(0.0, 1.0, 9.0));
    assert_eq!(view.occlusion(a, b), 0.0);
    let (a, b) = (Vec3::new(80.0, 1.0, -9.0), Vec3::new(80.0, 1.0, 9.0));
    assert!(view.occlusion(a, b) > 1.0);
}

/// Clouds popped on the same tick give the same answers whatever order the
/// pops arrive in.
#[test]
fn pop_order_does_not_matter() {
    let level = common::level();
    let pops: Vec<CloudSpec> = common::pops()
        .into_iter()
        .enumerate()
        .map(|(i, (origin, seed))| {
            let style = [Style::Volumetric, Style::Faceted, Style::Crystal][i % 3];
            CloudSpec::new(origin, 50, seed, style)
        })
        .collect();
    let mut forward = Smokes::new(config(), &level);
    let mut backward = Smokes::new(config(), &level);
    for spec in &pops {
        forward.pop(*spec);
    }
    for spec in pops.iter().rev() {
        backward.pop(*spec);
    }
    let tick = 50 + 5 * 128;
    for (a, b) in segments(9, 300, Vec3::new(-16.0, 0.0, -10.0), Vec3::new(16.0, 4.0, 12.0)) {
        assert_eq!(
            forward.occlusion(a, b, tick).to_bits(),
            backward.occlusion(a, b, tick).to_bits()
        );
    }
}

/// Crystal, core and chord checks are cheaper than exact faceted ones (they
/// visit far fewer puffs), and the exact rule's `blocks_sight` is cheaper still
/// than its amounts, since chord bounds settle most lines without tracing
/// them; a rough guard against regressions, not a benchmark.
#[test]
fn crystal_core_and_chord_checks_are_cheap() {
    use std::time::Instant;
    // Best of five: 200 segments through 8 grown clouds, counting the lines
    // blocked by `blocks_sight` or (`amounts`) by `occlusion`.
    let time = |style: Style, rule: FacetRule, amounts: bool| {
        let mut cfg = config();
        cfg.tuning.facet_rule = rule;
        let mut smokes = Smokes::new(cfg, FlatGround::default());
        for i in 0..8 {
            smokes.pop(CloudSpec::new(Vec3::new(i as f32 * 12.0, 0.0, 0.0), 0, i, style));
        }
        let tick = 5 * 128;
        let view = smokes.view(tick);
        let segs: Vec<(Vec3, Vec3)> = (0..200)
            .map(|i| {
                let x = (i % 4) as f32 * 12.0 - 6.0;
                (Vec3::new(x, 1.0, -0.5), Vec3::new(x + 48.0, 1.5, 0.5))
            })
            .collect();
        let mut best = f64::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            let n = if amounts {
                segs.iter().filter(|(a, b)| view.occlusion(*a, *b) >= 0.8).count()
            } else {
                segs.iter().filter(|(a, b)| view.blocks_sight(*a, *b)).count()
            };
            best = best.min(t.elapsed().as_secs_f64());
            assert!(n > 150, "{style:?} {rule:?}: only {n} blocked");
        }
        best
    };
    let exact = time(Style::Faceted, FacetRule::Exact, true);
    let core = time(Style::Faceted, FacetRule::Core, true);
    let chords = time(Style::Faceted, FacetRule::Chords, true);
    let crystal = time(Style::Crystal, FacetRule::Exact, true);
    let bounded = time(Style::Faceted, FacetRule::Exact, false);
    assert!(core < exact * 0.6, "core {core} vs exact {exact}");
    assert!(chords < exact * 0.6, "chords {chords} vs exact {exact}");
    assert!(crystal < exact * 0.6, "crystal {crystal} vs exact {exact}");
    assert!(
        bounded < exact * 0.5,
        "exact blocks_sight {bounded} vs exact amounts {exact}"
    );
}
