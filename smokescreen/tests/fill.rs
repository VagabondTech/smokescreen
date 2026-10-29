//! The flood fill: where smoke goes, and that it gets there the same way
//! however the work is split.

mod common;

use std::sync::Arc;

use smokescreen::glam::Vec3;
use smokescreen::light::{LightRays, visibility};
use smokescreen::*;

fn config() -> Config {
    Config::new(Units::METRES_Y_UP, 128.0)
}

fn fill(level: &dyn Level, origin: Vec3, style: Style) -> CloudShape {
    fill_cloud(&config(), &CloudSpec::new(origin, 0, 7, style), level)
}

#[test]
fn fills_target_volume_in_open_space() {
    let g = fill(&FlatGround::default(), Vec3::new(0.0, 0.05, 0.0), Style::Volumetric).grid;
    let t = config().tuning;
    let target = (t.volume / (t.voxel_size * t.voxel_size * t.voxel_size)).round() as u32;
    assert_eq!(g.filled, target);
    // Wider than tall, resting on the ground.
    let size = g.bounds_max - g.bounds_min;
    assert!(size.x > size.y && size.z > size.y, "size {size:?}");
    assert!(
        g.bounds_min.y >= -0.31,
        "smoke went below the floor: {:?}",
        g.bounds_min
    );
    // Roughly CS2-sized: 6-10 m across.
    assert!(size.x > 6.0 && size.x < 11.0, "size {size:?}");
}

#[test]
fn walls_stop_smoke_and_doorways_let_it_through() {
    // A wall at z = 2 with a 1.2 m doorway centred on x = 0.
    let wall = |x0: f32, x1: f32| {
        ObstacleBox::from_center_size(Vec3::new((x0 + x1) * 0.5, 1.5, 2.0), Vec3::new(x1 - x0, 3.0, 0.2))
    };
    let origin = Vec3::new(0.0, 0.0, 0.8);
    let level = BoxObstacles::new(vec![wall(-20.0, -0.6), wall(0.6, 20.0)]);
    let g = fill(&level, origin, Style::Volumetric).grid;
    let filled = |p: Vec3| g.sample_distance(p) < 0.9;
    assert!(filled(Vec3::new(0.0, 1.0, 3.3)), "smoke should pass the doorway");
    assert!(!filled(Vec3::new(-3.0, 1.0, 2.7)));
    assert!(!filled(Vec3::new(3.0, 1.0, 2.7)));

    let closed = BoxObstacles::new(vec![wall(-20.0, 20.0)]);
    let g = fill(&closed, origin, Style::Volumetric).grid;
    let filled = |p: Vec3| g.sample_distance(p) < 0.9;
    assert!(filled(Vec3::new(0.0, 1.0, 0.5)));
    for x in [-3.0, 0.0, 3.0] {
        assert!(!filled(Vec3::new(x, 1.0, 2.6)), "leaked through the wall at x={x}");
    }
}

#[test]
fn corridor_pushes_smoke_further() {
    let level = BoxObstacles::new(vec![
        ObstacleBox::from_center_size(Vec3::new(-1.1, 1.5, 0.0), Vec3::new(0.2, 3.0, 40.0)),
        ObstacleBox::from_center_size(Vec3::new(1.1, 1.5, 0.0), Vec3::new(0.2, 3.0, 40.0)),
        ObstacleBox::from_center_size(Vec3::new(0.0, 3.1, 0.0), Vec3::new(2.4, 0.2, 40.0)),
    ]);
    let open = fill(&FlatGround::default(), Vec3::ZERO, Style::Volumetric).grid;
    let corridor = fill(&level, Vec3::ZERO, Style::Volumetric).grid;
    let reach_open = open.bounds_max.z - open.bounds_min.z;
    let reach_corridor = corridor.bounds_max.z - corridor.bounds_min.z;
    assert!(reach_corridor > reach_open * 1.3, "{reach_corridor} vs {reach_open}");
}

#[test]
fn nowhere_to_go_gives_an_empty_cloud() {
    let buried = BoxObstacles::new(vec![ObstacleBox::from_center_size(Vec3::ZERO, Vec3::splat(40.0))]);
    let shape = fill(&buried, Vec3::ZERO, Style::Faceted);
    assert!(shape.is_empty());
    let mut smokes = Smokes::new(config(), buried);
    smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 1, Style::Faceted));
    assert_eq!(
        smokes.occlusion(Vec3::new(-5.0, 0.0, 0.0), Vec3::new(5.0, 0.0, 0.0), 300),
        0.0
    );
}

/// Volumetric clouds carry the cells their samples read, as
/// `Grid::build_sample_cells` builds them from the finished grid, an empty
/// cloud's too; faceted and crystal clouds, whose rules never sample the
/// distances that way, carry none.
#[test]
fn fills_build_the_sample_cells() {
    let level = common::level();
    for style in [Style::Volumetric, Style::Faceted, Style::Crystal] {
        for (origin, seed) in common::pops() {
            let grid = fill_cloud(&config(), &CloudSpec::new(origin, 0, seed, style), &level).grid;
            if style == Style::Volumetric {
                let mut rebuilt = grid.clone();
                rebuilt.build_sample_cells();
                assert_eq!(grid.sample_cells, rebuilt.sample_cells, "cloud at {origin:?}");
                // Cells beside the ground or a wall, and cells that read
                // nothing at any age.
                assert!(grid.sample_cells.contains(&0) && grid.sample_cells.contains(&grid::EMPTY_VOXEL));
            } else {
                assert!(grid.sample_cells.is_empty(), "{style:?} cloud at {origin:?}");
            }
        }
    }
    let buried = BoxObstacles::new(vec![ObstacleBox::from_center_size(Vec3::ZERO, Vec3::splat(40.0))]);
    let grid = fill(&buried, Vec3::ZERO, Style::Volumetric).grid;
    assert!(grid.is_empty() && grid.sample_cells.len() == grid.len());
    assert!(grid.sample_cells.iter().all(|&c| c == grid::EMPTY_VOXEL));
}

#[test]
fn roofs_shade_sun_and_sky() {
    // Half the smoke under a 3 m roof covering z > 0.
    let level = BoxObstacles::new(vec![ObstacleBox::from_center_size(
        Vec3::new(0.0, 3.1, 10.0),
        Vec3::new(40.0, 0.2, 20.0),
    )]);
    let g = fill(&level, Vec3::ZERO, Style::Volumetric).grid;
    let rays = LightRays {
        sun: Some(Vec3::new(0.3, 1.0, -0.2).normalize()),
        sun_length: 60.0,
        sky_length: 20.0,
    };
    let light = visibility(&g, &level, &Units::METRES_Y_UP, &rays);
    let at = |pos: Vec3| {
        let c = ((pos - g.min) / g.voxel_size).floor().as_uvec3();
        light[g.index(c)]
    };
    let [sun_out, sky_out] = at(Vec3::new(0.0, 1.0, -2.0));
    let [sun_in, sky_in] = at(Vec3::new(0.0, 1.0, 2.5));
    assert!(sun_out > 200 && sun_in < 60, "sun {sun_out} vs {sun_in}");
    assert!(sky_out > 200 && sky_in < sky_out / 2, "sky {sky_out} vs {sky_in}");
}

/// The brief's "split fills" test: the same cloud filled in one tick, and
/// spread over about 200 ticks, gives identical data and identical queries.
#[test]
fn split_fills_are_identical() {
    let level = common::level();
    for style in [Style::Volumetric, Style::Faceted, Style::Crystal] {
        for (origin, seed) in common::pops().into_iter().take(4) {
            let spec = CloudSpec::new(origin, 1000, seed, style);
            let whole = fill_cloud(&config(), &spec, &level);

            // A budget that takes a couple of hundred ticks, with the start
            // far enough out that the fill is never forced to finish.
            let mut cfg = config();
            cfg.fill_budget = match style {
                Style::Volumetric => 1_500,
                _ => 5_000,
            };
            cfg.tuning.timeline.start = 4.0;
            cfg.tuning.timeline.lifetime = 22.0;
            let mut smokes = Smokes::new(cfg.clone(), &level);
            let id = smokes.pop(spec);
            let mut ticks = 0;
            let mut tick = 1000;
            while smokes.pending_fills() > 0 {
                smokes.advance(tick);
                tick += 1;
                ticks += 1;
            }
            assert!(ticks > 100, "{style:?} fill took only {ticks} ticks");
            let split = smokes.cloud(id).and_then(|c| c.shape().cloned()).expect("filled");
            assert_eq!(*split, whole, "{style:?} cloud at {origin:?} differs");

            // And every query result, with the one-tick fill inserted.
            let mut whole_world = Smokes::new(cfg, &level);
            whole_world.insert(spec, Arc::new(whole));
            let mut rng = common::Rng(seed as u64);
            for tick in (1000..1000 + 22 * 128).step_by(97) {
                for _ in 0..20 {
                    let a = rng.point(origin - Vec3::new(9.0, 0.0, 9.0), origin + Vec3::new(9.0, 4.0, 9.0));
                    let b = rng.point(origin - Vec3::new(9.0, 0.0, 9.0), origin + Vec3::new(9.0, 4.0, 9.0));
                    let x = smokes.occlusion(a, b, tick);
                    let y = whole_world.occlusion(a, b, tick);
                    assert_eq!(x.to_bits(), y.to_bits(), "tick {tick}: {x} vs {y}");
                }
            }
        }
    }
}

/// Splitting differently (any budget) never changes the cloud: a faceted
/// one in the corridor, and a volumetric one in the room, padded against
/// its walls.
#[test]
fn any_budget_gives_the_same_cloud() {
    let level = common::level();
    for (pop, style) in [(1, Style::Faceted), (3, Style::Volumetric)] {
        let (origin, seed) = common::pops()[pop];
        let spec = CloudSpec::new(origin, 0, seed, style);
        let whole = fill_cloud(&config(), &spec, &level);
        for budget in [1, 7, 333, 4_096, 50_000] {
            let mut cfg = config();
            cfg.fill_budget = budget;
            // Never forced, never expired.
            cfg.tuning.timeline.start = 1000.0;
            cfg.tuning.timeline.lifetime = 2000.0;
            let mut smokes = Smokes::new(cfg, &level);
            let id = smokes.pop(spec);
            let mut tick = 0;
            while smokes.pending_fills() > 0 && tick < 2_000_000 {
                smokes.advance(tick);
                tick += 1;
            }
            let shape = smokes.cloud(id).and_then(|c| c.shape().cloned()).expect("filled");
            assert_eq!(*shape, whole, "{style:?}, budget {budget}");
        }
    }
    let padded = |pop: usize| {
        let (origin, seed) = common::pops()[pop];
        let shape = fill_cloud(&config(), &CloudSpec::new(origin, 0, seed, Style::Volumetric), &level);
        shape
            .grid
            .occupancy
            .iter()
            .filter(|&&o| o == grid::occupancy::PADDED)
            .count()
    };
    assert!(padded(3) > 0, "the room's cloud isn't padded");
}

/// A fill that hasn't finished when the cloud starts to grow is finished on
/// the spot, with the same result.
#[test]
fn due_fills_finish_on_demand() {
    let level = common::level();
    let (origin, seed) = common::pops()[0];
    let spec = CloudSpec::new(origin, 50, seed, Style::Faceted);
    let whole = fill_cloud(&config(), &spec, &level);
    let mut smokes = Smokes::new(config(), &level);
    let id = smokes.pop(spec);
    let start = smokes.cloud(id).map(|c| c.start_tick()).unwrap_or(0);
    // One tick of work, then nothing until the cloud is due (a stalled
    // server, say).
    smokes.advance(50);
    assert_eq!(smokes.pending_fills(), 1);
    // A query at the start tick needs the cloud.
    let _ = smokes.occlusion(origin - Vec3::X * 5.0, origin + Vec3::X * 5.0, start);
    assert_eq!(smokes.pending_fills(), 0);
    let shape = smokes.cloud(id).and_then(|c| c.shape().cloned()).expect("filled");
    assert_eq!(*shape, whole);
}

/// A fill that can't finish by the time its cloud starts at the normal
/// budget gets a bigger one, spread over the ticks it has left, instead of
/// finishing all at once when it is due.
#[test]
fn fills_speed_up_to_meet_their_start() {
    let config = Config::new(Units::METRES_Y_UP, 128.0);
    let mut smokes = Smokes::new(config, FlatGround::default());
    // A faceted fill takes about 40 ticks of the normal budget, but this
    // cloud starts 0.1 s (13 ticks) after the pop.
    let id = smokes.pop(CloudSpec::new(Vec3::ZERO, 0, 9, Style::Faceted));
    let start = smokes.cloud(id).map(|c| c.start_tick()).unwrap_or(0);
    assert!(start < 20, "start tick {start}");
    let mut finished = None;
    for tick in 0..start {
        smokes.advance(tick);
        if finished.is_none() && smokes.pending_fills() == 0 {
            finished = Some(tick);
        }
    }
    // Done before it was due, and not long before (so the work was spread).
    let finished = finished.expect("finished before the start tick");
    assert!(finished + 4 >= start, "finished at tick {finished}, due at {start}");
}
