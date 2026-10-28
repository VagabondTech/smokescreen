# Smokescreen

[![CI](https://github.com/VagabondTech/smokescreen/actions/workflows/ci.yml/badge.svg)](https://github.com/VagabondTech/smokescreen/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/bevy_smokescreen.svg)](https://crates.io/crates/bevy_smokescreen) [![docs.rs](https://docs.rs/bevy_smokescreen/badge.svg)](https://docs.rs/bevy_smokescreen)

Smoke that fills rooms, pours through doorways, stops at walls and blocks sight. **`bevy_smokescreen`** brings it to **Bevy 0.20**: smoke grenades, smoke walls, poison gas, fog banks and steam, in three looks, with gameplay queries that agree with what players see. Underneath is **`smokescreen`**, an engine-free simulation that gives bit-identical answers on x86-64 and ARM (checked in CI), so it works for multiplayer games and dedicated servers too.

**Status:** early (0.1), for Bevy 0.20 only. Tested on an Apple M4 (macOS) and a software rasteriser, and used in a game on an AMD RX 6700 XT (Windows 10); NVIDIA and Intel GPUs haven't been tried yet.

![A smoke grenade is thrown into a yard, bounces and pops; the cloud floods the yard, bullets carve tunnels through it, and an HE grenade clears a sphere that refills from the edges](https://raw.githubusercontent.com/VagabondTech/smokescreen/main/docs/media/hero.webp)

*The demo (`examples/demo.rs`) on an Apple M4. **[Play it in your browser](https://vagabondtech.github.io/smokescreen/)** (it needs WebGPU: a current Chrome, Edge or Safari 26; on a phone, drag to look and tap to shoot), or natively with `cargo run --release --example demo`. To add it to your game, see [Quick start](#quick-start).*

**Volumetric**: soft, light-scattering smoke, lit by the sun and self-shadowed.

![Volumetric smoke seen from a slow orbit: bullet tunnels, then an HE grenade's cleared sphere](https://raw.githubusercontent.com/VagabondTech/smokescreen/main/docs/media/volumetric.webp)

**Faceted** (experimental): hard-edged, tumbling polyhedral puffs merged like metaballs. It costs many times more to draw than the other two looks; see [Looks](#looks).

![Faceted smoke: hexagonal bullet tunnels, then an HE grenade's cavity](https://raw.githubusercontent.com/VagabondTech/smokescreen/main/docs/media/faceted.webp)

**Crystal**: tumbling polyhedra that don't merge, drawn as ordinary meshes. The cheapest look to draw and to check, and what players see is exactly what blocks.

![Crystal smoke: bullets and an HE grenade knock the crystals out of the way](https://raw.githubusercontent.com/VagabondTech/smokescreen/main/docs/media/crystal.webp)

*Recorded frame by frame by the demo (`SMOKE_DEMO_SCRIPT=video`); `tools/encode_clips.sh` turns the frames into these animations.*

## What you get

- Clouds that flood rooms, follow corridors, pour through doorways and off ledges, and stop at walls, dense right up to them.
- Presets: smoke grenades, smoke walls, poison gas, fog banks and steam vents, or your own mix of volume, lifetime, colour and look.
- Bullets carve tunnels that close again; explosions clear a sphere that refills from the edges.
- Lit by the sun with self-shadowing, darker indoors, lit from inside by muzzle flashes and explosions (any Bevy `PointLight`).
- **Gameplay sees the same smoke.** `blocks_sight(a, b)` and `occlusion(a, b)` for AI, server-side visibility and audio; `density_of(cloud, p)` for gas damage.
- **Level setup without collision code:** mark your level's meshes with `SmokeObstacle`.
- **One performance dial per camera**, or a GPU millisecond budget that turns it for you. No dial step changes a cloud's shape, so a cheaper setting doesn't show through smoke a better one hides (one exception: the experimental faceted look's bullet tunnels, see [Limitations](#limitations)); volumetric smoke's noise falls a little differently from step to step (in the demo's views, at most 0.65 % of a smoke's pixels let more than 30 % more of the scene through than at the best step).
- **Deterministic core for multiplayer** (`smokescreen`): engine-free, runs on your ticks and in your units, bit-identical on x86-64 and ARM, with a per-tick CPU budget.

| Round walls and down corridors | Indoors, through a doorway | Off a ledge |
|---|---|---|
| ![From above, with the roof hidden: one cloud fills a corridor, another pours into a building](https://raw.githubusercontent.com/VagabondTech/smokescreen/main/docs/media/cutaway.webp) | ![From inside a room: smoke pours in through the doorway, darker indoors](https://raw.githubusercontent.com/VagabondTech/smokescreen/main/docs/media/rooms.webp) | ![A cloud on a raised platform pours down its edge to the ground](https://raw.githubusercontent.com/VagabondTech/smokescreen/main/docs/media/ledge.webp) |

![Presets](https://raw.githubusercontent.com/VagabondTech/smokescreen/main/docs/presets.jpg)

*`examples/presets.rs`: three grenades (volumetric, faceted, crystal), a smoke wall behind a low wall, gas kept in a room, a fog bank and a steam vent. The walls and floor are plain meshes marked `SmokeObstacle`.*

## Quick start

```toml
[dependencies]
bevy = "0.20"
bevy_smokescreen = "0.1"
```

| `bevy_smokescreen` | Bevy | Minimum Rust |
|---|---|---|
| 0.1 | 0.20 | 1.97.1 |

Smoke needs compute shaders: native (Vulkan, Metal, DX12) or WebGPU, not WebGL2. On WebGL2 the game still runs, with crystal smoke only, and the log says why.

```rust,no_run
use bevy::prelude::*;
use bevy_smokescreen::prelude::*;

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, SmokePlugin::default()))
        .add_systems(Startup, setup)
        .add_systems(Update, bot_vision)
        .run();
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.spawn((
        Camera3d::default(),
        SmokeCamera::HIGH, // the performance dial
        Transform::from_xyz(0.0, 1.7, -10.0).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight::default(),
        SmokeSun, // the light that lights the smoke
        Transform::from_xyz(-4.0, 10.0, -6.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // Level geometry smoke flows around: just mark it.
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(50.0, 50.0))),
        MeshMaterial3d(materials.add(Color::srgb(0.6, 0.6, 0.6))),
        SmokeObstacle,
    ));

    // A smoke grenade popping at the origin.
    commands.spawn((Smoke::grenade(), Transform::from_xyz(0.0, 0.0, 0.0)));
}

fn bot_vision(vision: SmokeVision) {
    if vision.blocks_sight(Vec3::new(0.0, 1.6, -10.0), Vec3::new(0.0, 1.6, 10.0)) {
        // The bot can't see the player.
    }
}
```

`examples/minimal.rs` is the same with bullets, explosions and a level made of boxes instead of meshes.

If a setup step is missing, the plugin says so in the log, once, with the fix: no camera with `SmokeCamera`, no level, `SmokeObstacle` on an entity without a mesh (a glTF scene's root: put it on the scene's meshes), several directional lights and no `SmokeSun`, an orthographic camera, more smokes than `Tuning::max_clouds`, a source bigger than the fill grid, a server without `TransformPlugin`, and a few more.

The plugin's components, resources and messages are reflected and registered, so inspectors, editors and scene files see them. That includes glTF extras written by a Blender add-on, which can mark level meshes with `SmokeObstacle`. A smoke's style is the exception: it is the core's type, which can't be reflected from the plugin, so a smoke loaded from a scene takes `DefaultSmokeStyle`.

The API is documented on docs.rs: [`bevy_smokescreen`](https://docs.rs/bevy_smokescreen), and [`smokescreen`](https://docs.rs/smokescreen) for the engine-free core.

**With a coding assistant?** Point it at [`llms.txt`](https://raw.githubusercontent.com/VagabondTech/smokescreen/main/llms.txt): the API of both crates on one page, with recipes whose code is compiled as doc tests. The crate's package carries the copy for its version.

## Looks

Pick a default with the `DefaultSmokeStyle` resource, or per smoke with `Smoke::with_style`. The style is a rule, not a look: it decides what blocks, so every machine in a match must give a smoke the same one.

| | Volumetric | Faceted (experimental) | Crystal |
|---|---|---|---|
| Look | soft, light-scattering | hard facets, merged like metaballs | hard, separate tumbling crystals |
| Drawn by | raymarching at reduced resolution | sphere tracing a distance field | plain meshes (Bevy's PBR) |
| GPU cost² | low: 0.32 ms at mid-range, 0.75 ms filling the screen; one dial per camera | 20–40× volumetric: 6.6 ms at mid-range, 30 ms filling the screen, about 3× that with bullet tunnels | about 0.1 ms: a few thousand triangles per cloud |
| 250 sight checks through 4 of 32 clouds¹ | 0.03 ms | 0.04 ms, with any `FacetRule` | 0.02 ms (any contact blocks), 0.03 ms (0.8 m rule) |
| Bullets and explosions | tunnels and cleared spheres | hexagonal tunnels, cavities | knock crystals out of the way |
| Picture against the rule | the same density, but the noise thins some smoke the rule counts, most while it fades; `SmokeSettings::noise_within_rule` (`Capped` or `Filled`) stops that | never shows through what the rule blocks; thin edges drawn solid | exact with `sight_threshold = 0` |

¹ Fully grown clouds, on an Apple M4; the 2-core 2.1 GHz VM this was built on takes about 3× as long. Halfway through a cloud's fade, when many lines are only partly blocked, they cost more: 3× for volumetric smoke, 0.7 ms for the exact faceted rule, which traces more of them then (0.13 ms with `FacetRule::Chords`); see [Performance](#performance). `cargo bench -p smokescreen` measures it on your CPU.
² At the best step of the dial, at 1920×1080 on an Apple M4. Close up, the cheapest step costs a quarter of that for faceted smoke, and nearly as much for volumetric smoke, which marches at a quarter of the resolution there at every step. See [Performance](#performance).

**Faceted is experimental in 0.2.** It costs 20–40 times what a volumetric smoke costs to draw, and bullet tunnels make it about three times dearer again. At the dial's cheaper steps, tunnels you can see right through come out a few pixels wider than at the best, so players there see a little more through them (see [Limitations](#limitations)). Its simulation and rule are as finished as the others'; the label goes once tunnels cost less and every step draws them alike.

How each look works, and why faceted costs more than volumetric despite its hard edges, is in [docs/design.md](https://github.com/VagabondTech/smokescreen/blob/main/docs/design.md).

## Presets and sources

```rust,no_run
use bevy::prelude::*;
use bevy_smokescreen::prelude::*;

fn spawn_smokes(mut commands: Commands) {
    commands.spawn((Smoke::grenade(), Transform::from_xyz(0.0, 0.0, 3.0)));
    commands.spawn((Smoke::wall(Vec3::new(10.0, 0.0, 0.0)), Transform::from_xyz(-5.0, 0.3, 8.0)));
    commands.spawn((Smoke::gas(), Transform::from_xyz(4.0, 0.0, -2.0)));
    commands.spawn((Smoke::fog_bank(Vec3::new(6.0, 1.5, 4.0)), Transform::from_xyz(0.0, 1.5, 20.0)));
    commands.spawn((
        Smoke::steam_vent().with_style(SmokeStyle::Crystal).with_tint(Color::srgb(0.9, 0.95, 1.0)),
        Transform::from_xyz(8.0, 0.0, 0.0),
    ));
}
```

| Preset | What it does |
|---|---|
| `Smoke::grenade()` | Spreads from where it lands to a 90 m³ cloud, lasts 18 s (the default `Smoke`) |
| `Smoke::wall(offset)` | Spreads from every point between the entity and `offset` from it, lasts 25 s |
| `Smoke::gas()` | Greenish, thinner-looking, spreads over 4 s, lasts 25 s; hurt players in it with `SmokeVision::density_of` |
| `Smoke::fog_bank(half_extents)` | Fills a box, feathered at the edges, for as long as the entity lives |
| `Smoke::steam_vent()` | White, thin, spreads slowly over 8 s, clears after 15 s |

All of them take the builders `with_style`, `with_volume`, `with_lifetime`, `with_grow_time`, `with_start` (a fuse before it grows), `with_fade_time`, `with_seed`, `with_source` (`SmokeSource::Point`, `Line` or `Box`), `with_pop_tick`, `with_tint`, `with_density` and `with_despawn_when_done`. Style, volume, the four times, seed, source and pop tick are part of the rules: they change what blocks, so in a multiplayer game every machine must use the same. Tint and density are looks. A source's offset and box are in world axes: turning the entity doesn't turn them.

## Gameplay

`SmokeVision` answers for the current tick, in world units:

- `blocks_sight(a, b)`: does smoke hide `b` from `a`? True once the segment passes through `Tuning::sight_threshold` of cloud (0.8 m by default; 0 blocks on any contact).
- `occlusion(a, b)`: how much cloud lies between them, every smoke added up.
- `transmittance(a, b)`: fraction of light that gets through, for audio and other soft effects.
- `density_at(p)`: how dense the smoke is at a point, every smoke added up.
- `density_of(cloud, p)`: how dense one smoke is there, for gas damage that plain smoke doesn't do (`Smoke::cloud()` gives its id).

It only reads, so systems that use it run in parallel. For many questions, take `vision.view()` once: the tick's `SmokeView`, which any number of threads can ask at once (`par_iter`) and your own functions can take; `drawn_view()` is the drawn tick's, for checks that must agree with the picture. It answers from views the plugin prepares when the clock moves (in `PreUpdate`) and again once the frame's pops and impacts are in (after `SmokeSystems::Simulate`), so every system in `Update` sees the same smoke whatever order they run in. A change made straight to `SmokeWorld` in `Update` shows from then on, or at once after `world.prepare_vision(&clock)`; for another tick (lag compensation), ask `SmokeWorld::view(tick)`.

![Five bots look at a target across open ground; their sight lines turn red as a smoke grows between them, and a bullet fired along one line briefly turns it green again](https://raw.githubusercontent.com/VagabondTech/smokescreen/main/docs/media/sightlines.webp)

*Each line is green while `blocks_sight` says the bot can see the target's head. Holes count: a bullet's tunnel opens a line of sight until it closes.*

The rule is the same function on every machine and in every look's terms: for volumetric smoke the density integrated along the segment, for faceted and crystal smoke the length of the segment inside the puffs. Bullets (`SmokeBullet`) and explosions (`SmokeExplosion`) are messages; holes take part in the rule.

A bullet or explosion lands on the current tick, or on its own `tick`: the shot's, for a shot heard late or replayed.

For sounds, effects and UI, the plugin triggers `SmokePopped` on a smoke's entity when its cloud pops, and `SmokeEnded` when the cloud is gone, before the entity despawns. Observe them for every smoke (`app.add_observer(|popped: On<SmokePopped>| …)`) or for one (`.observe(…)` on its entity); each carries the entity, the cloud and the tick. They come in `PostUpdate` before Bevy propagates transforms, so what an observer spawns is placed that frame. A pop heard late is announced at once with its own tick, so a sound that would come too late can be left out.

## Multiplayer and servers

The simulation is its own crate, `smokescreen` (`smokescreen = "0.1"`, Rust 1.88 or newer), with no Bevy in it. A dedicated server links just that:

```rust,ignore
use smokescreen::*;

let mut config = Config::new(Units::INCHES_Z_UP, 128.0); // your units and tick rate
config.tuning.sight_threshold = 32.0;                     // 32 inches of cloud hide a player
let mut smokes = Smokes::new(config, level);             // `level`: your collision, behind the `Level` trait

smokes.pop(CloudSpec::new(origin, tick, seed, Style::Crystal));
smokes.bullet(muzzle, impact, tick);
smokes.advance(tick);                                     // every tick: spend the fill budget
let view = smokes.view(tick);                             // `Copy + Sync`: share it between threads
let hidden = view.blocks_sight(eye, enemy_head);
```

`smokescreen/examples/server.rs` is a complete headless loop. A Bevy server runs the plugin without a window, a renderer or assets: it draws nothing and does none of the drawing's work.

```rust,no_run
use std::time::Duration;

use bevy::{app::ScheduleRunnerPlugin, prelude::*};
use bevy_smokescreen::prelude::*;

fn main() {
    // The level every machine shares, voxelised once (`VoxelObstacles::to_bytes`).
    let bytes = std::fs::read("level.voxels").expect("the level");
    let level = VoxelObstacles::from_bytes(&bytes).expect("the level's voxels");
    App::new()
        .add_plugins((
            // A frame per tick, and nothing to draw with.
            MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / 128.0))),
            // Places smokes where their entities are.
            TransformPlugin,
            SmokePlugin::default(),
        ))
        .insert_resource(SmokeLevel::new(level))
        .run();
}
```

Gameplay systems ask `SmokeVision` as in a game. `SmokeObstacle` works on a server too, given Bevy's `AssetPlugin` and `MeshPlugin` and the level's meshes, but shipping the voxels is lighter and gives every machine the same level.

- **Your ticks.** A cloud at tick *t* is a pure function of its `CloudSpec` (origin, pop tick, seed, style, source, volume, timeline), the level, the `Tuning` and *t*; holes are events with ticks. Events can arrive late or in any order, late joiners compute the same clouds from the same events, and a `view` of a recent tick answers as it would have then (lag compensation).
- **Your units.** `Units { up, metres_per_unit }`: Y-up metres, Z-up inches, or anything else.
- **Bit-identical on x86-64 and ARM.** Only exact float arithmetic in a fixed order, `libm` for transcendental functions, no fused multiply-add, no SIMD types, no hash-map iteration; `smokescreen/clippy.toml` enforces it. `smokescreen/tests/determinism.rs` hashes 10,000 query results across all styles, and a level voxelised from meshes with clouds filled in it; the hashes match on x86-64 (debug, release, `-C target-cpu=native`) and aarch64. CI runs them on macOS, Linux and Windows.
- **A per-tick CPU budget.** A cloud's fill runs a little each tick (`Config::fill_budget`, about 0.2 ms); it speeds up only if the cloud is about to start growing. The finished cloud is the same however the work is split.
- **Networking.** Send the `CloudSpec` of each pop (a couple of dozen bytes) and each bullet and explosion with its tick; every machine computes the same clouds, as long as they share the `Tuning` and the same level answers. A late joiner needs the spec of each cloud still alive and `Smokes::impacts()`: every bullet and explosion the world keeps, exactly as it was given (`Smokes::impact` adds each one back). With the `serde` feature of either crate (off by default), `CloudSpec`, `Impact`, `SmokeBullet` and `SmokeExplosion` implement `Serialize` and `Deserialize` (bevy_replicon and lightyear want them). Binary formats such as postcard and bincode carry every number exactly, and so does `serde_json` for every finite `f32` (all of them checked). For mesh-based levels, voxelise once and ship the voxels (`VoxelObstacles::to_bytes`). A level voxelised at the tuning's voxel size (`VoxelObstacles::from_level`) gives exactly the clouds its source level would, faster: cloud grids lie on the same world lattice. `fill_cloud` runs a whole fill outside the world (on a worker thread, say) for `Smokes::insert`, but a `CloudShape` has no wire format: between machines, send the `CloudSpec`.

In Bevy, `SmokeWorld` is the `Smokes` the plugin runs, and `SmokeClock` sets the tick (by default from Bevy's virtual time; set `auto: false` to drive it from your netcode, with `draw_tick` for an interpolation delay and `draw_fraction` for how far the frame is past that tick). Write it in `First`, before the plugin prepares `SmokeVision`'s views in `PreUpdate`, or call `world.prepare_vision(&clock)` after writing it (a tick per fixed step, say); the plugin warns if `SmokeVision` is asked after the clock has moved on. A `Smoke` pops on that tick. For a cloud the server announced, spawn `Smoke::from_spec(spec)` with the `CloudSpec` it sent: you get the server's cloud to the bit however late you hear of it, with your own look (`with_tint`), and the entity despawns when the cloud is gone. Where every machine spawns the same `Smoke` from the same event instead (lockstep, rollback), `with_pop_tick(tick)` gives it the event's tick, and a `SmokeBullet` or `SmokeExplosion` takes its event's `tick` the same way. Volumetric smoke's noise moves on those ticks too, not on each machine's clock, so players who draw the same tick see the same billows. `SmokePlugin::new(config)` takes the same `Config`, so a Bevy client in Z-up inches works too:

![Units](https://raw.githubusercontent.com/VagabondTech/smokescreen/main/docs/units.jpg)

*`examples/units.rs`: the same scene built in Y-up metres (left) and in Z-up inches (right), volumetric above and faceted below, gives the same smoke.*

## Performance

Measured on an Apple M4 (Mac mini) at 128 ticks a second. Run `cargo bench -p smokescreen` on your own hardware. A 2-core 2.1 GHz cloud VM takes 2.5 to 3.5 times as long. On x86-64, building with `-C target-cpu=x86-64-v2` or newer turns rounding into single instructions instead of library calls; results are the same on every CPU target.

**Fills**, per cloud, in a voxelised level (the median of 11 runs):

| | Volumetric | Faceted | Crystal |
|---|---|---|---|
| All at once | 0.54 ms | 1.1 ms | 1.1 ms |
| Default timeline (the cloud starts 0.1 s after the pop): per tick, until it starts | median 0.05 ms, worst 0.07 ms, for 14 ticks | median 0.10 ms, worst 0.16 ms, for 13 ticks | median 0.09 ms, worst 0.16 ms, for 13 ticks |
| A start of 0.4 s or later: per tick | as above | median 0.04 ms, worst 0.08 ms, for 29–33 ticks | median 0.03 ms, worst 0.07 ms, for 29–33 ticks |

The fill budget (`Config::fill_budget`, about 0.2 ms a tick on the VM, under 0.1 ms on the M4: it counts work, not time) grows only when a fill would otherwise not be ready by the time its cloud starts. Faceted and crystal fills need about 0.26 s of ticks, so give those clouds a later `Timeline::start` to keep to the budget.

**Sight checks**: 250 a tick, each through 4 of 32 clouds, in milliseconds (the median of 11 runs):

| | Growing | Grown | Halfway through the fade |
|---|---|---|---|
| Volumetric | 0.03 | 0.03 | 0.09 |
| Volumetric, `VolumeSteps::HalfVoxel` | 0.05 | 0.05 | 0.14 |
| Faceted, `FacetRule::Exact` | 0.04 | 0.04 | 0.67 |
| Faceted, `FacetRule::Core` | 0.04 | 0.04 | 0.15 |
| Faceted, `FacetRule::Chords` | 0.04 | 0.04 | 0.13 |
| Crystal, 0.8 m rule | 0.03 | 0.03 | 0.05 |
| Crystal, any contact blocks | 0.02 | 0.02 | 0.04 |

Halfway through the fade the clouds are thin, so many lines never reach the threshold and are integrated all the way through. The volumetric rule samples a line every voxel; with `Tuning::volume_steps` set to `HalfVoxel` it samples every half voxel, which counts a little more precisely and costs about 1.5 times as much. The exact faceted rule settles most lines from exact chord bounds (the puffs' hard polyhedra lie inside the cloud, and the polyhedra grown by the blend radius contain it) and traces only lines within 2 cm of the threshold, which are commoner in the fade. `FacetRule::Chords` counts only the inner of those bounds, each puff's hard polyhedron, and never traces, so halfway through the fade it costs what the core rule costs. Before the first check of a tick, the clouds' state at that tick (puff positions, holes) is computed once. For these 32 clouds, viewed every tick, that costs 0.047 ms a tick for faceted clouds and 0.030 ms for crystal ones. For volumetric clouds it costs 0.0005 ms once they are grown, and 0.005 ms while all 32 fade, as each then builds its density table again (about 0.15 µs each). The first state of a tick in a fresh slot of the cache costs about twice as much (0.01 ms for volumetric clouds, whose tables it all builds); the bench prints both.

**On the GPU**: what the smoke adds to a 1920×1080 frame with MSAA on an Apple M4 (10-core GPU), at the best and the cheapest step of the quality dial:

| The smoke | Volumetric | Faceted (experimental) | Crystal |
|---|---|---|---|
| far away (27 m) | 0.20 → 0.07 ms | 2.7 → 1.06 ms | 0.07 ms |
| mid-range (14 m) | 0.32 → 0.16 ms | 6.6 → 3.1 ms | 0.06 ms |
| close, covering two-thirds of the screen | 0.75 → 0.66 ms | 30 → 7.5 ms | 0.1 ms |

These come from the demo's tier sweep (`SMOKE_DEMO_SCRIPT=tiers`): frame time with the smoke minus the same view without, every frame waiting for the GPU, the median of five alternated runs (ten, in two batches, for volumetric smoke). Close up, every volumetric step marches at a quarter of the resolution, as the rays there see smoke no farther than a quarter allows (`SmokeCamera::measured_depth`), so the dial saves little: 0.62–0.66 ms at the three cheapest steps. With MSAA they hold while nothing transparent is on screen: the smoke then draws straight into the resolved image. While something transparent is drawn, the smoke draws into the multisampled target for Bevy's transparent pass to continue from, which adds about 0.2 ms a frame, at any distance. (Metal's GPU timestamp queries lose the device, so the sweep times whole frames on macOS; with GPU timestamps, on Vulkan or DX12, it and the demo's HUD report each pass's GPU time.) The dial is per camera (`SmokeCamera::quality`, or `SmokeCamera::budget(ms)` to fit a GPU time budget); [docs/design.md](https://github.com/VagabondTech/smokescreen/blob/main/docs/design.md#the-performance-dial) has every step. Crystal smoke is ordinary geometry, with no dial. Volumetric smokes that overlap on screen are marched together: two grenades side by side take 1.08 ms close up at the best step and 0.70 ms at mid-range (`SMOKE_DEMO_SWEEP_SMOKES=2`).

Point lights near volumetric smoke cost extra, as every sample within a light's range adds its light: close up at the best step, one muzzle flash inside a grown cloud costs about 12 % more (0.83 against 0.74 ms), and eight cost 1.8 times as much (1.33 ms). Faceted smoke, lit once per pixel, costs about the same with eight lights as with none.

## Tuning

### The rules (`Tuning`, the same on every machine)

`Tuning::new(&units)` converts these defaults to your units; set it in `SmokePlugin::new(config)` (or `Config::tuning` for `smokescreen`). Each smoke's style is a rule too: `DefaultSmokeStyle`, or `Smoke::with_style`.

| Field | Default | Effect |
|---|---|---|
| `grid_dims`, `voxel_size` | 48 × 24 × 48, 0.3 m | Fill grid per cloud (dimensions rounded up to multiples of 4) |
| `volume` | 90 m³ | The fill stops once this much is filled |
| `up_cost`, `down_cost` | 1.4, 0.9 | Spreading upwards costs more, downwards less: a dome that pours off ledges |
| `lumpiness`, `lump_size` | 0.3, 1.6 m | Noise on spreading costs, for a lumpy outline |
| `floor_fraction` | 0.25 | Fraction of the grid below the source |
| `timeline` | starts 0.1 s after the pop, full 1.4 s later, gone at 18 s after a 3 s fade | Per cloud via `Smoke` or `CloudSpec::timeline` |
| `facets`, `crystals` | 0.9 m spacing, 0.72 m puffs, 0.3 m blend (crystal: none) | Puff shape of the faceted and crystal looks |
| `facet_rule` | `Exact` | `Chords` or `Core` for cheaper faceted checks that never block what the picture leaves clear: `Chords` counts each puff's hard polyhedron, `Core` only its inscribed sphere |
| `volume_steps` | `Voxel` | `HalfVoxel` samples volumetric clouds every half voxel: a slightly more precise rule, at about 1.5 times the cost of a check |
| `holes` | bullets: 0.38 m, closed by 1.8 s; blasts: 3.5 m, refilled by 5 s | `None` turns holes off |
| `max_holes_per_cloud` | 16 | The newest win; the plugin draws up to 512 at once across all smokes (16 for each of 32) |
| `max_clouds` | 32 | Past this the oldest stop counting (and aren't drawn); also sizes the GPU atlas |
| `sight_threshold` | 0.8 m | Cloud a sight line must cross to be blocked; 0 blocks on any contact |

### The simulation (`Config`)

| Field | Default | Effect |
|---|---|---|
| `units`, `tick_rate` | set in `Config::new` (Bevy: Y-up metres, 128 ticks per second) | Your units, up axis and ticks |
| `fill_budget` | 20,000 | Work units per tick for fills, about 10 ns each. Performance only |
| `level_query_units` | 4 | Units charged per `Level` query (`is_blocked`, `is_inside`); raise it if yours are slow |
| `history_ticks` | one second | How long ended clouds and holes are kept, for views of past ticks |
| `cached_ticks` | 4 | Ticks whose snapshots are kept |

`Config::validate` checks for values that would only make nonsense: every number finite; the tick rate, the unit's length, the voxel size and the volume above 0; `floor_fraction` from 0 to 1; and a grid of at most 2^24 voxels. `Smokes::new` and `SmokePlugin` panic with its message, so call it first to handle a configuration read from a file.

### The look (`SmokeSettings`, change any time)

Nothing here changes what blocks, so it can differ from player to player (a graphics menu, say). Its lengths are in metres whatever `Config::units` says; only `wind` is in world units.

| Field | Default | Effect |
|---|---|---|
| `extinction` | 7.0 /m | Opacity of the volumetric picture. At 7 you can't see past ~0.5 m inside |
| `albedo` | 0.9 grey | Smoke colour; tint per smoke with `Smoke::tint` |
| `multi_scattering` | 1.0 | 0 = physically single-scattered (too dark); ~1 = bright, like CS2 |
| `ambient_occlusion` | 0.85 | Darkening deep inside and under the cloud |
| `indoor_ambient` | 0.5 | Ambient kept where the level hides the sky |
| `anisotropy` | 0.25 | Glow when looking towards the sun through thin smoke |
| `noise_frequency`, `erosion`, `detail_erosion` | 0.22, 0.6, 0.35 | Billow size and how ragged volumetric edges are |
| `noise_within_rule` | off | Keeps the volumetric noise from showing through what the rule blocks, for games whose server, bots or abilities use the rule: `Capped` looks smoother inside and costs less, `Filled` keeps the billows and costs more |
| `wind` | slight upward drift | Drift of the volumetric noise |
| `crystal_shadows` | on | Crystal puffs cast shadows |
| `debug_silhouette` | off | Draws faceted and crystal smoke flat magenta, to check silhouettes |
| `debug_transmittance` | off | Draws volumetric smoke as how much of the scene shows through it, to check it against the rule |

## Examples

| Example | What it shows |
|---|---|
| `minimal` | The smallest integration: a grenade, bullets, explosions, a bot's line of sight |
| `presets` | Every preset and look, in a yard made of `SmokeObstacle` meshes |
| `demo` | A playable test range, also [in your browser](https://vagabondtech.github.io/smokescreen/) with touch controls on a phone ([how it's built](https://github.com/VagabondTech/smokescreen/blob/main/web/README.md)): throw smoke and HE grenades (G, H), shoot, switch looks (F), turn the dial (1–6), budget mode (B); GPU timings in the HUD; scripted captures, and the clips in this README |
| `viewmodel` | A first-person weapon that stays visible inside smoke (a second camera) |
| `units` | The same scene in Y-up metres and Z-up inches |
| `smokescreen/examples/server` | A headless server loop in Z-up inches with 250 bots |

From a clone of the repository, run one with `cargo run --release --example presets`.

## Limitations

- A first-person weapon needs its own camera to stay visible inside smoke (see `viewmodel`).
- Transparent (alpha-blended) objects behind or inside volumetric or faceted smoke draw over it at full strength, so a tracer, a muzzle-flash sprite or particles behind a smoke show through it: Bevy draws them after the smoke, which writes no depth. Opaque and alpha-masked (`AlphaMode::Mask`) objects are hidden as they should be, crystal smoke hides everything behind it, and transparent objects in front of a smoke draw over it as they should. For effects that must stay hidden, use one of those, or hide them where the rule blocks the line from the camera (`SmokeVision::blocks_sight`).
- Volumetric and faceted smoke are lit by one directional light and up to eight point lights at a time (unshadowed; the brightest of those whose range reaches both a smoke and a camera's view); spot lights don't light them yet. Crystal smoke takes any Bevy light.
- Orthographic cameras aren't supported: volumetric and faceted smoke aren't drawn through them (the log says so); crystal smoke is.
- Clouds don't drift with wind, and doors closing on a smoke don't re-shape it.
- Smoke fills right up to walls and floors at least a voxel thick (0.3 m by default), wherever they sit on the voxel grid. Against thinner walls it stops at the middle of the first voxel they reach into, so up to about half a voxel can stay clear in front of them, and as much can show past them; picture and rule agree. Level meshes (`SmokeObstacle`) need triangles facing outwards for this, as Bevy's shapes do; against a mesh facing inwards, or a wall made of a single vertical sheet, smoke stops up to half a voxel short. A single sheet facing up (a `Plane3d`, say) counts as ground, solid underneath, so up to a voxel of smoke can show beneath a raised floor made of one: use a box for floors with open air below.
- Faceted smoke is experimental. It costs 20–40 times what volumetric smoke costs to draw (13 times far away), and bullet tunnels make it about three times dearer again: one faceted smoke with 16 tunnels takes about 83 ms close up at the best step on an Apple M4. At the dial's cheaper steps, tunnel mouths you can see right through come out a few pixels wider than at the best step, so a player there sees slightly more through them than one at the best.
- Faceted and crystal smoke can still stop up to about a voxel short of walls (see [ROADMAP.md](https://github.com/VagabondTech/smokescreen/blob/main/ROADMAP.md)).
- With the default 0.8 m rule, faceted and crystal pictures draw thin edges solid where the rule sees through.
- Tested on an Apple M4 (Metal) and a software rasteriser, and used in a game on an AMD RX 6700 XT (Windows 10); NVIDIA and Intel GPUs haven't been tried yet. The browser demo runs in Chrome on the M4 and in Safari 26 on an iPhone 15 Pro, touch controls included; Firefox, Edge and Android haven't been tried yet.
- Safari 26 can't compile Bevy 0.20's `StandardMaterial` shader for meshes with UVs: it keeps textures in `let`s, which needs WGSL's `texture_and_sampler_let`, and the screen stays black. That's Bevy's shader, not the smoke's; crystal puffs have no UVs, and neither do the demo's meshes. Textured meshes need a fix in Bevy.

[ROADMAP.md](https://github.com/VagabondTech/smokescreen/blob/main/ROADMAP.md) has the plan. [CONTRIBUTING.md](https://github.com/VagabondTech/smokescreen/blob/main/CONTRIBUTING.md) explains how to build, test and keep the core deterministic.

## Versions

- **Results.** Releases within 0.2 avoid changing what the simulation computes where they can: a change shows in the determinism test's hashes, and the changelog says so. To be safe, run the same version on every machine in a match.
- **The API.** Later 0.2 releases may add to it without breaking code written the way these docs write it. Enums marked `#[non_exhaustive]` (`SmokeStyle`, `SmokeSource`, `NoiseWithinRule` and others) may gain variants, so match them with a `_` arm. Structs whose fields are all public may gain fields, so build them from a constructor (`SmokePlugin::new`, `Config::new`, `SmokeCamera::HIGH`) or with `..default()` (`SmokeSettings`, `SmokeClock`, `SmokeBullet`, `SmokeExplosion`). A `Smoke` is built with its presets and builders. `CloudSpec` and `Impact` gain fields only in a new 0.x: every machine must send them, so a new one should stop code that sends them field by field from compiling. `SmokePopped` and `SmokeEnded` may gain fields: read the ones you need.
- **Serialised and reflected forms** follow the types: a 0.2 release may add a variant or a field where the type may. Send serialised specs and impacts between machines running the same version, as for results. A scene file saved by an earlier 0.2 release still loads: a field it lacks takes its default.
- **Hidden items.** What docs.rs leaves out is for the plugin and the tests, and may change in any release; that is why `bevy_smokescreen` depends on exactly one version of `smokescreen`.
- **Bevy.** `bevy_smokescreen` 0.2 is for Bevy 0.20 only (the table in [Quick start](#quick-start)).

## License

Dual-licensed under [MIT](https://github.com/VagabondTech/smokescreen/blob/main/LICENSE-MIT) or [Apache 2.0](https://github.com/VagabondTech/smokescreen/blob/main/LICENSE-APACHE), at your option.

Smokescreen is made by [VagabondTech](https://github.com/VagabondTech).
