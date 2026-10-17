# Roadmap

What is done, what a first public release still needs, and ideas after
that. Measured numbers come from a 2-core 2.1 GHz cloud VM with a software
rasteriser unless said otherwise; an Apple M4 is 2.5 to 5 times faster on
the CPU (the README has both).

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
  edges, on the demo's agreement run).
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

## Before a first public release

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
   It draws in Safari 26 on an iPhone 15 Pro now that its meshes have no
   UVs (Bevy 0.20's PBR shader keeps textures in `let`s, which Safari
   can't compile: report that to Bevy), and it has touch controls, tried
   so far only with simulated touches in Chrome. Still to do: the touch
   controls on a real phone, Firefox, Edge and Android, other GPUs, and the
   click-to-look pointer lock with someone at the keyboard (tried only on
   a locked screen, where Chrome refuses it).
3. **crates.io.** Make README image and file links absolute (crates.io
   doesn't resolve relative ones), publish `smokescreen` first, then
   `bevy_smokescreen`, and switch the README's install lines from git to
   version numbers. Add a table of supported Bevy versions to the README.
4. **CI for the Bevy crate on macOS and Windows** (only Linux checks it
   today), and the picture-against-rule check in CI on a software
   rasteriser (Mesa lavapipe runs on GitHub's Linux runners). The demo's
   captures draw offscreen now, so that needs no virtual display.

## Rules and looks

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
- **Volumetric noise.** The picture's noise erosion must never thin smoke
  the rule counts as blocking. Extend `tools/agreement.py` to soft smoke
  (compare the picture's transmittance with the rule) and keep the noise
  from eroding below the rule's density where it matters.

## Performance

The README's tables come from `cargo bench -p smokescreen`. The weak spots:

- **Thin clouds.** Halfway through the fade, volumetric checks cost 1.1 ms
  per 250 (0.3 ms when grown): lines that never reach the threshold are
  integrated to the end. `blocks_sight` could stop as soon as the density
  left along the segment (an upper bound per brick) can't reach the
  threshold. The same holds for the faceted core rule (0.7 ms).
- **The per-tick snapshot** of faceted and crystal clouds (every puff's
  position and size: 0.1 to 0.3 ms for 32 clouds) is computed for every
  cloud, whether or not a check comes near it. Compute a cloud's puffs on
  the first check that touches it.
- Volumetric checks: integrate whole bricks of constant density at once;
  consider whole-voxel steps as a tuning option.
- Faceted fills cost about 3.5× volumetric ones (0.35 s of ticks at the
  default budget). A server using `FacetRule::Core` could skip the wall-clip
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
