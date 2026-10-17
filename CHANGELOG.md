# Changelog

Changes that alter simulation results (and so break lockstep with older
versions) are marked **results**.

## 0.1.0 (unreleased)

The first version, for Bevy 0.20.

### Since the first snapshots

For anyone already running a copy from before the repository existed:

- **results** Volumetric smoke against walls and floors is as dense as the
  smoke in front of them, right up to the middle of the first solid voxel.
  It used to fade out over the last voxel before every surface (solid
  voxels are empty), and the noise then frayed that layer to nothing:
  players could see along walls through smoke the rule still partly
  counted. The rule
  (`Grid::sample_distance`) and the picture (an open mask per voxel,
  `Grid::open_texels`) both average only the open voxels around a point.
  The fill also asks the level about every neighbour of its last voxel.
  The determinism hash changed: update servers and clients together.
- **results** Volumetric smoke also fills right up to walls and floors that
  only clip the first solid voxel. It used to stop at that voxel's middle,
  up to 12 cm short of the wall (and with level meshes up to 15 cm, even
  for walls on the voxel grid facing −x or −z): a see-through layer along
  it. The fill pads such voxels (`occupancy::PADDED`) wherever that can't
  show smoke past a wall; against walls thinner than a voxel it stops as
  before. The determinism hash changed again.
- `Level::is_inside`: is a point inside? By default `is_blocked(point,
  0.0)`. `VoxelObstacles` records the answers, in a new byte format (the old
  one still loads, without the padding), and `from_triangles` works them out
  from which way the triangles face, counted down a new `up` argument: they
  should face outwards, as Bevy's shapes do, and a single sheet facing up
  counts as ground. `SmokeObstacle` keeps meshes under a mirroring transform
  facing outwards. Voxelising meshes takes about a third longer.
- `VoxelObstacles` built at the tuning's voxel size gives exactly the clouds
  its source level would (documented and tested), padding included.
- **results** That holds even where a wall's face lies exactly on a voxel's
  centre, or on the edge of the cube `BoxObstacles` tests (0.4 voxels from
  the centre): fills ask the level at points worked out from each voxel's
  index on the world lattice, as `VoxelObstacles` does. Before, a different
  rounding could flip such a voxel.
- Smoke against a shaded wall is no longer lit up along it, and smoke padded
  against a wall isn't darkened as if inside it.
- The crates are now `smokescreen` (was `smoke_core`) and
  `bevy_smokescreen` (was `bevy_smoke`).
- The demo throws grenades that fly, bounce off the level and go off (G and
  H no longer drop smoke where you aim), looks around with a click (Esc
  lets go), clears smokes with X, and has buttons for throwing, clearing,
  the style and the dial. C moves down
  instead of Ctrl, which closes a browser tab with W. Its bullet tracers
  are drawn: the examples lacked Bevy's `bevy_gizmos_render` feature. The
  map gains a window, a second room, a raised platform with steps, a low
  wall and more cover.
- The demo's scripted captures draw offscreen (`SMOKE_DEMO_WINDOW=1` for a
  window), take a size (`SMOKE_DEMO_SIZE`), and wait for every screenshot
  to be written before exiting. A `video` script records captioned clips
  frame by frame. Without GPU timings (macOS, where they lose the Metal
  device), the `tiers` sweep times whole frames, each waiting for the GPU,
  against the same view without smoke; `tools/fairness.py` reads either.
- The browser demo's page shows what went wrong (no GPU, WebGPU errors, a
  lost device, a panic) instead of a blank screen, shows download progress,
  and with `?log` in the address the whole log, to copy. The demo's
  settings work in the address too (`?smoke=0&ldr&msaa=0`), and two are
  new: `SMOKE_DEMO_MSAA` and `SMOKE_DEMO_SMOKE`.
- Crystal puffs have no UVs (they had all-zero ones): Safari 26 can't
  compile Bevy 0.20's PBR shader for meshes with UVs. The demo's meshes
  have none either, and its page reports shader compile errors, which Bevy
  drops in a browser.

### smokescreen

- Engine-free simulation on caller ticks and in caller units (`Units`: any
  up axis and scale). Bit-identical results on x86-64 and aarch64, checked
  by a hash of 10,000 queries.
- Flood fill through any `Level` (boxes, voxels from boxes, a closure or
  triangles, a flat ground, or your own), spread over ticks within a work
  budget, the same however it is split.
- Sources: point (grenade), line (smoke wall) and box (fog bank, gas-filled
  room).
- Three styles: volumetric (soft density), faceted (polyhedral puffs merged
  like metaballs) and crystal (separate polyhedra).
- Bullet tunnels and explosion cavities as tick-stamped events; for crystal
  smoke, puffs shrink out of the way.
- The rule: `occlusion`, `blocks_sight` (against `Tuning::sight_threshold`,
  0.8 m by default) and `density_at`, every cloud added up; allocation-free;
  views of past ticks for lag compensation; `View` is `Copy + Sync`.
- `FacetRule::Core` for cheap, conservative faceted checks.
- `Tuning::max_clouds` (default 32): past it the oldest clouds stop counting.
- `VoxelObstacles::to_bytes` / `from_bytes` to ship a voxelised level.

### bevy_smokescreen

- `SmokePlugin`, the `Smoke` component with presets (`grenade`, `wall`,
  `gas`, `fog_bank`, `steam_vent`) and builders, `SmokeBullet` and
  `SmokeExplosion` messages, `SmokeVision` for gameplay systems,
  `SmokeClock` for driving smoke from your own netcode.
- `SmokeObstacle`: mark level meshes and the plugin voxelises them in the
  background.
- Volumetric look: reduced-resolution raymarch of a baked 3D atlas, lit by
  the sun (self-shadowed), the sky (darker indoors) and point lights.
- Faceted look: sphere tracing with coarse passes and full-resolution
  retracing of silhouettes and creases.
- Crystal look: plain meshes through Bevy's PBR renderer, with shadows.
- One performance dial per camera (`SmokeCamera::quality`) or a GPU budget
  (`SmokeCamera::budget`); no step changes a cloud's shape.
- Smoke is drawn after opaque and transmissive objects and before
  transparent ones, with or without MSAA; first-person weapons use a second
  camera (`examples/viewmodel.rs`).
- Examples: `minimal`, `presets`, `demo` (also for browsers with WebGPU;
  checked in Chrome on an Apple M4), `viewmodel`, `units`; `smokescreen`
  has a headless `server`.
