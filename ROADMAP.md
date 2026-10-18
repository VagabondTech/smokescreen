# Roadmap

What is done, what comes next, and ideas after that. Measured numbers come
from a 2-core 2.1 GHz cloud VM with a software rasteriser unless said
otherwise; an Apple M4 is 2.5 to 5 times faster on the CPU (the README has
both).

## Done

- **Engine-free core** (`smokescreen`): caller ticks, per-tick snapshots, a
  fill spread over ticks within a work budget (and sped up when a cloud is
  about to start), any units and up axis, bit-identical results on x86-64
  and aarch64, holes as tick-stamped events.
- **Three looks:** volumetric, faceted, and crystal (unblended polyhedra
  drawn as plain meshes; holes shrink whole puffs).
- **Rules:** volumetric density integral; faceted exact or core
  (`FacetRule`); crystal exact. With `sight_threshold = 0` the crystal
  picture and rule agree pixel for pixel (beyond one-pixel silhouette
  edges, on the demo's agreement run). The volumetric rule works out
  density in the picture's order (per voxel, then interpolated), so
  without the noise the two agree too, and `SmokeSettings::noise_within_rule`
  (`Capped` or `Filled`) keeps the noise from showing through what the
  rule blocks (0.2).
- **Faster checks:** skipping the parts of a volumetric cloud the growth
  front hasn't reached or the fade has eaten (3× faster late in the fade),
  per-check hole filtering; both leave results bit-identical.
- **General-purpose clouds:** sources (point, line, box), presets
  (`grenade`, `wall`, `gas`, `fog_bank`, `steam_vent`), `Tuning::max_clouds`
  so every cloud that counts is drawn, pop order independence.
- **Level from meshes:** `SmokeObstacle` voxelises marked meshes;
  `VoxelObstacles::from_triangles`, `to_bytes` and `from_bytes`.
- **Renderer:** smoke composited before transparent objects (MSAA-aware),
  32 slots, a viewmodel example.
- **Smoke against walls:** samples average only the open voxels, and the
  fill pads voxels a wall only clips, so smoke is dense right up to walls
  and floors in the rule and the picture, wherever they sit on the voxel
  grid: box levels, your own `Level`, voxelised levels and meshes.
- **Repository:** README and design notes for a general audience, the
  README's examples compiled as doc tests, CI for the core (three OSes,
  determinism hash, MSRV), the plugin (Linux) and the browser build.
- **The README's media** are recorded by the demo (`SMOKE_DEMO_SCRIPT=video`)
  on a real GPU and encoded with `tools/encode_clips.sh`, so they can be
  remade whenever the look changes.
- **crates.io:** `smokescreen` and `bevy_smokescreen` 0.1.0 (2026-10-17)
  and 0.1.1 (2026-10-18, the performance work of two reviews; tags `v0.1.0`
  and `v0.1.1`), with their API docs on docs.rs.
- **Touch controls** for the browser demo: drag to look, a thumb stick to
  move, tap to shoot. Checked on an iPhone 15 Pro (Safari 26).

## Next

1. **Real GPUs.** Done on an Apple M4 (Metal): the showcase, the README's
   clips, the `tiers` sweep with `tools/fairness.py`, and the `agreement`
   run with `tools/agreement.py`; the README has the GPU costs. Metal's GPU
   timestamp queries lose the device ("Cannot allocate sample buffer"), so
   on macOS the sweep times whole frames that wait for the GPU, and
   `budget` mode has no measurements to steer by. Still to do: NVIDIA, AMD
   and Intel GPUs (with timestamps), `budget` mode on a real GPU, and MSAA
   and TAA on and off with HDR and LDR cameras.
2. **Web demo.** It runs in Chrome 155 (WebGPU) on an Apple M4: every
   style, grenades, shooting and the dial, with no changes for WebGPU's
   limits. `.github/workflows/web.yml` builds it for pull requests and
   publishes `main` to <https://vagabondtech.github.io/smokescreen/>.
   It runs in Safari 26 on an iPhone 15 Pro too, touch controls included,
   now that its meshes have no UVs (Bevy 0.20's PBR shader keeps textures
   in `let`s, which Safari can't compile: report that to Bevy). Still to
   do: Firefox, Edge and Android, Safari on a Mac, other GPUs, and the
   click-to-look pointer lock with someone at the keyboard (tried only on
   a locked screen, where Chrome refuses it).
3. **CI for the Bevy crate on macOS and Windows** (only Linux checks it
   today), and the picture-against-rule check in CI on a software
   rasteriser (Mesa lavapipe runs on GitHub's Linux runners). The demo's
   captures draw offscreen now, so that needs no virtual display.

## Rules and looks

- **A faceted rule between Core and Exact.** The lower bound `blocks_sight`
  uses for faceted clouds (the hard polyhedra clipped to the fill's space,
  minus a capsule around each hole) is itself a rule: exact, as cheap as
  the crystal rule, and conservative like `FacetRule::Core` while counting
  about twice as much of each puff. A new `FacetRule` variant breaks the
  API, so it waits for 0.2.
- **Walls thinner than a voxel.** Smoke stops at the middle of the first
  voxel such a wall reaches into, so up to 12 cm can stay clear in front of
  it and as much can show past it (15 cm with meshes). Padding can't help
  without letting smoke past the wall. A fix: keep a distance to the wall
  per solid voxel next to the smoke, and clip samples to it, in the core
  and the shaders.
- **Meshes facing inwards, and single sheets.** A level mesh whose
  triangles face inwards (a room made of one box seen from inside) has no
  inside, nor has a vertical sheet, so smoke stops up to half a voxel short
  of them; and a single sheet facing up counts as ground, so up to a voxel
  of smoke can show under a raised floor made of one. `from_triangles`
  could look at the nearest surface's facing near such meshes instead.
- **Faceted and crystal smoke against walls.** Their clip (the region
  distance) still crosses zero between the last open voxel and the first
  solid one, so their puffs can stop up to about a voxel short of walls.
  The same masked sampling would apply to the region channel (core,
  crystal sizing and the faceted shaders).

- **Faceted thin edges.** With the 0.8 m rule the faceted picture draws
  thin edges as solid smoke that the rule sees through (bots see a little
  more than players). Options: draw chords under the threshold see-through,
  or recommend `FacetRule::Core` plus a lower threshold. Crystal has the
  same trade-off unless its threshold is 0.
- **Volumetric noise between players.** The noise animates on each
  client's own clock (`Time`), so two players looking at each other through
  the same smoke see different holes at the same moment. Driven by the
  simulation's ticks instead, it would be the same for everyone, and a hole
  that lets one player see another would let the other see them back. By
  default the noise also thins some smoke the rule counts, most while a
  cloud fades (docs/design.md, "The picture against the rule"): players all
  see the same thinning, but a server or bots that use the rule see less
  than they do there. `SmokeSettings::noise_within_rule` is for those games.
- **`NoiseWithinRule::Filled`.** At the two cheapest dial steps a few pixels
  just inside the edge of what the rule blocks still let a little through
  (up to 16 by more than 1 %, none by more than 30 %): their quarter
  resolution blurs the fill's sharper edge. Ending the fill a little before the threshold would close that. Its
  cost (about 10 % close up at the top steps) could come down by adding up
  the baked light channels along the ray instead of working out the light
  at every sample, and the outline of a fading cloud's dome could be
  softer.
- **The march's skip through empty space.** Where the filtered density is
  under 0.002, the volumetric march moves on at least a voxel, but a
  sample can read that little within a voxel of dense smoke (trilinear
  weights are small near a cell's far corner). So it can step over the
  start of a cloud's edge: drawn without its noise, the picture shows up
  to 10 % less than the rule counts through a grown cloud's edges. With
  the noise on, which erodes that thin smoke anyway, marching it changes
  nothing visible, and with `noise_within_rule` capped or filled, what it
  skips counts as cloud the noise took. Going back whenever a jump lands in smoke
  made the two agree to within 0.5 % but cost 12–17 % close up at the top
  two dial steps on an Apple M4; a cheaper way would be welcome.

## Performance

The README's tables come from `cargo bench -p smokescreen`. The weak spots:

- **Thin clouds.** Halfway through the fade, volumetric checks cost 3× what
  they cost when grown (0.18 against 0.06 ms per 250 on an Apple M4): lines
  that never reach the threshold are integrated to the end. An upper bound
  per brick on the density left doesn't help: tried in the first
  performance review, it was correct but 2 to 4 m where the true amount was
  under 0.1 m, and it cost 15 % on every other check. The dissolve eats a
  cloud from the outside in, so unseen lines run through the thinning shell
  a voxel from dense voxels, and no bound coarser than a sample sees that.
  Only coarser samples (whole-voxel steps, a tuning option) or an integral
  per voxel would, and both change results. The faceted core rule behaves
  the same way.
- **The per-tick snapshot** of faceted and crystal clouds (every puff's
  position and size: 0.1 to 0.3 ms for 32 clouds) is computed for every
  cloud, whether or not a check comes near it. Compute a cloud's puffs on
  the first check that touches it.
- Volumetric checks: integrate whole bricks of constant density at once;
  consider whole-voxel steps as a tuning option.
- Faceted fills cost about 3× volumetric ones (0.33 s of ticks at the
  default budget), most of it in the halo and the region's two distance
  transforms. Options, all of which change results: the halo as two
  dilations (it only produces a set), one distance transform of the
  boundary instead of two, an integer 3-4-5 chamfer, a narrow band around
  the fill. A server using `FacetRule::Core` could also skip the wall-clip
  stages if puff seeding stopped depending on the halo.
- Crystal clouds are thousands of entities. If entity overhead shows up in
  profiles, draw puffs with one instanced draw per cloud instead.
- Show a multi-threaded query pattern (a `View` is `Sync`).

## Features

- Physics adapters: `Level` for avian and rapier colliders, as optional
  features or small crates.
- Dynamic obstacles (a door closing on a smoke): re-fill or patch the grid.
- Wind that moves a cloud, not just its texture.
- Continuous emitters (today a long `grow_time` stands in for a vent).
- Lights: spot lights, several directional lights, shadowed point lights.
- Orthographic cameras.
- Mixed styles on screen: faceted is drawn before volumetric, so a
  volumetric smoke behind a faceted one still draws over it.
- Faceted puffs writing depth and casting shadows on the level (crystal
  already does both).
- An audio example using `SmokeVision::transmittance`.
