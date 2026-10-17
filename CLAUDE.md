# CLAUDE.md

Notes for Claude Code sessions (and humans) working on this repository. Read
this first; `ROADMAP.md` has what is left to do.

## What this is

Smokescreen (https://github.com/VagabondTech/smokescreen, by VagabondTech):
smoke for games, as a plugin for Bevy 0.20, in two crates in one workspace:

- **`smokescreen/`**: the simulation, with no engine in it. Flood fill, a
  cloud's shape over time, bullet and explosion holes, and the sight rule
  (`occlusion`, `blocks_sight`). Runs on caller-supplied ticks, in caller
  units, bit-identical on every platform. Depends only on `glam` and `libm`.
- **`bevy_smokescreen`** (the repository root): the Bevy plugin. Draws what
  the core computes in one of three looks, wraps it in ECS conveniences
  (`Smoke` component and presets, `SmokeVision`, `SmokeObstacle`), and has a
  per-camera quality dial.

The three looks (`Style` in the core, `SmokeStyle` in Bevy):

| Style | Drawn by | Rule (`occlusion`) |
|---|---|---|
| Volumetric | raymarch of a baked 3D texture (`src/render.rs`, `src/shaders/smoke_{bake,march,composite}.wesl`) | density integrated along the segment |
| Faceted | sphere tracing of blended polyhedra (`src/shaders/smoke_facet*.wesl`) | chord through the blended SDF (`FacetRule::Exact`; `blocks_sight` decides most lines from exact chord bounds and traces those within 2 cm of the threshold: about half a volumetric check grown, twice one mid-fade) or through each puff's inscribed sphere (`FacetRule::Core`, cheap, conservative) |
| Crystal | ordinary meshes, one entity per puff (`src/crystal.rs`) | exact chord through the union of convex polyhedra (`smokescreen/src/chords.rs`); with `sight_threshold = 0` the picture and the rule agree exactly |

## Commands

```sh
cargo test -p smokescreen --release         # core tests, ~10 s
cargo test -p bevy_smokescreen              # builds every example, runs the README's examples as doc tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all                             # rustfmt.toml: max_width = 120
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo bench -p smokescreen                  # fill and query timings: the README's performance tables
cargo run --release -p smokescreen --example server
cargo run --example presets                 # every preset and look; also minimal, demo, units, viewmodel
```

- Bevy 0.20 needs Rust 1.97.1 (`rust-version` in `Cargo.toml`).
  `smokescreen` needs 1.88; clippy's `incompatible_msrv` lint flags newer std
  APIs, and CI builds it with 1.88. The workspace uses resolver 2 so that
  this lower `rust-version` doesn't hold Bevy's dependencies back.
- The README's and `smokescreen/README.md`'s Rust code blocks are compiled as
  doc tests (`ReadmeDoctests` in each `lib.rs`): keep them compiling, and
  mark blocks that can't stand alone `ignore`.
- The browser build of the demo: `web/README.md` and
  `.github/workflows/web.yml` (`--features webgpu`, `--profile web`).
- Scripted captures (screenshots, measurements) are driven by environment
  variables; see the module docs at the top of `examples/demo.rs`,
  `examples/units.rs`, `examples/presets.rs` and `examples/viewmodel.rs`.
  The demo's captures draw offscreen, without a window, so they work on a
  locked screen and without a display (a window's screenshots come out
  black while a Mac's screen is locked); its `video` script records the
  README's clips frame by frame. The other examples' captures need a
  window: on a headless Linux machine, `xvfb-run -a`.
- `tools/fairness.py` and `tools/agreement.py` (numpy, Pillow) turn the
  demo's `tiers` and `agreement` captures into reports.
- For quicker local builds you can add a `.cargo/config.toml` (ignored by
  git) with `lld` as the linker and lower `opt-level`s.

## Invariants: don't break these

1. **Determinism (`smokescreen`).** Only `+ - * /`, `sqrt`, comparisons,
   `floor`/truncation and integer maths on `f32`, in a fixed order. Sines,
   cube roots and the like come from `libm` (`smokescreen::math::{sin_cos,
   cbrt}`). No fused multiply-add, no glam SIMD types (`Vec3A`, `Quat`,
   `Mat4`, `Vec4`), no `HashMap`/`HashSet` iteration, no threads or clocks.
   `smokescreen/clippy.toml` forbids the std functions and types that would
   break this; keep clippy clean.
2. **The determinism hash.** `smokescreen/tests/determinism.rs` hashes
   10,000 query results over the lives of eight clouds (all three styles, a
   line and a box source, holes, metres and Z-up inches, both facet rules),
   and separately a level voxelised from meshes with clouds filled in it.
   `EXPECTED` and `EXPECTED_MESHES` must match on every platform. If you change results on
   purpose, re-record it and check it on x86-64 *and* aarch64 (CI runs
   macOS ARM, Linux and Windows). Locally, aarch64 under emulation:
   ```sh
   CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
   CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER="qemu-aarch64 -L /usr/aarch64-linux-gnu" \
   cargo test -p smokescreen --release --target aarch64-unknown-linux-gnu --test determinism
   ```
   Changes that shouldn't change results (speed-ups, refactors) must leave
   the hash alone: that is the regression test.
3. **Fills don't depend on how they are split.** The fill
   (`smokescreen/src/fill.rs`) is a state machine that stops after any small
   step. Work is charged in units of about 10 ns; a piece that can't be
   split calls `work.begin(n)`, which waits for a fresh tick rather than
   overrunning one. `split_fills_are_identical` and
   `any_budget_gives_the_same_cloud` check the result is the same whatever
   the budget.
4. **Queries allocate nothing** (`queries_do_not_allocate`). Use stack
   buffers (`holes::HOLE_BUFFER`, `chords::Intervals`).
5. **Fairness.** The picture must never show through anything the rule
   blocks, and no quality-dial step may change a cloud's shape (only how
   precisely it is drawn). Where they can't agree exactly, the picture
   errs towards more smoke. Check with the demo's `agreement` and `tiers`
   captures and the two tools.
6. **Units.** The core works in metres, Y up, internally; everything public
   is in the caller's units (`Units`). Renderers draw in the internal frame
   and convert per view.
7. **Holes are events with ticks.** Renderers read hole shapes from
   `CloudState::holes` (and crystal puff sizes from `CloudState::live`);
   never re-implement the timing curves in shaders.
8. **Samples skip solid voxels.** Volumetric density is the trilinear
   average of the *open* voxels only (`grid::sample_open`; on the GPU the
   filtered density divided by the filtered open mask, `smoke_open_atlas`),
   so smoke stays dense right up to walls. The fill pads solid voxels a
   wall only clips (`occupancy::PADDED`, open to samples; the `Pad*` stages
   in `fill.rs`), never where smoke could show past a wall
   (`unsafe_to_pad`). The core and the shaders must keep doing the same
   thing, and `VoxelObstacles` must keep answering `is_blocked` and
   `is_inside` exactly as its source would at the tuning's voxel size: the
   fill asks at points computed from each voxel's lattice index in the
   caller's units (`FillJob::level_center`), and `VoxelObstacles` computes
   its cells' centres the same way (`Lattice::centre`); keep the two in
   step to the last bit. `tests/walls.rs` covers all of it.
9. **Every cloud that counts is drawn.** `Tuning::max_clouds` (default 32)
   caps the clouds the rule counts; the renderer has 32 slots. Crystal
   clouds need no slot.

## Where things are

| Path | What |
|---|---|
| `smokescreen/src/world.rs` | `Smokes` (pops, holes, ticks, fill budget, snapshots, queries), `Tuning`, `Config`, `CloudSpec`, `CloudSource`, `View` |
| `smokescreen/src/fill.rs` | The fill state machine: spread, padding clipped wall voxels, bricks, halo, region and air distance transforms, puff seeding, binning; sources |
| `smokescreen/src/grid.rs` | The fill grid, bricks and front bounds for skipping, chamfer distance transforms |
| `smokescreen/src/volume.rs` | Volumetric rule: density integral, skipping empty bricks and inactive parts |
| `smokescreen/src/facets.rs` | Polyhedra, puffs, bins, the blended faceted SDF and its exact integrator |
| `smokescreen/src/chords.rs` | Crystal and faceted-core rules: exact chords through convex pieces; `ChordBounds`, the exact faceted rule's bounds for `blocks_sight` |
| `smokescreen/src/holes.rs`, `timeline.rs` | Hole events and shapes; a cloud's life and the density model |
| `smokescreen/src/level.rs` | `Level` trait, `FlatGround`, `BoxObstacles`, `VoxelObstacles` (from boxes, a closure or triangles, with winding numbers for which side of a mesh is solid; to and from bytes) |
| `smokescreen/src/light.rs` | Sun and sky visibility per voxel, for renderers |
| `smokescreen/tests/` | `fill`, `queries`, `styles`, `walls`, `determinism` |
| `src/lib.rs` | Plugin, `Smoke` and presets, `SmokeVision`, `SmokeClock`, systems |
| `src/render.rs` | Atlas, uploads, bake, raymarch, faceted passes, composite |
| `src/crystal.rs` | Crystal meshes and the per-frame puff sync |
| `src/obstacles.rs` | `SmokeObstacle`: voxelises marked meshes into the level |
| `src/quality.rs` | The dial and the budget controller |
| `src/shaders/` | WGSL (`.wesl`); `smoke_common.wesl` mirrors the `Gpu*` structs and limits in `render.rs` |
| `examples/` | `minimal`, `presets`, `demo` (also scripted captures and the README's clips), `viewmodel`, `units` |
| `tools/` | `agreement.py`, `fairness.py`: reports from the demo's captures |
| `web/` | The page that hosts the browser build of the demo |
| `docs/design.md` | How it works, measurements; keep it in step with the code |
| `docs/media/` | The README's clips: recorded by the demo's `video` script, encoded by `tools/encode_clips.sh`; remake them when the look changes |
| `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, `ROADMAP.md` | For users and contributors; record user-visible changes in the changelog |

## Gotchas

- The smoke composite runs in Bevy's `Core3d` schedule between
  `main_opaque_pass_3d` (and the transmissive pass) and
  `main_transparent_pass_3d`, drawing into `ViewTarget::get_color_attachment()`
  so it works with MSAA; the overlay pipelines are specialised on the sample
  count. Bevy's render internals change every release: expect this file to
  need work on each Bevy upgrade.
- Keep `MAX_SLOTS`, `MAX_HOLES`, `MAX_LIGHTS` and the `Gpu*` struct layouts
  in `render.rs` in step with `smoke_common.wesl`.
- Atlases per slot: source (distance, sun, sky, region), open mask (R8),
  density and lit (baked on the GPU). The march and the lighting bake bind
  the open mask.
- Crystal puffs are plugin-owned entities (`CrystalPuff`). `sync_crystals`
  runs after transform propagation, so it writes `GlobalTransform` itself.
- `SmokeObstacle` voxelisation runs on a background task; until the first
  level exists, new `Smoke` entities wait before popping.
- GPU timings exist for an Apple M4 (Metal) and, as ratios only, for a
  software rasteriser. Metal's GPU timestamp queries lose the device, so on
  macOS the demo's tier sweep times whole frames instead; NVIDIA, AMD and
  Intel GPUs are still to try (`ROADMAP.md`).
- Prose style in docs and comments: plain words, short sentences, British
  spelling.

## Sandboxed environments

Cloud sandboxes may have an older Rust and no access to rustup's servers.

- If cargo refuses because Bevy needs a newer rustc and `rustup update`
  can't reach the network, pass `--ignore-rust-version`: Bevy 0.20 builds
  with 1.97.0.
- If `rustup target add` is blocked (aarch64 for the determinism check,
  wasm32 for the browser build), put the standard library's sources for
  your exact rustc (the `library/` folder of rust-lang/rust at the commit
  `rustc -vV` prints, from GitHub) into
  `$(rustc --print sysroot)/lib/rustlib/src/rust/library`, and build with
  `RUSTC_BOOTSTRAP=1 cargo … -Zbuild-std=std,panic_abort --target …`.
- Without a GPU, Mesa's software Vulkan driver (llvmpipe) runs the examples
  and their scripted captures, slowly; the windowed ones also need
  `xvfb-run -a`. Use dev builds: release builds of Bevy can take longer than
  a tool call allows.
- The browser build can't be tried in such a sandbox yet: headless
  Chromium 141 offers WebGPU only through SwiftShader, whose device is lost
  at startup ("A valid external Instance reference no longer exists"), even
  for a plain Bevy scene. A newer Chromium may behave.
