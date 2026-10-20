//! Gameplay queries: the rule, holes, ticks, units, and that nothing
//! allocates per query.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use smokescreen::glam::Vec3;
use smokescreen::*;

/// Counts this thread's allocations, to check queries make none (other
/// tests run on other threads at the same time).
struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn allocations() -> usize {
    ALLOCATIONS.with(Cell::get)
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn config() -> Config {
    Config::new(Units::METRES_Y_UP, 128.0)
}

/// A world with one grown cloud at the origin; returns it and a tick at
/// which the cloud is full.
fn one_cloud(style: Style) -> (Smokes<FlatGround>, u64) {
    let mut smokes = Smokes::new(config(), FlatGround::default());
    smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 4, style));
    (smokes, 5 * 128)
}

#[test]
fn line_of_sight_through_smoke() {
    for style in [Style::Volumetric, Style::Faceted] {
        let (mut smokes, tick) = one_cloud(style);
        let a = Vec3::new(-10.0, 1.2, 0.0);
        let b = Vec3::new(10.0, 1.2, 0.0);
        let through = smokes.occlusion(a, b, tick);
        assert!(
            through > 3.0,
            "{style:?}: a full cloud should be metres thick: {through}"
        );
        assert!(smokes.blocks_sight(a, b, tick));
        // Over the top there is nothing.
        let up = Vec3::Y * 7.0;
        assert_eq!(smokes.occlusion(a + up, b + up, tick), 0.0);
        assert!(!smokes.blocks_sight(a + up, b + up, tick));
        // Nothing before the cloud starts growing, nothing after it's gone.
        assert_eq!(smokes.occlusion(a, b, 1), 0.0);
        assert_eq!(smokes.occlusion(a, b, 19 * 128), 0.0);
        // The amount never exceeds the segment's length.
        let inside = smokes.occlusion(Vec3::new(-0.5, 1.0, 0.0), Vec3::new(0.5, 1.0, 0.0), tick);
        assert!(inside <= 1.0 + 1e-3, "{style:?}: {inside}");
    }
}

#[test]
fn bullets_tunnel_through_and_the_tunnel_closes() {
    for style in [Style::Volumetric, Style::Faceted] {
        let (mut smokes, tick) = one_cloud(style);
        let a = Vec3::new(-10.0, 1.2, 0.0);
        let b = Vec3::new(10.0, 1.2, 0.0);
        let before = smokes.occlusion(a, b, tick);
        let prior = smokes.occlusion(a, b, tick - 1);
        smokes.bullet(a, b, tick);
        let after = smokes.occlusion(a, b, tick + 13);
        assert!(after < before * 0.1, "{style:?}: tunnel left {after} of {before}");
        assert!(!smokes.blocks_sight(a, b, tick + 13), "{style:?}");
        // A parallel line a metre away is still blocked.
        assert!(smokes.blocks_sight(a + Vec3::Z, b + Vec3::Z, tick + 13), "{style:?}");
        // The tunnel has no effect before the shot, or once it has closed.
        assert_eq!(smokes.occlusion(a, b, tick - 1).to_bits(), prior.to_bits(), "{style:?}");
        let closed = smokes.occlusion(a, b, tick + 3 * 128);
        let untouched = {
            let (mut fresh, _) = one_cloud(style);
            fresh.occlusion(a, b, tick + 3 * 128)
        };
        assert_eq!(closed.to_bits(), untouched.to_bits(), "{style:?}");
    }
}

#[test]
fn explosions_clear_and_refill() {
    for style in [Style::Volumetric, Style::Faceted] {
        let (mut smokes, tick) = one_cloud(style);
        let a = Vec3::new(-10.0, 1.0, 0.0);
        let b = Vec3::new(10.0, 1.0, 0.0);
        let before = smokes.occlusion(a, b, tick);
        smokes.explosion(Vec3::new(0.0, 1.0, 0.0), None, tick);
        let open = smokes.occlusion(a, b, tick + 40);
        let later = smokes.occlusion(a, b, tick + 4 * 128);
        assert!(open < before * 0.5, "{style:?}: {open} vs {before}");
        assert!(later > open, "{style:?}: should refill: {later} vs {open}");
    }
}

#[test]
fn holes_from_before_the_pop_do_nothing() {
    let mut smokes = Smokes::new(config(), FlatGround::default());
    let a = Vec3::new(-10.0, 1.2, 0.0);
    let b = Vec3::new(10.0, 1.2, 0.0);
    smokes.bullet(a, b, 100);
    smokes.pop(CloudSpec::new(Vec3::ZERO, 110, 4, Style::Faceted));
    let mut clean = Smokes::new(config(), FlatGround::default());
    clean.pop(CloudSpec::new(Vec3::ZERO, 110, 4, Style::Faceted));
    let t = 110 + 30;
    assert_eq!(smokes.occlusion(a, b, t).to_bits(), clean.occlusion(a, b, t).to_bits());
}

#[test]
fn holes_off() {
    let mut cfg = config();
    cfg.tuning.holes = None;
    let mut smokes = Smokes::new(cfg, FlatGround::default());
    smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 4, Style::Volumetric));
    let a = Vec3::new(-10.0, 1.2, 0.0);
    let b = Vec3::new(10.0, 1.2, 0.0);
    let before = smokes.occlusion(a, b, 640);
    smokes.bullet(a, b, 600);
    smokes.explosion(Vec3::new(0.0, 1.0, 0.0), None, 600);
    assert_eq!(smokes.occlusion(a, b, 640).to_bits(), before.to_bits());
}

/// The order events arrive in doesn't matter, only their ticks.
#[test]
fn event_order_does_not_matter() {
    let shots: Vec<(Vec3, Vec3, u64)> = (0..24)
        .map(|i| {
            let y = 0.4 + (i % 6) as f32 * 0.35;
            (
                Vec3::new(-9.0, y, -3.0 + i as f32 * 0.25),
                Vec3::new(9.0, y + 0.3, 2.0),
                600 + (i % 5) as u64,
            )
        })
        .collect();
    let run = |order: &[usize]| {
        let mut smokes = Smokes::new(config(), FlatGround::default());
        smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 4, Style::Faceted));
        for &i in order {
            let (a, b, t) = shots[i];
            smokes.bullet(a, b, t);
        }
        let mut out = Vec::new();
        for k in 0..40 {
            let z = -4.0 + k as f32 * 0.2;
            out.push(
                smokes
                    .occlusion(Vec3::new(-9.0, 1.0, z), Vec3::new(9.0, 1.4, z), 610)
                    .to_bits(),
            );
        }
        out
    };
    let forward: Vec<usize> = (0..shots.len()).collect();
    let backward: Vec<usize> = (0..shots.len()).rev().collect();
    let shuffled: Vec<usize> = (0..shots.len()).map(|i| (i * 7) % shots.len()).collect();
    let a = run(&forward);
    assert_eq!(a, run(&backward));
    assert_eq!(a, run(&shuffled));
}

/// `blocks_sight` is exactly `occlusion > 0 && occlusion >= threshold`.
#[test]
fn blocks_sight_matches_occlusion_exactly() {
    let level = common::level();
    for style in [Style::Volumetric, Style::Faceted] {
        for threshold in [0.0, 0.3, 0.8, 2.5] {
            let mut cfg = config();
            cfg.tuning.sight_threshold = threshold;
            let mut smokes = Smokes::new(cfg, &level);
            for (origin, seed) in common::pops().into_iter().take(3) {
                smokes.pop(CloudSpec::new(origin, 0, seed, style));
            }
            let mut rng = common::Rng(99);
            let tick = 300;
            let mut blocked = 0;
            for _ in 0..400 {
                let a = rng.point(Vec3::new(-12.0, 0.0, -6.0), Vec3::new(16.0, 4.0, 8.0));
                let b = rng.point(Vec3::new(-12.0, 0.0, -6.0), Vec3::new(16.0, 4.0, 8.0));
                let o = smokes.occlusion(a, b, tick);
                let expected = o > 0.0 && o >= threshold;
                assert_eq!(smokes.blocks_sight(a, b, tick), expected, "{style:?} {threshold}: {o}");
                blocked += expected as u32;
            }
            assert!(blocked > 20, "{style:?} {threshold}: only {blocked} blocked");
        }
    }
}

/// The faceted amount is the length of the segment inside the surface:
/// compare with brute-force sampling of the distance field's sign.
#[test]
fn faceted_amount_is_the_chord_length() {
    let (mut smokes, _) = one_cloud(Style::Faceted);
    let mut rng = common::Rng(5);
    let mut worst = 0.0_f32;
    for tick in [140, 300, 640, 2100] {
        let view = smokes.view(tick);
        let Some(cloud) = view.clouds().next() else {
            panic!("no cloud at {tick}");
        };
        let field = cloud.facet_field().expect("faceted");
        for _ in 0..60 {
            let a = rng.point(Vec3::new(-7.0, 0.0, -7.0), Vec3::new(7.0, 3.5, 7.0));
            let b = rng.point(Vec3::new(-7.0, 0.0, -7.0), Vec3::new(7.0, 3.5, 7.0));
            let fast = view.occlusion(a, b);
            let len = a.distance(b);
            let n = (len / 0.0005).ceil() as usize;
            let dt = len / n as f32;
            let brute: f32 = (0..n)
                .filter(|&i| field.sdf(a.lerp(b, (i as f32 + 0.5) / n as f32)) < 0.0)
                .count() as f32
                * dt;
            worst = worst.max((fast - brute).abs());
        }
    }
    assert!(worst < 0.03, "chord error up to {worst} m");
}

/// A client that joins late (or rewinds for a replay) computes the same
/// cloud at the same tick as one that watched it from the pop.
#[test]
fn late_joiners_and_replays_agree() {
    let level = common::level();
    let pops = common::pops();
    let mut live = Smokes::new(config(), &level);
    for (i, (origin, seed)) in pops.iter().enumerate() {
        let style = if i % 2 == 0 { Style::Faceted } else { Style::Volumetric };
        live.pop(CloudSpec::new(*origin, 10 * i as u64, *seed, style));
    }
    let mut results = Vec::new();
    for tick in 0..1000 {
        live.advance(tick);
        if tick % 50 == 0 {
            live.bullet(Vec3::new(-15.0, 1.0, -3.0), Vec3::new(18.0, 1.5, 7.0), tick);
        }
        if tick % 123 == 7 {
            results.push((
                tick,
                live.occlusion(Vec3::new(-15.0, 1.2, 0.0), Vec3::new(18.0, 1.6, 5.0), tick),
            ));
        }
    }
    // Joins at tick 900 and asks about the past.
    let mut late = Smokes::new(config(), &level);
    for (i, (origin, seed)) in pops.iter().enumerate() {
        let style = if i % 2 == 0 { Style::Faceted } else { Style::Volumetric };
        late.pop(CloudSpec::new(*origin, 10 * i as u64, *seed, style));
    }
    for tick in (0..1000).step_by(50) {
        late.bullet(Vec3::new(-15.0, 1.0, -3.0), Vec3::new(18.0, 1.5, 7.0), tick);
    }
    for (tick, expected) in results.iter().rev() {
        let got = late.occlusion(Vec3::new(-15.0, 1.2, 0.0), Vec3::new(18.0, 1.6, 5.0), *tick);
        assert_eq!(got.to_bits(), expected.to_bits(), "tick {tick}");
    }
}

/// Z-up inches and Y-up metres describe the same world.
#[test]
fn units_and_up_axis() {
    let metres = Units::METRES_Y_UP;
    let inches = common::z_up_inches();
    let mut a_world = Smokes::new(Config::new(metres, 128.0), common::level_in(metres));
    let mut b_world = Smokes::new(Config::new(inches, 128.0), common::level_in(inches));
    for (i, (origin, seed)) in common::pops().into_iter().take(4).enumerate() {
        let style = if i % 2 == 0 { Style::Faceted } else { Style::Volumetric };
        a_world.pop(CloudSpec::new(origin, 0, seed, style));
        b_world.pop(CloudSpec::new(inches.from_internal(origin), 0, seed, style));
    }
    // Inches: 32 u is 0.81 m.
    assert!((b_world.config().tuning.sight_threshold - 0.8 / 0.0254).abs() < 1e-3);
    let mut rng = common::Rng(17);
    let mut agree = 0;
    let mut total = 0;
    let mut worst = 0.0_f32;
    for _ in 0..400 {
        let a = rng.point(Vec3::new(-12.0, 0.2, -6.0), Vec3::new(14.0, 3.5, 12.0));
        let b = rng.point(Vec3::new(-12.0, 0.2, -6.0), Vec3::new(14.0, 3.5, 12.0));
        let om = a_world.occlusion(a, b, 700);
        let oi = b_world.occlusion(inches.from_internal(a), inches.from_internal(b), 700) * 0.0254;
        worst = worst.max((om - oi).abs());
        total += 1;
        agree += (a_world.blocks_sight(a, b, 700)
            == b_world.blocks_sight(inches.from_internal(a), inches.from_internal(b), 700)) as u32;
    }
    // Rounding in the unit conversion moves voxels by a hair, nothing more.
    assert!(agree as f32 >= total as f32 * 0.98, "{agree}/{total} agree");
    assert!(worst < 0.25, "worst difference {worst} m");
}

/// Queries allocate nothing once a tick's state exists.
#[test]
fn queries_do_not_allocate() {
    let level = common::level();
    for rule in [FacetRule::Exact, FacetRule::Core, FacetRule::Chords] {
        let mut cfg = config();
        cfg.tuning.facet_rule = rule;
        let mut smokes = Smokes::new(cfg, &level);
        for (i, (origin, seed)) in common::pops().into_iter().enumerate() {
            let style = [Style::Faceted, Style::Volumetric, Style::Crystal][i % 3];
            smokes.pop(CloudSpec::new(origin, 0, seed, style));
        }
        smokes.bullet(Vec3::new(-15.0, 1.0, -3.0), Vec3::new(18.0, 1.5, 7.0), 590);
        smokes.explosion(Vec3::new(0.0, 1.0, 4.0), None, 595);
        let tick = 600;
        let _ = smokes.occlusion(Vec3::ZERO, Vec3::ONE, tick);
        let mut rng = common::Rng(3);
        let segments: Vec<(Vec3, Vec3)> = (0..500)
            .map(|_| {
                (
                    rng.point(Vec3::new(-16.0, 0.0, -10.0), Vec3::new(16.0, 4.0, 12.0)),
                    rng.point(Vec3::new(-16.0, 0.0, -10.0), Vec3::new(16.0, 4.0, 12.0)),
                )
            })
            .collect();
        let before = allocations();
        let mut sum = 0.0;
        for (a, b) in &segments {
            sum += smokes.occlusion(*a, *b, tick);
            sum += smokes.blocks_sight(*a, *b, tick) as u32 as f32;
            sum += smokes.density_at(*a, tick);
        }
        let after = allocations();
        assert!(sum > 0.0);
        assert_eq!(after - before, 0, "queries allocated ({rule:?})");
    }
}

/// A view answers from many threads at once.
#[test]
fn views_are_shared_between_threads() {
    let (mut smokes, tick) = one_cloud(Style::Faceted);
    let view = smokes.view(tick);
    let a = Vec3::new(-10.0, 1.2, 0.0);
    let b = Vec3::new(10.0, 1.2, 0.0);
    let expected = view.occlusion(a, b);
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| assert_eq!(view.occlusion(a, b).to_bits(), expected.to_bits()));
        }
    });
}

/// A cloud filled elsewhere and inserted answers exactly as one popped, in
/// every style; so does a volumetric one whose grid comes without its
/// sample cells, only more slowly.
#[test]
fn insert_matches_pop() {
    let level = common::level();
    let (origin, seed) = common::pops()[3];
    for style in [Style::Volumetric, Style::Faceted, Style::Crystal] {
        let spec = CloudSpec::new(origin, 0, seed, style);
        let shape = fill_cloud(&config(), &spec, &level);
        let mut bare = shape.clone();
        bare.grid.sample_cells = Vec::new();
        let mut a = Smokes::new(config(), &level);
        a.pop(spec);
        let mut b = Smokes::new(config(), &level);
        b.insert(spec, Arc::new(shape));
        let mut c = Smokes::new(config(), &level);
        c.insert(spec, Arc::new(bare));
        let mut rng = common::Rng(seed as u64);
        // Inside the room the cloud fills.
        let (lo, hi) = (origin - Vec3::new(6.0, 0.0, 2.5), origin + Vec3::new(5.0, 3.0, 5.0));
        let mut through = 0;
        // Growing, grown, and early, halfway and late in the fade.
        for tick in [20, 200, 900, 1990, 2112, 2240] {
            for _ in 0..40 {
                let (p, q) = (rng.point(lo, hi), rng.point(lo, hi));
                let want = a.occlusion(p, q, tick);
                let (got, bare) = (b.occlusion(p, q, tick), c.occlusion(p, q, tick));
                assert_eq!(got.to_bits(), want.to_bits(), "{style:?}, tick {tick}");
                assert_eq!(bare.to_bits(), want.to_bits(), "{style:?} without cells, tick {tick}");
                through += (want > 0.0) as u32;
            }
        }
        assert!(through > 60, "{style:?}: {through} lines through cloud");
    }
}

/// `blocks_sight` says exactly what `occlusion` implies (every cloud adds a
/// non-negative amount, so stopping early can't change the answer), also
/// where faceted clouds under the exact rule are settled from chord bounds
/// rather than traced: random segments, long and short, through clouds of
/// every style over their whole lives, with bullets and explosions, under
/// every facet rule, with volumetric samples a voxel and half a voxel apart,
/// and with any contact blocking.
fn blocks_sight_agrees_with_occlusion(per_tick: usize) {
    let level = common::level();
    for (rule, steps, threshold) in [
        (FacetRule::Exact, VolumeSteps::Voxel, 0.8),
        (FacetRule::Core, VolumeSteps::HalfVoxel, 0.8),
        (FacetRule::Chords, VolumeSteps::Voxel, 0.8),
        (FacetRule::Exact, VolumeSteps::Voxel, 0.0),
    ] {
        let mut cfg = config();
        cfg.tuning.facet_rule = rule;
        cfg.tuning.volume_steps = steps;
        cfg.tuning.sight_threshold = threshold;
        let mut smokes = Smokes::new(cfg, &level);
        let mut rng = common::Rng(0xdead_beef_cafe_f00d);
        let styles = [Style::Volumetric, Style::Faceted, Style::Crystal];
        for i in 0..24 {
            let origin = Vec3::new(rng.range(-16.0, 16.0), rng.range(0.0, 3.0), rng.range(-12.0, 16.0));
            smokes.pop(CloudSpec::new(origin, 20 * i, i as u32 * 7 + 1, styles[i as usize % 3]));
        }
        let lo = Vec3::new(-20.0, 0.0, -14.0);
        let hi = Vec3::new(20.0, 5.0, 18.0);
        let mut blocked = 0;
        for tick in 0..(19 * 128) {
            smokes.advance(tick);
            if tick % 23 == 0 {
                let (a, b) = (rng.point(lo, hi), rng.point(lo, hi));
                smokes.bullet(a, b, tick);
            }
            if tick % 301 == 50 {
                smokes.explosion(rng.point(lo, hi), None, tick);
            }
            let view = smokes.view(tick);
            for _ in 0..per_tick {
                let a = rng.point(lo, hi);
                let b = if rng.next_u32().is_multiple_of(4) {
                    a + rng.point(Vec3::splat(-3.0), Vec3::splat(3.0))
                } else {
                    rng.point(lo, hi)
                };
                let amount = view.occlusion(a, b);
                let blocks = view.blocks_sight(a, b);
                assert_eq!(
                    blocks,
                    amount > 0.0 && amount >= threshold,
                    "{rule:?}, {steps:?} at {threshold}: from {a} to {b} at tick {tick}, amount {amount}"
                );
                blocked += blocks as usize;
            }
        }
        // The segments must test both answers.
        let total = 19 * 128 * per_tick;
        assert!(
            blocked > total / 5 && blocked < total * 4 / 5,
            "{rule:?}, {steps:?} at {threshold}: {blocked} of {total} blocked"
        );
    }
}

#[test]
fn blocks_sight_matches_occlusion() {
    blocks_sight_agrees_with_occlusion(8);
}

/// About 730,000 queries: `cargo test -p smokescreen --release -- --ignored`.
#[test]
#[ignore = "slow: about 730,000 queries"]
fn blocks_sight_matches_occlusion_thoroughly() {
    blocks_sight_agrees_with_occlusion(100);
}
