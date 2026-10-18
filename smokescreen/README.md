# smokescreen

A deterministic smoke simulation for games and their servers, with no engine
in it: smoke grenades, smoke walls, gas and fog banks that flood rooms, pour
through doorways and stop at walls, and the rule that decides whether smoke
hides one point from another. It gives the same clouds and the same answers,
bit for bit, on x86-64 and ARM (checked in CI).

It is the simulation behind [`bevy_smokescreen`](https://github.com/VagabondTech/smokescreen), which draws
it in Bevy. Use this crate on its own for a dedicated server, bots, a replay
tool or another engine.

```rust
use smokescreen::glam::Vec3;
use smokescreen::*;

// Z-up inches at 64 ticks a second; a flat floor at height 0.
let mut smokes = Smokes::new(Config::new(Units::INCHES_Z_UP, 64.0), FlatGround::new(0.0, UpAxis::Z));
let origin = Vec3::new(0.0, 0.0, 0.0);
smokes.pop(CloudSpec::new(origin, 0, CloudSpec::seed_from_origin(origin), Style::Crystal));

// Two seconds later the cloud has grown: it hides a player behind it.
let tick = 2 * 64;
smokes.advance(tick);
let view = smokes.view(tick);
assert!(view.blocks_sight(Vec3::new(-400.0, 0.0, 64.0), Vec3::new(400.0, 0.0, 64.0)));
```

`examples/server.rs` is a complete headless loop with 250 bots.

It needs Rust 1.88 or newer, and depends only on `glam` and `libm`:

```toml
[dependencies]
smokescreen = "0.1"
```

## What it does

- **Fill.** A Dijkstra flood fill through a small voxel grid around each
  cloud's source (a point, a line or a box), stopping at your level's walls
  (anything behind the `Level` trait) once it reaches the cloud's volume. It
  runs a little each tick within a CPU budget (`Config::fill_budget`, about
  0.2 ms), and the finished cloud is the same however the work was split.
- **Three styles.** Volumetric (a soft density), faceted (tumbling
  polyhedral puffs merged like metaballs) and crystal (separate polyhedra).
- **Holes.** Bullets carve tunnels and explosions clear spheres, as events
  with ticks; they close and refill over time.
- **The rule.** `occlusion(a, b)` is the amount of cloud along a segment,
  every cloud added up; `blocks_sight(a, b)` is whether that reaches
  `Tuning::sight_threshold`; `density_at(p)` is for gas damage. Queries
  allocate nothing, skip clouds the segment doesn't come near, and a `View`
  of one tick can be shared between threads.

## Multiplayer

- A cloud at tick *t* is a pure function of its `CloudSpec` (origin, pop
  tick, seed, style, source, volume, timeline), the level, the `Tuning` and
  *t*. Send the spec of each pop, and each bullet and explosion with its
  tick: every machine computes the same clouds. Events may arrive late or
  out of order, late joiners catch up from the same events, and views of
  recent past ticks answer as they would have then.
- The level must answer the same everywhere. For mesh levels, voxelise once
  (`VoxelObstacles::from_triangles`) and ship the bytes
  (`VoxelObstacles::to_bytes`).
- `Level` asks two things: whether a voxel-sized box is blocked
  (`is_blocked`), and whether a point is inside (`is_inside`, which by
  default asks `is_blocked` with a box of size zero). The second lets smoke
  fill right up to walls that only clip a voxel. If your collision code
  can't test a point, answer with `is_blocked(point, half a voxel)`: smoke
  then stops up to half a voxel short of such walls.
- Or fill on the server only (`fill_cloud`) and send the result to clients
  (`Smokes::insert`).

## Determinism

Only exact `f32` arithmetic in a fixed order, `libm` for transcendental
functions, no fused multiply-add, no SIMD types, no hash-map iteration, no
threads or clocks; `clippy.toml` enforces it. `tests/determinism.rs` hashes
10,000 query results across all styles, sources, holes and two unit systems,
and a level voxelised from meshes; the hashes match on x86-64 (debug,
release and `-C target-cpu=native`) and aarch64, and CI checks macOS, Linux
and Windows.

## Your units

`Units { up, metres_per_unit }`: Y-up metres, Z-up inches, or anything else.
Positions, lengths, volumes and results are in your units;
`Tuning::new(&units)` converts the defaults (a 90 m³ cloud, 0.3 m voxels,
0.8 m of cloud blocks sight).

## Cost

On an Apple M4: a volumetric fill is about 0.55 ms of work and a faceted or
crystal one about 1.2 ms, spread over ticks. 250 sight checks through 4 of
32 grown clouds take 0.03 ms (crystal, blocking on any contact) or 0.04 ms
(faceted, with either `FacetRule`, and volumetric). Halfway through a
cloud's fade they cost more: up to 3× as much, and 0.7 ms with the exact
faceted rule, which then traces more of the lines. A 2-core 2.1 GHz cloud VM
takes 2.5 to 3.5 times as long. The main README has the full tables, and
`cargo bench -p smokescreen` measures it on your machine. On x86-64,
`-C target-cpu=x86-64-v2` or newer turns rounding into single instructions
instead of library calls; results are the same on every CPU target.

## Licence

[MIT](https://github.com/VagabondTech/smokescreen/blob/main/LICENSE-MIT) or [Apache 2.0](https://github.com/VagabondTech/smokescreen/blob/main/LICENSE-APACHE), at
your option.
