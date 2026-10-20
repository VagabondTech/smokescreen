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
- **Rules:** volumetric density integral; faceted exact, chords or core
  (`FacetRule`; chords in 0.2); crystal exact. With `sight_threshold = 0` the crystal
  picture and rule agree pixel for pixel (beyond one-pixel silhouette
  edges, on the demo's agreement run). The volumetric rule works out
  density in the picture's order (per voxel, then interpolated), so
  without the noise the two agree too, and `SmokeSettings::noise_within_rule`
  (`Capped` or `Filled`) keeps the noise from showing through what the
  rule blocks (0.2).
- **The same noise for every player:** volumetric smoke's noise moves on
  the simulation's ticks (`SmokeClock::draw_fraction` between them), not on
  each machine's clock, so players drawing the same tick see the same
  billows (0.2).
- **Faster checks:** skipping the parts of a volumetric cloud the growth
  front hasn't reached or the fade has eaten (3× faster late in the fade),
  per-check hole filtering; both leave results bit-identical. Volumetric
  samples a voxel apart instead of half a voxel (`Tuning::volume_steps`), a
  third cheaper, and crystal checks work out each puff's chord once per
  line instead of in every bin cell that lists it, half the cost (0.2).
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
  or recommend `FacetRule::Chords` plus a lower threshold (it leaves out
  the blend between puffs, most of what it doesn't count). Crystal has the
  same trade-off unless its threshold is 0.
- **The exact faceted rule's precision.** Its trace places each surface
  crossing to about 5 mm (`MIN_STEP`). Against a reference trace in f64
  with steps down to 1e-7 m, a line's amount is off by 1.3–3.2 mm on
  average and by up to 2.2 cm, so `blocks_sight`'s 2 cm margin holds by
  experience (the tests' 730,000 random lines), not by proof. Measured on
  an Apple M4 for 0.2 and left out: placing each crossing to 1e-5 m (a
  secant guess, then a step just past it) cuts the average error a
  hundredfold but still misses slivers thinner than a step (up to 4.5 mm
  on a line), and costs 7–8 % on checks through fading clouds and 13–23 %
  on `occlusion`. Also searching every stretch a step jumps over brings
  the worst line to 0.3 mm, at 21–35 % and 36–46 %. A margin small enough
  to prove would also need steps that hold two crossings searched, and the
  wall clip's slope allowed for: trilinear between voxels, it changes up to
  about 1.8 times as fast as distance near corners. The margin itself
  matters little at 0.8 m: through faceted clouds, `blocks_sight` traces
  13.7 % of its checks with it and would trace 13.4 % without. When any
  contact blocks, a margin of 0 would halve them (23 % to 11 %).
- **`NoiseWithinRule::Filled`.** Its cost (about 12 % more than the default
  close up at the best step) could come down by adding up the baked light
  channels along the ray instead of working out the light at every sample,
  and the outline of a fading cloud's dome could be softer. Its leak at
  the two cheapest dial steps (a few pixels just inside the edge of what
  the rule blocks, up to 16 by more than 1 %) closed when it took the
  default's 5 % ray end and began its fill at 0.43 m of cloud instead of
  0.66 m.
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

- **Volumetric frame time.** Close up at the best step, the smoke now
  costs 1.45 ms on an Apple M4 (5.1 before 0.2): each smoke gets a
  dispatch of its own, the composite got cheaper, and the march goes
  coarser close up. About 0.4–0.5 ms of that is the composite's draw. Its
  pass over the multisampled target (about 0.2 ms at any distance) is
  gone while nothing transparent is on screen. With something
  transparent, the composite could join Bevy's transparent pass instead
  of a pass of its own, if its upsample moved to a compute pass. The
  coarser march takes the far side of the farthest smoke's box, which is
  cautious: inside a dense cloud rays end long before it, so close up a
  quarter would keep the noise too, but the march would have to measure
  how far its rays get. Next: point lights baked per voxel (each light in
  range costs every sample with smoke: close up at the best step, one
  muzzle flash in a grown cloud costs 14 % more, eight 2.1 times as
  much). Tried on the M4
  and dropped: a third fewer instructions in the sample loop (6–7 %
  slower), the lighting read once per four samples (no faster on a grown
  cloud, and up to 87 levels of change at the cheapest steps), and the
  composite reading the march's depths at its resolution instead of the
  multisampled depth (no faster).

- **Thin clouds.** Halfway through the fade, volumetric checks cost 2.7×
  what they cost when grown (0.098 against 0.036 ms per 250 on an Apple
  M4): lines that never reach the threshold are integrated to the end. An
  upper bound per brick on the density left doesn't help: tried in the
  first performance review, it was correct but 2 to 4 m where the true
  amount was under 0.1 m, and it cost 15 % on every other check. The
  dissolve eats a cloud from the outside in, so unseen lines run through
  the thinning shell a voxel from dense voxels, and no bound coarser than a
  sample sees that. Samples a voxel apart (`Tuning::volume_steps`, the
  default since 0.2) made every check about a third cheaper, and skipping
  the samples whose eight voxels all have density 0 (about half of them,
  from a byte per cell) another 10–16 %; an integral per voxel would go
  further, and change results again. Integrating whole bricks of solid
  core at once would not pay: none of the bench's checks samples a brick
  that is solid core with the voxel around it, and at most 0.08 % of their
  samples lie in a cell that is. Grown lines reach the threshold in the
  shell, and halfway through the fade only the inner 3/8 of the fill
  distance is still solid core, which the lines the rule sees through
  miss. The faceted core rule behaves the same way.
- **`occlusion` under the exact faceted rule** traces every line to the
  end: 8.2 ms for the bench's 250 lines through four grown clouds on an
  Apple M4, about 33 µs a line, where `blocks_sight` settles most of them
  from chord bounds in 0.04 ms. `SmokeVision::transmittance` calls
  `occlusion`, so a game that asks it often through faceted smoke pays
  that; the chord rule costs 0.9 ms there.
- **The per-tick snapshot** of faceted and crystal clouds (every puff's
  position and size: 0.1 to 0.3 ms for 32 clouds) is computed for every
  cloud, whether or not a check comes near it. Compute a cloud's puffs on
  the first check that touches it.
- Faceted fills cost about 2.1× volumetric ones (0.26 s of ticks at the
  default budget). Since 0.2 the halo grows from the fill's open faces and
  the distance out of the region runs over the halo's box and one voxel
  around it, so the spread, which volumetric fills share, is the biggest
  part, and the distance into the region (over five voxels around the
  halo's box) comes next: about 0.15 ms of a 1.14 ms faceted fill on an
  Apple M4. Both distances in one pass over one array (a voxel steps from
  0 where its neighbour is on the other side) gives the very same values,
  but took 0.45 ms where the two took 0.30: the test per voxel keeps the
  loop from vectorising. One distance transform of the region's boundary
  instead of two would save about 0.07 ms more, but it changes results
  (each puff's room and the clip surface move by up to half a voxel near
  walls), so 0.2 leaves it out; an integer 3-4-5 chamfer and a narrow
  band around the fill would change results too. A server using
  `FacetRule::Core` could also skip the wall-clip stages if puff seeding
  stopped depending on the halo.
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
