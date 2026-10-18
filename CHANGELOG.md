# Changelog

Changes that alter simulation results (and so break lockstep with older
versions) are marked **results**.

## Unreleased (0.2.0)

Simulation results change in this release: a 0.2 server and a 0.1 client
are not in lockstep.

- **results** The volumetric rule works out density in the same order as
  the picture. A sample used to take the density of the distance
  interpolated between voxels, while the GPU bakes each voxel's density and
  its sampler interpolates those. The two differ wherever the density
  curve bends, most at a cloud's edge: drawn without its noise, the
  picture showed 1.2 to 6 times the amount of cloud the rule counted along
  lines through less than 0.1 m of it, and 3–84 % more along thicker ones.
  Now each cloud's snapshot holds a `DensityTable` (`CloudState::density`:
  the density of every fill-distance byte at that tick), and samples
  interpolate its values over the open voxels. The two now agree to within
  1 % halfway through the fade from 0.05 m of cloud up, and to within 10 %
  on a grown cloud, where the rest is the march stepping over the start of
  a cloud's edge (see the roadmap). The rule counts more smoke near
  clouds' edges: 1–3 % more along a typical line through a grown cloud,
  6–16 % halfway through the fade, and 1–4 % more of the lines in the
  demo's views are blocked. Checks are faster, as samples no longer work
  out three smoothsteps each: on an Apple M4, 250 volumetric checks
  through 4 of 32 clouds take 0.183 instead of 0.250 ms halfway through
  the fade and 0.061 instead of 0.078 ms once the clouds are grown
  (medians of 11 alternated runs). The snapshot builds a table for each
  volumetric cloud that is growing or fading, about 0.15 µs each; a grown
  cloud keeps its table. The determinism hash (`EXPECTED`) changed, while
  the one for fills in a level voxelised from meshes did not: update
  servers and clients together.
- `VolumeField` holds the cloud's `DensityTable` (`density`) instead of
  `growth` and `dissolve`.
- `SmokeSettings::debug_transmittance` draws volumetric smoke as how much
  of the scene shows through it. The demo's `agreement` script now runs
  volumetric smoke too (it used to switch to faceted), with its age set
  by `SMOKE_DEMO_SWEEP_AGE` as in the tier sweep, and the new
  `SMOKE_DEMO_NOISE=0` draws volumetric smoke without its noise.
  `tools/agreement.py` compares the picture's transmittance with the rule.
  It shows the noise thinning the picture below what the rule blocks: in
  the demo's close view, 5.5 % of the pixels the rule blocks let more than
  30 % of the scene through in a grown cloud (2.9 % with the old rule,
  which counted less of the edges the noise erodes), and a third of them
  halfway through the fade, before and after. Without the noise, none do.
  `SmokeSettings::noise_within_rule` (below) keeps the noise from doing
  so.
- `SmokeSettings::noise_within_rule` (`NoiseWithinRule`) can keep
  volumetric smoke's noise from showing through what the rule blocks, for
  games whose server, bots or abilities use the rule. It is `Off` by
  default, and costs nothing then: each of the other two modes is compiled
  into its own march pipelines, used only when chosen.
  - `Capped`: along any line of sight the noise takes no more cloud than
    the rule leaves spare (`Tuning::sight_threshold` less the ln(100) /
    `SmokeSettings::extinction` the picture needs to be opaque: 0.14 m by
    default). Thicker smoke looks smoother, and a fading cloud stays a pale,
    smooth dome where the rule still blocks. It costs less, as a ray stops
    sampling the noise once it has taken all it may: on an Apple M4 at
    1920×1080, close up (medians of five alternated runs), the best step
    takes 4.44 ms where `Off` takes 5.12 once the cloud is grown (−13 %)
    and 3.13 where it takes 3.40 halfway through the fade (−8 %); the two
    cheapest steps cost about the same.
  - `Filled`: the noise runs as before, and on lines through more than
    ln(20) / extinction of cloud (0.43 m by default, where rays end as
    they do with `Off`) the opacity it took is filled back in behind what
    was drawn, completely by the rule's threshold. The billows stay; a
    fading cloud stays a textured dome where the rule still blocks. It
    costs more: close up at the best step, 5.72 ms where `Off` takes 5.12
    once the cloud is grown (+12 %), and 3.74 where it takes 3.40 halfway
    through the fade (+10 %).

  With either, none of the pixels the rule blocks in the demo's four
  agreement views let more than 1 % of the scene through (away from
  edges), at the best step, grown, blooming and halfway through the fade;
  before, up to 5.5 % let more than 30 % through in a grown cloud, and a
  third of them halfway through the fade. Both do the same at every step
  of the dial. Thin smoke keeps all its noise in both. The demo picks the mode with
  `SMOKE_DEMO_NOISE_WITHIN_RULE=capped` or `filled`, its `agreement`
  script draws at any step of the dial with `SMOKE_DEMO_TIER`, and
  `tools/overlay.py` paints where the picture and the rule disagree.
- Volumetric rays end once less than 5 % of the scene shows through them,
  instead of 1 %, with the noise as it is by default
  (`NoiseWithinRule::Off`), and are drawn opaque as before. On an Apple M4
  at 1920×1080, close up (medians of five alternated runs), the best step
  takes 5.13 instead of 5.60 ms once the cloud is grown (−8 %), 5.17
  instead of 5.68 ms 0.8 s into the bloom (−9 %) and 3.42 instead of 3.47
  ms halfway through the fade (−2 %); at mid-range, 1.17 instead of 1.25
  ms grown. The picture changes by at most 6 levels out of 255 in the tier
  sweep's frames (most opaque pixels by 1 or 2), and only towards more
  smoke: a line through more than 0.43 m of the cloud as drawn is opaque,
  where until 0.66 m up to 5 % of the scene showed through. In the demo's
  agreement views, the pixels the rule blocks that let more than 1 % of
  the scene through fall by a fifth to a third; those letting more than
  10 % or 30 % through are unchanged, and so is the agreement between dial
  steps. Lines the picture now draws opaque while the rule sees through
  them (between 0.43 m and the rule's 0.8 m of cloud) are at most 0.3 %
  of the smoke's pixels in a view (close up, grown), and none halfway
  through the fade. `Filled` ends its rays at 5 % too and fills from
  there (above): that made it 7 % cheaper close up and closed a leak at
  the two cheapest dial steps, where its fill used to start at 0.66 m and
  a few pixels just inside the edge of the blocked area let a little
  through (up to 16 by more than 1 %). `Capped` goes on to 1 %, which its
  budget is worked out from: at 5 % the noise could take 0.37 m instead
  of 0.14 m, was read for longer, and cost 15 % more close up.
  Skipping the noise once a ray is nearly opaque, tried first, cost 6–8 %
  more: rays run in lockstep, and those through thin smoke, which never
  get nearly opaque, set the pace.
- Volumetric smoke's noise moves on the simulation's clock instead of each
  machine's own. It scrolled with Bevy's `Time`, so two players drawing
  the same tick saw different holes in the same smoke whenever their apps
  had been running for different lengths of time. In the demo, with one
  app's clock about 20 minutes ahead of the other's, close up at the best
  step, 11 % of a grown cloud's pixels let more than a tenth more or less
  of the scene through on one than on the other (4 % more than 0.3), and
  34 % halfway through the fade (18 %); far more than a cheaper dial step
  differs from the best one. Now the noise's time is the drawn tick plus
  the new `SmokeClock::draw_fraction`, how far the frame is past that
  tick: players who draw the same tick and fraction with the same
  `SmokeSettings` see the same noise, and a gap that lets one see another
  lets the other see them back, as closely as each one's march samples
  it. With `auto` on, the plugin fills the fraction in from virtual time,
  and the picture is as before: the same pixels in the demo's captures, at
  every step and age. If you drive the clock yourself, set `draw_fraction`
  to your interpolation fraction; left at 0, the noise moves once a tick.
  It still starts again every hour, now after an hour's worth of ticks, on
  the same tick for everyone. Code that builds `SmokeClock` with a struct
  literal needs the new field (or `..default()`), and `SmokeClock` is no
  longer `Eq`. The demo's `SMOKE_DEMO_CLIENT_CLOCK=<seconds>` starts its
  app's clock that far ahead of the ticks, which must change no pixel.
- The demo starts with the quality dial at low (it was medium), in every
  look; its scripted captures still start at high.

## 0.1.1 (2026-10-18)

The performance work of the first two reviews. No public API changes, and
simulation results are unchanged: the determinism hashes are those of
0.1.0, so a 0.1.0 server and a 0.1.1 client stay in lockstep.

- Fills are 16–20 % faster (on an Apple M4) and finish in fewer ticks at the
  default budget: 14 instead of 15 for a volumetric cloud, 37–42 instead of
  40–45 for a faceted one. Neighbours are found by index offsets, the lump
  noise keeps the corners of its last few cells, bricks are built from the
  filled voxels alone, and the region's distance transforms cover a smaller
  box. From the first performance review. Results are unchanged: the
  determinism hashes are the same.
- `blocks_sight` through faceted clouds under `FacetRule::Exact` is about
  17× faster while clouds grow and once they are grown (0.71 → 0.04 ms for
  250 checks through 4 of 32 clouds on an Apple M4), and 2× faster halfway
  through the fade. It decides most lines from exact chord bounds (the
  puffs' hard polyhedra lie inside the cloud, and the polyhedra grown by the
  blend radius contain it) and sphere-traces only lines within 2 cm of the
  threshold; `occlusion` still traces. From the first performance review.
  Results are unchanged: the determinism hashes are the same, and a new
  test checks `blocks_sight` against `occlusion` on random lines through
  clouds of every style with holes.
- Each smoke is dispatched only over the screen area of what it occupies at
  the tick being drawn (`CloudState::active`), not of its whole box, which
  is much bigger while a cloud blooms and fades. On an Apple M4 at
  1920×1080, volumetric smoke halfway through its fade costs about a fifth
  less at mid-range (0.93 instead of 1.18 ms at the best dial step). Rays
  still start where they did, so the picture is the same, pixel for pixel.
  The demo's tier sweep can freeze the smoke at a chosen age
  (`SMOKE_DEMO_SWEEP_AGE`) to measure that.
- A ray through one volumetric smoke returns at once if it misses the box
  the smoke occupies at the drawn tick, ends at that box's far side, and
  steps over the run-up to it without fetching; and the lighting bake clears
  the voxels around that box instead of lighting them. The samples stay
  exactly where they were, so the picture is the same, pixel for pixel
  (checked at every dial step, three distances and three ages). On an
  Apple M4 at 1920×1080, volumetric smoke halfway through its fade costs
  12 % less close up at the best dial step (4.16 → 3.65 ms) and 10–15 %
  less at the middle steps, and about a tenth less at mid-range and far
  away; grown and blooming smoke costs the same. (Skipping the texture
  fetches of empty steps inside the march, per brick as the rule does,
  was tried and measured as a cost on the M4: a lane that skips a fetch
  saves nothing while the rest of its SIMD group fetches.)
- Queries look only at the clouds a segment could touch: the per-tick
  snapshot keeps a coarse grid over the clouds' boxes with a bit per cloud
  in each cell, and a query reads the cells its segment's box covers, in
  index order, instead of testing every cloud's box. Volumetric clouds also
  carry a per-voxel flag saying whether a solid voxel is among the eight a
  sample reads, so the common case, away from walls, tests one byte instead
  of eight. Results are unchanged: the determinism hashes are the same and
  the thorough differential test passes. On an Apple M4, 250 volumetric
  checks through 4 of 32 clouds take 0.07 ms instead of 0.10 while the
  clouds grow and once they are grown, and 0.22 ms instead of 0.30 halfway
  through the fade (medians of 11 alternated runs); checks that cross
  clouds' boxes but no cloud take half the time; crystal checks a little
  less than before. The per-tick snapshot costs about 0.3 µs more for 32
  clouds.
- The volumetric march's samples now lie on a sequence fixed by each ray
  alone (the same spacing and jitter as before, started at the camera
  instead of at a smoke's box), so a ray starts where it enters the box the
  smoke occupies at the drawn tick, with no sample moving as that box
  changes. The dither pattern changes once (at the two best dial steps by
  a mean of 0.3–0.5 of 255 over the smoke, at the cheapest step far away by
  about 5); the smoke's shape, the step sizes and the tiers' reach do not.
  On an Apple M4 at 1920×1080, volumetric smoke halfway through its fade
  costs 8 % less close up at the best dial step (3.65 → 3.37 ms) and 7–10 %
  less at mid-range; while it blooms, 5 % less close up (5.97 → 5.67 ms)
  and 4–6 % less at mid-range; grown smoke costs the same. (Jumping the
  bricks of voxels the rule skips was tried on top of this and cost 5–15 %
  close up and at mid-range: the lookups outweigh the iterations they save
  in a loop that runs in lockstep, so the march only skips a voxel at a
  time through empty space, as before.)
- The demo starts with the quality dial at medium (it was high), in every
  look; its scripted captures still start at high.
- The browser demo has touch controls: drag to look, a thumb stick in the
  lower left to move, tap to shoot where you tap, and tap the buttons. On a
  small screen the buttons and the HUD say less, and once a finger touches
  the screen the help shows touch hints. The page no longer selects text,
  shows a magnifier or bounces under a finger. Checked on an iPhone 15 Pro
  (Safari 26).

## 0.1.0 (2026-10-17)

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
