# CLAUDE.md

Notes for Claude Code sessions (and humans) working on this repository. Read
this first; `ROADMAP.md` has what is left to do.

## What this is

Smokescreen (https://github.com/VagabondTech/smokescreen, by VagabondTech):
smoke for games, as a plugin for Bevy 0.20, in two crates in one workspace:

- **`smokescreen/`**: the simulation, with no engine in it. Flood fill, a
  cloud's shape over time, bullet and explosion holes, and the sight rule
  (`occlusion`, `blocks_sight`). Runs on caller-supplied ticks, in caller
  units, bit-identical on every platform. Depends only on `glam` and `libm`
  (and `serde`, behind a feature of that name, off by default).
- **`bevy_smokescreen`** (the repository root): the Bevy plugin. Draws what
  the core computes in one of three looks, wraps it in ECS conveniences
  (`Smoke` component and presets, `SmokeVision`, `SmokeObstacle`), and has a
  per-camera quality dial.

The three looks (`Style` in the core, `SmokeStyle` in Bevy):

| Style | Drawn by | Rule (`occlusion`) |
|---|---|---|
| Volumetric | raymarch of a baked 3D texture (`src/render.rs`, `src/shaders/smoke_{bake,march,composite}.wesl`) | density integrated along the segment, a sample every voxel (`Tuning::volume_steps`), each voxel's density interpolated as the GPU's sampler does (`DensityTable`) |
| Faceted | sphere tracing of blended polyhedra (`src/shaders/smoke_facet*.wesl`) | chord through the blended SDF (`FacetRule::Exact`; `blocks_sight` decides most lines from exact chord bounds and traces those within 2 cm of the threshold: about half a volumetric check grown, twice one mid-fade), through each puff's polyhedron clipped to the ball of its room (`FacetRule::Chords`, exact chords, about as cheap as `Core`, conservative) or through each puff's inscribed sphere (`FacetRule::Core`, cheap, conservative) |
| Crystal | ordinary meshes, one entity per puff (`src/crystal.rs`) | exact chord through the union of convex polyhedra (`smokescreen/src/chords.rs`); with `sight_threshold = 0` the picture and the rule agree exactly |

## Commands

```sh
cargo test -p smokescreen --release         # core tests, ~10 s
cargo test -p smokescreen --release --features serde  # with the serde feature's tests (tests/serde.rs)
cargo test -p bevy_smokescreen              # builds every example, runs the README's and llms.txt's examples as doc tests
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --features serde -- -D warnings  # both crates' serde feature
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
- `llms.txt` maps both crates' API for coding assistants, with recipes;
  its Rust blocks are doc tests too (`LlmsTxtDoctests` in `src/lib.rs`).
  Change it with the public API. Its blocks stay complete and runnable as
  they are: full `use` lines and no `#`-hidden lines (the raw file shows
  them), and every non-Rust block tagged (`toml`, `text`), as rustdoc
  compiles untagged ones as Rust. Its glTF recipe needs Bevy's scene types,
  which the dev-dependencies enable for native targets only. Its docs.rs
  links name items' pages: check them against `cargo doc`'s output when
  items move. The plugin's package ships it; `CLAUDE.md` stays out.
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
  demo's `tiers` and `agreement` captures into reports. The `agreement`
  run takes the demo's style: volumetric smoke is drawn as its
  transmittance (`SmokeSettings::debug_transmittance`, tonemapping off),
  and `SMOKE_DEMO_SWEEP_AGE` and `SMOKE_DEMO_NOISE=0` set its age and
  turn off its noise.
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
   line and a box source, holes, metres and Z-up inches, the exact and core
   facet rules, both volumetric steps); the metres and inches runs again
   under the chord rule, apart (`EXPECTED_CHORDS`, so adding that rule left
   `EXPECTED` alone); and separately a level voxelised from meshes with
   clouds filled in it.
   `EXPECTED`, `EXPECTED_CHORDS` and `EXPECTED_MESHES` must match on every
   platform. If you change results on purpose, re-record them and check
   them on x86-64 *and* aarch64 (CI runs macOS ARM, Linux and Windows).
   Locally, aarch64 under emulation:
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
5. **Fairness.** First between players: everyone must see the same smoke,
   whatever their quality-dial step, and whatever their machine's clock
   says: what is drawn follows the drawn tick only (the volumetric noise
   moves on it and `SmokeClock::draw_fraction`; the demo's
   `SMOKE_DEMO_CLIENT_CLOCK` must change no pixel). No step may change a
   cloud's shape (only how precisely it is drawn), and where steps can't
   agree exactly the cheaper one errs towards more smoke (volumetric noise
   falls a little differently from step to step:
   `tools/fairness.py --volumetric` fails if more than 1 % of a view's smoke
   pixels let 0.3 more of the scene through than at the best step; the
   demo's views reach 0.65 %, the far smoke mid-fade at the cheapest step).
   Then against the rule: the faceted and crystal pictures never show
   through anything the rule blocks. The volumetric noise does, most while a
   cloud fades, unless `SmokeSettings::noise_within_rule` is `Capped` (the
   noise may take at most `sight_threshold` less ln(100) / extinction from
   any ray, the march's `budget`) or `Filled` (the march fills the opacity
   it took back in at the end, from its sum of the rule's cloud); the
   empty-space jumps charge what they skip to either. Check with the demo's
   `agreement` (`SMOKE_DEMO_TIER` for any step) and `tiers` captures and the
   tools (`tools/overlay.py` paints the disagreements).
6. **Units.** The core works in metres, Y up, internally; everything public
   is in the caller's units (`Units`). Renderers draw in the internal frame
   and convert per view.
7. **Holes are events with ticks.** Renderers read hole shapes from
   `CloudState::holes` (and crystal puff sizes from `CloudState::live`);
   never re-implement the timing curves in shaders.
8. **Samples skip solid voxels.** Volumetric density is the trilinear
   average of the *open* voxels' densities only (`grid::sample_open` over
   the cloud's `DensityTable`: density per voxel first, then interpolated,
   the GPU's order; on the GPU the filtered baked density divided by the
   filtered open mask, `smoke_open_atlas`), so smoke stays dense right up
   to walls, and the rule counts the density the picture draws (check
   with the demo's volumetric `agreement` run, `SMOKE_DEMO_NOISE=0`, and
   `tools/agreement.py`). The fill pads solid voxels a
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
| `smokescreen/src/world.rs` | `Smokes` (pops, holes, ticks, fill budget, snapshots and pinned views, queries), `Tuning`, `Config` and `Config::validate`, `CloudSpec`, `CloudSource`, `View` |
| `smokescreen/src/fill.rs` | The fill state machine: spread, padding clipped wall voxels, bricks, sample cells, halo, region and air distance transforms, puff seeding, binning; sources |
| `smokescreen/src/grid.rs` | The fill grid, bricks and front bounds for skipping, sample cells (walls, and samples that read nothing), chamfer distance transforms |
| `smokescreen/src/volume.rs` | Volumetric rule: density integral, skipping empty bricks, inactive parts and samples that read nothing |
| `smokescreen/src/facets.rs` | Polyhedra, puffs, bins, the blended faceted SDF and its exact integrator |
| `smokescreen/src/chords.rs` | Crystal, faceted-chord and faceted-core rules: exact chords through convex pieces; `ChordBounds`, the exact faceted rule's bounds for `blocks_sight`; `hole_spans`, the holes along a line |
| `smokescreen/src/holes.rs`, `timeline.rs` | Hole events (`Impact`, as given) and shapes; a cloud's life, the density model and `DensityTable` |
| `smokescreen/src/level.rs` | `Level` trait, `FlatGround`, `BoxObstacles`, `VoxelObstacles` (from boxes, a closure or triangles, with winding numbers for which side of a mesh is solid; to and from bytes) |
| `smokescreen/src/light.rs` | Sun and sky visibility per voxel, for renderers |
| `smokescreen/tests/` | `fill`, `queries` (late joiners too), `styles`, `walls`, `determinism`, `serde` (only with the feature) |
| `src/lib.rs` | Plugin, `Smoke` and presets, `SmokePopped` and `SmokeEnded`, `SmokeVision`, `SmokeClock`, systems |
| `src/render.rs` | Atlas, uploads, bake, raymarch, faceted passes, composite |
| `src/crystal.rs` | Crystal meshes and the per-frame puff sync |
| `src/obstacles.rs` | `SmokeObstacle`: voxelises marked meshes into the level |
| `src/quality.rs` | The dial, the budget controller, and the march's resolution by distance (`march_resolution`, kept in `SmokeQuality`) |
| `src/setup.rs` | Warnings for setup mistakes: `SetupWarnings` logs each once per app (the plugin tests read it); the checks sit in the systems that already see each mistake, so none costs a frame anything |
| `src/shaders/` | WGSL (`.wesl`); `smoke_common.wesl` mirrors the `Gpu*` structs and limits in `render.rs` |
| `examples/` | `minimal`, `presets`, `demo` (also scripted captures and the README's clips), `viewmodel`, `units` |
| `tools/` | `agreement.py`, `fairness.py`: reports from the demo's captures |
| `web/` | The page that hosts the browser build of the demo |
| `docs/design.md` | How it works, measurements; keep it in step with the code |
| `docs/media/` | The README's clips: recorded by the demo's `video` script, encoded by `tools/encode_clips.sh`; remake them when the look changes |
| `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, `ROADMAP.md` | For users and contributors; record user-visible changes in the changelog |
| `llms.txt` | The API on one page with recipes, for coding assistants; compiled as doc tests, shipped in the plugin's package, served on the demo's site |

## Gotchas

- The smoke composite runs in Bevy's `Core3d` schedule between
  `main_opaque_pass_3d` (and the transmissive pass) and
  `main_transparent_pass_3d`. With MSAA, the overlay passes draw into
  `ViewTarget::get_color_attachment()` (the multisampled texture) when the
  camera's `Transparent3d` phase has items, so Bevy's transparent pass
  continues from it. Otherwise they draw into the resolved image
  (`get_unsampled_color_attachment()`), which skips loading, storing and
  resolving the multisampled target: about 0.2 ms a frame at 1080p on an
  Apple M4. So each overlay pipeline exists for both sample counts
  (`OverlayPipelines`). Bevy's render internals change every release:
  expect this file to need work on each Bevy upgrade.
- `gather_point_lights` reads `ViewVisibility`, which Bevy clears in
  `VisibilitySystems::VisibilityPropagate` and sets again in
  `CheckVisibility`: keep it ordered after `CheckVisibility`. Read before
  that, every light reads as hidden and no flash lights the smoke (the
  plugin test `point_lights_that_reach_the_view_light_the_smoke` fails).
- Keep `MAX_SLOTS`, `MAX_HOLES`, `MAX_LIGHTS` and the `Gpu*` struct layouts
  in `render.rs` in step with `smoke_common.wesl`. `limits_match_the_shaders`
  checks the constants, and that `GpuGlobals`, one uniform buffer, stays
  within the 64 KiB WebGPU allows. Every drawn smoke's holes share
  `GpuGlobals::holes`: 512, the default 16 for each of 32 slots.
- Atlases per slot: source (distance, sun, sky, region), open mask (R8),
  density and lit (baked on the GPU). The march and the lighting bake bind
  the open mask.
- The volumetric march's samples lie on a sequence fixed by the ray alone
  (`Seq` in `smoke_march.wesl`: `t_k = u0·r^k − 1/g`, the step
  `base_step·(1 + t·growth)` started at the camera), so a ray may start or
  jump anywhere along it without any sample moving as a smoke's boxes
  change tick by tick. Keep it that way: samples anchored to a smoke's box
  (how it was first done) made the noise crawl whenever a ray started at a
  box that changed, and skipping that only dropped fetches while the samples
  stayed put measured as a cost on the M4 (a lane that skips a fetch saves
  nothing while the rest of its SIMD group fetches). A change to the
  sequence changes the picture: compare the
  tier sweep's frames grown and at `SMOKE_DEMO_SWEEP_AGE=0.8` and `16.5`
  before and after, and run `tools/fairness.py` on them.
- The march's resolution close up comes from a depth measured on the GPU
  (`SmokeCamera::measured_depth`; `read_back_depths` in render.rs) and read
  three frames late (`MEASURED_LAG` in quality.rs). The demo's captures
  wait for the GPU every frame (`wait_for_gpu`), so the measurement lands
  on the same frame every run and captures stay reproducible; without the
  wait it lands when the GPU is done. A capture that must not depend on it
  can turn it off per camera.
- Skipping the samples inside a smoke's occupied box that read nothing
  (the 4³ bricks the rule skips) does not pay on the M4: tried twice in
  the march, dropping their fetches and jumping them outright, it cost
  5–15 % close up and at mid-range both times. The lookups cost more than
  what they save in a loop that is ALU-bound and runs in lockstep.
- Skipping the noise where a ray is already nearly opaque doesn't pay
  either: a test around the noise read in every sample cost 4–12 % close
  up even where it skipped nothing (one that changed no pixel cost 10 %),
  and a SIMD group keeps the pace of its rays through thin smoke, which
  never get nearly opaque. Skipping it only where no noise value could
  leave smoke, which is exact, fared no better: the test on the sample's
  density before the noise read cost 5–7 % even where it never fired, and
  skipping 36 % of the noise reads halfway through the fade only won that
  back. Ending rays sooner paid instead: a ray ends once less than 5 % of
  the scene shows through (`OPAQUE_BELOW` in `smoke_march.wesl`), 8–9 %
  cheaper close up by default for at most 6 levels out of 255, and 7 %
  with `Filled`, which then fills from where its march turns opaque.
  `Capped` goes on to 1 %: its budget is worked out from it, and at 5 %
  the larger budget kept rays reading the noise for longer, 15 % slower.
  Taking every second sample once less than a quarter of the scene shows
  through paid much less: 2–4 % close up at the top three steps while a
  cloud is grown or blooming, barely above the noise, and nothing halfway
  through the fade, so it was left out. Its test on the transmittance
  cost nothing.
- The march is just as touchy about state carried along the ray. The
  noise budget's bookkeeping (two floats and a few operations per sample)
  cost 9–14 % close up at the top dial steps even when it limited nothing,
  so `SmokeSettings::noise_within_rule`'s modes are compiled in only when
  chosen (`NOISE_CAPPED`, `NOISE_FILLED`): six march pipelines, three
  modes with and without MSAA, plus the single-smoke kernels below. Going
  back over what an empty-space jump skipped, to draw a cloud's edge
  exactly, cost 12–17 % close up for no visible change with the noise on.
  Fewer fetches don't pay on their own either. Reading density and the
  open mask in one fetch, and the lighting only where the noise leaves
  smoke, took 14–23 % of the fetches out close up and saved nothing.
  Reading the lighting once per voxel's worth of samples took 22–26 % out
  and cost 2–6 % on a grown cloud, as it carried the light along the ray.
- The march is short of registers rather than arithmetic on the M4. One
  dispatch per smoke with a kernel compiled for one smoke (`SINGLE_SMOKE`:
  no per-ray interval arrays, the slot in `view.params.z`, so its data is
  the same for the whole dispatch) made the best step 27–32 % cheaper
  close up; compiling out the unused point-light code (`NO_POINT_LIGHTS`)
  was worth 8 % once the arrays were gone and nothing before. Taking a
  third of the sample loop's instructions out (constants folded per smoke,
  coordinates advanced along the ray) made it 6–7 % slower, with the slot
  uniform or not. `render.rs` picks per camera and frame: smokes whose
  screen rectangles come close are marched together, one dispatch per group
  (`march_groups`), by the general kernel; a smoke on its own by the
  single-smoke one. The general kernel's per-ray arrays have room for 8
  smokes (`RAY_SMOKES_8`) where no point on screen lies in more than 8 of
  the group's rectangles (`most_overlapping`): its overlapping path reads
  them at every sample, and the short arrays made two overlapping smokes
  32–39 % cheaper close up, 39–47 % with the light code out too (it was
  worth 4–11 % there). Groups that need more fall back on the 32-slot
  kernel. The kernels share the one-smoke path's text through
  `@if(SINGLE_SMOKE)`: keep it shared, so they draw the same pixels.
- Crystal puffs are plugin-owned entities (`CrystalPuff`). `sync_crystals`
  runs after transform propagation, so it writes `GlobalTransform` itself.
  It runs before Bevy's `VisibilityPropagate`, `CalculateBounds` and
  `AssetEventSystems`, so each puff is culled where it is now and drawn in
  the frame the rule counts it, with a new cloud's material; new puffs are
  spawned in place for the same reason. Unordered, Bevy's systems ran first
  and puffs switched on and off a frame late. The plugin test
  `every_crystal_puff_the_rule_counts_is_drawn` catches that.
- `SmokeObstacle` voxelisation runs on a background task; until the first
  level exists, new `Smoke` entities wait before popping.
- `SmokeVision` only reads. It answers from two views that
  `Smokes::pin_view` keeps out of the snapshot cache's turnover (the
  current tick and the drawn tick), prepared by `prepare_views` at
  `PreStartup`, after `advance_clock` in `PreUpdate`, and at the end of
  `SmokeSystems::Simulate`. A pinned view stays as it was when pinned,
  whatever the world does next: each `CloudState` points at its cloud's
  `Filled` (the fill, spec and timeline, made once when the fill is done)
  instead of indexing the cloud list. Games
  that move the clock or change the world mid-frame call
  `SmokeWorld::prepare_vision`; `SmokeVision` warns once when the clock
  has moved past its view. `build_frame` and `sync_crystals` still ask
  `world.view(drawn)`, which reuses the pinned snapshot when nothing
  changed, so the picture has every change.
- Without Bevy's `RenderPlugin` (a dedicated server) the plugin adds none
  of its drawing systems (they are added in `finish`). It registers its
  shaders and extraction only where a render world exists: without the
  renderer, registering shaders panics and extraction logs errors.
  `SmokeUploads` drops what it is given unless a render world drains it:
  kept, the queue held every cloud's grid. The plugin tests' headless app
  keeps `RenderPlugin` with no GPU backend, so the drawing systems run
  there; `server_app` builds a server's app.
- `SmokePopped` and `SmokeEnded` are triggered by `announce_smokes` in
  `PostUpdate` before `TransformSystems::Propagate`, so what observers
  spawn is placed that frame. Spawned after propagation (inside
  `SmokeSystems::Simulate`, say), an entity keeps `GlobalTransform`'s
  default, the origin, until the next frame's propagation, and is drawn
  there for a frame. A pop is so announced the frame after `pop_smokes`
  popped it; `retire_smokes` triggers whatever is still due before it
  despawns a smoke (one heard after it ended). `SmokeEntities` keeps which
  events each entity has had.
- Reflection. Bevy's remote reflection can't mirror the core's
  `#[non_exhaustive]` enums: `#[reflect_remote(smokescreen::Style)]`
  generates a check that matches every variant, which fails with E0004
  from another crate. An opaque wrapper compiles but can only be shown,
  not edited. So `Smoke::style` and `CrystalPuff::cloud` are
  `#[reflect(ignore)]`, and the core has no Bevy (an optional
  `bevy_reflect` feature there was the alternative, left out to keep the
  core engine-free). A field added to a reflected struct needs a default
  for reflection (its type registers `Default`, or the field has
  `#[reflect(default)]`), or scene files saved by an earlier 0.2 release
  stop loading; `VolumetricQuality` and `FacetQuality` have no `Default`.
- `Smokes::impacts` gives back each impact as it was given: every hole
  entry keeps its `Impact` beside its metres, as converting the metres
  back doesn't always give the same bits (Z-up inches, say). The metres
  are computed as before, so the hashes held. serde_json 1.0.154 gives
  back every finite `f32` in the same bits (all 4,278,190,080 checked,
  with and without its `float_roundtrip` feature), which the docs say.
- GPU timings exist for an Apple M4 (Metal) and, as ratios only, for a
  software rasteriser. Metal's GPU timestamp queries lose the device, so on
  macOS the demo's tier sweep times whole frames instead; NVIDIA, AMD and
  Intel GPUs are still to try (`ROADMAP.md`).
- What is public. Each crate's API is what its docs show: the root, and
  the plugin's `prelude` and `quality`. `#![warn(missing_docs)]` keeps it
  documented. What the plugin or the tests need but games shouldn't call
  is `#[doc(hidden)]` on its definition (`missing_docs` reads the
  definition, not the re-export), and may change in any release, which is
  why the plugin depends on the core's exact version. Anything nothing
  outside its crate uses is `pub(crate)`. The core's modules are private
  except `grid`, `light` and `math`, which are hidden: a type defined in a
  hidden module escapes `missing_docs` even when the root re-exports it,
  so documented types live in private modules.
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
