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
- **results** The volumetric rule samples a line every voxel instead of
  every half voxel: `Tuning::volume_steps` (`VolumeSteps::Voxel` by
  default; `HalfVoxel` for the old spacing), which sets the new
  `VolumeField::step`. Checks cost about a third less: on an Apple M4,
  250 sight checks through 4 of 32 clouds take 0.040 instead of 0.059 ms
  once the clouds are grown (−32 %), 0.116 instead of 0.178 ms halfway
  through the fade (−35 %) and 0.030 instead of 0.040 ms late in it
  (−25 %) (medians of 11 alternated runs), and `occlusion` over the same
  lines costs 37–38 % less. The amount of cloud along a line is a little
  less precise. Against samples 1/64 of a voxel apart, over 8,600 random
  lines through smoke (on open ground, and in a level with walls, bullets
  and explosions), the median line is within 0.1 % instead of 0.03 %, the
  99th percentile of lines through at least 0.1 m of cloud within 13–17 %
  instead of 8–9 %, and the rule counts 0.2–0.8 % less cloud on average.
  0.31 % of the lines get a different answer from the fine count's,
  instead of 0.19 %, every one within 10 cm of the threshold; 21 of them
  are clear where the fine count blocks, instead of 10. The largest errors
  are where a line crosses a wall or the ground, up to half a step of
  full-density cloud per crossing. Against the picture nothing that
  matters changes: without the noise the two agree as closely, and with
  `noise_within_rule` capped or filled nothing the rule blocks shows
  through, at the best and the cheapest dial steps. `EXPECTED` changed
  again and now covers both settings; `EXPECTED_MESHES` did not.
  `CloudView` gained a private field, so only a `View` makes one. The
  demo's `SMOKE_DEMO_VOLUME_STEPS=half` samples every half voxel.
- **results** `FacetRule::Core` counts every hole. With
  `Tuning::max_holes_per_cloud` above 32 it read only a cloud's first 32
  holes, which are its oldest, so the newest tunnels and blasts cleared
  nothing under the core rule: it could block a line that the picture and
  the exact rule showed open. In a test, a line along a cloud's 33rd hole
  passed through 4.86 m of cloud instead of none. Now every hole counts.
  Where more than 32 separate stretches of hole cross one line, the two
  nearest are joined and the smoke between them counts as cleared too, so
  the rule still never counts more than the picture draws. With 32 holes
  or fewer per cloud (the default is 16) results are the same to the bit:
  the determinism hashes are unchanged. The exact rule's chord bounds
  count every hole too, so `blocks_sight` no longer traces every line
  through a faceted cloud with more than 32 holes. Checks cost the same:
  on an Apple M4 every figure of the bench is within its run-to-run noise
  of before.
- **results** The crystal rule works out each puff's chord once per line.
  It worked out the chord again in every bin cell that listed the puff,
  three times over on average, and added up the cells' unions; now, as
  with `FacetRule::Chords`, each chord is found the first time a cell
  lists its puff, and what it adds to the length already counted counts
  at once. On an Apple M4, 250 sight checks through 4 of 32 crystal clouds
  take 0.030 instead of 0.060 ms once the clouds are grown (−50 %) and
  0.046 instead of 0.066 ms halfway through the fade (−30 %) at the 0.8 m
  threshold, and 0.023 instead of 0.029 ms (−21 %) and 0.036 instead of
  0.041 ms (−12 %) when any contact blocks; `occlusion`, which counts every
  cloud along the line to its end, takes 0.60 instead of 1.58 ms for the
  same lines once grown (−62 %; medians of 11 alternated runs). The
  amounts are the same but for rounding: they differ in the last bits (by
  at most 6.3e-7 of the amount) on up to 9 % of the lines in the tests'
  workloads, and none of 140,000 `blocks_sight` answers changed. A line
  that meets more than 64 separate stretches of one cloud leaves the rest
  out, so the rule counts less there, never more (the tests' lines meet at
  most 9). `EXPECTED` and `EXPECTED_CHORDS` changed, as both hash crystal
  clouds; `EXPECTED_MESHES` did not.
- `FacetRule::Chords`, a faceted rule between `Core` and `Exact`: the
  length of a line inside each puff's hard polyhedron, clipped to the ball
  of its `Puff::room` (inside the fill's space, so clear of walls), minus a
  capsule around each hole. It is the lower bound the exact rule's
  `blocks_sight` already used, as a rule of its own (`ChordField`,
  `CloudView::chord_field`). Like `Core` it never blocks a line the picture
  leaves clear, and its chords are exact, with no tracing; it also counts
  each puff's corners and edges, which `Core`'s spheres leave out. Each
  puff's chord is worked out once per line, and what it adds to the length
  already counted counts at once, so a line that is blocked is found early.
  On an Apple M4, 250 sight checks through 4 of 32 clouds take 0.042 ms
  grown, 0.131 ms halfway through the fade and 0.075 ms late in it,
  against `Core`'s 0.044, 0.130 and 0.060 and the exact rule's 0.046, 0.73
  and 0.36 (medians of 11 alternated runs). It counts more than `Core`,
  though not much more: along the demo's views it counts 66–70 % of the
  exact rule's amount in a grown cloud and 48–64 % halfway through the
  fade, against `Core`'s 59–64 % and 47–58 %; at the 0.8 m threshold it
  blocks 78 % of the close view's pixels that the exact rule blocks in a
  grown cloud, against `Core`'s 73 %. Most of the rest is the blend between
  puffs, which only the exact rule counts. `FacetRule` is now
  `#[non_exhaustive]`: a `match` on it needs a `_` arm. The new variant
  breaks exhaustive matches anyway, and later ones won't. The determinism
  hashes are unchanged; the chord rule's results have a hash of their own
  (`EXPECTED_CHORDS`). The demo's `SMOKE_DEMO_FACET_RULE=core` or `chords`
  sets the rule its `agreement` script compares with the picture, and
  `tools/agreement.py` names it.
- Volumetric checks skip the samples that read nothing. In place of the
  flags that marked the cells of eight voxels next to a wall, each cloud
  keeps a byte per cell: 0 next to a wall, else the smallest fill-distance
  byte of the eight. A sample whose eight voxels the growth front hasn't
  reached yet or the fade has already eaten is then 0 from that one byte,
  without reading them. On the bench's lines that is 40–51 % of the
  samples: the run-up to a cloud's edge inside the 4³ bricks that aren't
  skipped whole, and the shell the fade has eaten. On an Apple M4, 250
  sight checks through 4 of 32 clouds take 0.036 instead of 0.040 ms once
  the clouds are grown (−10 %), 0.098 instead of 0.116 ms halfway through
  the fade (−16 %) and 0.027 instead of 0.030 ms late in it (−10 %); with
  `VolumeSteps::HalfVoxel`, 0.053 instead of 0.059, 0.149 instead of
  0.177 and 0.034 instead of 0.040 ms (medians of 11 alternated runs). In
  a level with walls, bullets and explosions, `blocks_sight` is 8–11 %
  faster through the fade. Results are unchanged: the determinism hashes
  are the same. The cells take no more memory than the flags, and about
  6 µs per cloud to build, where the flags took 15–20 µs.
- `Grid::sample_cells`: the byte per cell above is part of each
  volumetric cloud's grid, and the fill builds it within its budget, as a
  stage of its own after the bricks. In 0.1 the flags it replaces were
  kept apart from the grid and built outside the budget on the tick the
  fill finished. `Grid::build_sample_cells` builds them for a grid put
  together by hand; faceted and crystal grids have none.
  `VolumeField::amount` and `VolumeField::density` read them too, so code
  that integrates a cloud itself through `CloudView::volume_field` takes
  the shortcut the world's checks take, with the same amounts to the bit:
  on an Apple M4, 250 lines through 4 of 32 volumetric clouds, every
  cloud's amount summed this way, take 0.29 instead of 0.47 ms once the
  clouds are grown and 0.22 instead of 0.36 ms halfway through the fade
  (−38 %; medians of 11 alternated runs). A grid without the cells gives
  the same answers, more slowly. The new public field breaks code that
  builds a `Grid` with a struct literal: add `sample_cells: Vec::new()`,
  or call `build_sample_cells`. `Smokes::insert` uses a shape's grid as it
  is, so a server that sends shapes should send the cells too, or have
  its clients rebuild them (`fill_cloud` builds them). Results are
  unchanged: the determinism hashes are the same, and fills match in
  every other field. Sight checks cost the same. A volumetric fill all at
  once takes 0.57 instead of 0.56 ms in a voxelised level, as `fill_cloud`
  now builds the cells `Smokes::insert` built; spread over ticks at the
  default budget it still takes 14 ticks.
- Faceted and crystal fills are about a quarter faster. On an Apple M4, in
  a voxelised level, a faceted fill takes 1.14 instead of 1.51 ms all at
  once and a crystal one 1.07 instead of 1.46 ms; at the default budget
  they need 29–33 ticks instead of 37–42, and with the default timeline
  (the cloud starts 0.1 s after the pop) a faceted fill costs 0.097
  instead of 0.128 ms a tick (median; 0.155 instead of 0.195 ms at worst;
  medians of 11 alternated runs). The halo, which keeps puffs out of
  walls, expanded every filled voxel from a heap; it now grows from the
  filled voxels with an open face, through a plain queue, and the voxels
  at the edge of its reach only ask the level about their neighbours. And
  the distance out of the region (the fill and its halo) runs over the
  halo's box and one voxel around it, where it ran over the box of the
  distance in, five voxels bigger. Results are unchanged: the determinism
  hashes are the same, and the grids, puffs and bins of 120 test clouds
  (every style, point, line and box sources, three levels and Z-up
  inches) are the same as before, bit for bit. The halo's charge is
  re-measured (15 work units per voxel it expands, where it was 30 per
  voxel settled), so a faceted fill comes to about 660,000 units instead
  of 850,000.
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
- Volumetric smoke costs 27–32 % less to draw close up at the best dial
  step, with the same picture. Where no ray can meet two smokes' boxes,
  each smoke is marched by a dispatch of its own, with a kernel compiled
  for one smoke: it keeps none of the per-ray intervals the march collects
  for overlapping smokes, and its smoke's data is the same for the whole
  dispatch. Smokes that no point light reaches use a variant without the
  point-light code. On an Apple M4 at 1920×1080, close up at the best step
  (medians of five or six alternated runs), a grown cloud takes 3.67
  instead of 5.15 ms (−29 %), 0.8 s into the bloom 3.73 instead of 5.20
  (−28 %), and halfway through the fade 2.45 instead of 3.36 (−27 %); with
  `NoiseWithinRule::Capped` a grown cloud takes 3.18 instead of 4.46 ms
  (−29 %), and with `Filled` 3.88 instead of 5.71 (−32 %). At mid-range the
  best step takes 0.97 instead of 1.19 ms (−19 %). Close up, the steps in
  between gain 10–32 %, the two cheapest, which march a sixteenth of the
  pixels, up to 9 %; far away the best step gains 5–12 % and the rest is
  within the noise. Where smokes overlap on screen, one march adds them up
  as before, at the same cost. The pixels are the old march's, but for one
  level out of 255 in a few dozen pixels a frame (rounding). The new
  kernels compile on first use, for the noise mode and MSAA a camera uses;
  the old march draws until they are ready. On the M4 the march is short of
  registers rather than of arithmetic: taking a third of the sample loop's
  instructions out (folding constants per smoke, advancing coordinates
  along the ray) made it 6–7 % slower, so that isn't done.
- Volumetric smokes that overlap on screen cost about 40 % less to draw,
  with the same picture. Where two smokes came close on screen, every
  volumetric smoke on screen went through the march that adds up the smokes
  a ray crosses, with room for all 32 slots per ray and the point-light
  code compiled in; it reads those per-ray arrays at every sample, so two
  overlapping smokes cost 3.2 times one smoke. Now smokes are marched in
  groups: those whose screen rectangles come close, directly or through
  others, share one dispatch over their own rectangle, and a smoke apart
  from the rest keeps the single-smoke kernel even while others overlap
  elsewhere on screen. A group's kernel has room for 8 smokes per ray
  wherever no point on screen lies in more than 8 of the group's
  rectangles, and leaves the light code out when no light reaches the
  group; past 8, and until a kernel has compiled, the old march draws. On
  an Apple M4 at 1920×1080 (medians of four or five alternated runs), two
  grenades overlapping on screen take, close up at the best step, 2.75
  instead of 4.57 ms grown (−40 %) and 2.38 instead of 4.19 halfway through
  the fade (−43 %); with `NoiseWithinRule::Capped` 2.63 instead of 4.06
  (−35 %), with `Filled` 3.00 instead of 4.81 (−38 %). At mid-range the
  best step takes 1.11 instead of 1.91 ms (−42 %), far away 0.40 instead
  of 0.82 (−52 %), and close up the cheapest step 1.00 instead of 1.21
  (−17 %). With a third smoke apart from those two, the mid-range view
  takes 1.96 instead of 3.74 ms (−48 %). One smoke on its own costs what
  it did. No pixel changes by more than one level out of 255 (rounding
  where the light code is left out; none changes with one smoke, or with
  the light code in). The demo's `SMOKE_DEMO_SWEEP_SMOKES=2` or `3` puts
  those smokes into the `tiers` and `agreement` scripts.
- Point lights light volumetric and faceted smoke. They never did: the
  plugin read each `PointLight`'s `ViewVisibility` while Bevy had it
  cleared (after `VisibilitySystems::VisibilityPropagate` resets it and
  before `CheckVisibility` sets it again), so muzzle flashes and
  explosions lit the level around a smoke but not the smoke. The lights
  are now read after `CheckVisibility`, which marks a point light visible
  where its range reaches into a camera's view: of the lights whose range
  reaches a smoke, the brightest eight light it, and a light that can't
  reach anything on screen takes none of the eight places. Crystal smoke,
  which Bevy lights itself, was never affected. The picture changes only
  where a light is near smoke, and most while a cloud is thin: the march
  dims a light by the smoke halfway to it, so little of a light inside a
  grown cloud reaches its surface. Volumetric smoke pays for every sample
  within a light's range: on an Apple M4 at 1920×1080, close up at the
  best step (medians of four to seven alternated runs), a grown cloud with
  one muzzle flash in it takes 4.17 instead of 3.67 ms (+14 %), and with
  eight 7.83 ms (2.1×); halfway through the fade, 2.93 instead of 2.83 ms
  with one (+4 %) and 3.56 instead of 2.46 with eight (+45 %). With one
  flash in a grown cloud, `NoiseWithinRule::Capped` costs 23 % more and
  `Filled` 19 % more. At mid-range one flash costs 16 % more at the best
  step; at the two cheapest steps its cost is within the noise. Faceted
  smoke, lit once per pixel at its surface, costs about the same with
  eight lights as with none. Without a light near smoke the pixels and the
  cost are as before. `SmokeSystems::PrepareRender` now ends after Bevy's
  `CheckVisibility`. The demo's `SMOKE_DEMO_SWEEP_LIGHTS=<n>` puts n
  steady muzzle flashes in the tier sweep's smoke.
- Every open hole is drawn. The plugin drew at most 64 holes at once
  across all volumetric and faceted smokes, the oldest smokes' first, so
  with the default 16 holes per cloud the fifth smoke full of holes drew
  none of them: its tunnels and blasts stayed solid smoke where the rule
  saw through. It now has room for 512, 16 for each of 32 smokes, so the
  default tuning always fits, and it warns once if the smokes drawn hold
  more (possible only with `Tuning::max_holes_per_cloud` above 16).
  Crystal smoke, whose puffs shrink on the CPU, was never affected. With
  64 holes or fewer the picture is the same to the pixel and costs the
  same. Past 64, the smokes that lost their holes now pay for them as the
  first four always did: on an Apple M4 at 1920×1080, close up at the
  best step (medians of five alternated runs), six volumetric smokes with
  16 bullet tunnels each take 1.70 instead of 1.51 ms (one such smoke
  alone takes 1.60), and six faceted ones 83.5 instead of 30.2 ms, what
  one faceted smoke with 16 tunnels already took (83.2 ms: tunnels make
  faceted smoke about three times as costly to draw). The demo's
  `SMOKE_DEMO_TUNNELS=<n>` sets up that scene for the `agreement` and
  `tiers` scripts: n older smokes out of view with 16 tunnels each, then
  16 through the smoke in view.
- Crystal puffs are drawn in the frame the rule counts them. The plugin
  moved, showed and hid them after Bevy had worked out what is visible, so
  a puff switched on (as a cloud grows, or as a hole closes) was drawn a
  frame late, and a puff switched off (by a bullet, a blast or the fade)
  was drawn a frame too long at its old size. Bevy also culled each puff
  where it had been the frame before. A cloud whose first frame already
  holds grown puffs, as when a client hears of a pop late and pops it at
  the server's earlier tick, showed a frame late: its puffs were spawned
  hidden and placed the frame after. In the plugin's new test, which
  follows two clouds through their growth, a volley, a blast and the fade,
  the old order left 385 of the 23,501 puff-frames the rule counted
  undrawn and drew 312 that it no longer counted, and a camera on the
  cloud's edge drew or culled 22 against where the puffs were. Now there
  are none: picture and rule agree in every frame, as the crystal style
  promises. The demo's `crystal` clip changes only around its volley, its
  blast and the refill: the old order still showed, for a frame, the puffs
  a shot or the blast had knocked out, and drew those that grow back a
  frame late. `SmokeSystems::Simulate` now ends before Bevy's visibility
  systems (`VisibilityPropagate`, `CalculateBounds`) and asset events
  start, so a system ordered after those can't be ordered before it. The
  two used to run side by side, but making Bevy's wait costs nothing
  measurable: in a headless app on an Apple M4, with 2,000 or 10,000
  meshes, a shadowed sun and a new grenade every 2 s, the CPU frame stayed
  within 1 % (0.43 ms with crystal smoke and 0.36 ms with volumetric
  smoke, medians of 15 alternated runs), frames with a fill running
  included.
- The README no longer says that transparent objects need nothing.
  Alpha-blended things behind or inside volumetric or faceted smoke (a
  tracer, a muzzle-flash sprite, particles) draw over it at full strength,
  and always have: Bevy draws them after the smoke, which writes no depth.
  So they show through smoke that hides everything else. Opaque and
  alpha-masked (`AlphaMode::Mask`) objects are hidden as they should be,
  and crystal smoke hides everything behind it. Its Limitations say so
  now, with what to do meanwhile: make effects that must stay hidden
  opaque or masked, or hide them where `SmokeVision::blocks_sight` says the
  smoke blocks the line from the camera.
- Volumetric smoke costs less to draw at every distance and dial step,
  and faceted smoke a little less, with the same picture to within one
  level out of 255. With MSAA, the smoke's passes that draw onto the scene
  used the multisampled colour target, and a pass over it loads, stores
  and resolves all of it, whatever the smoke's size: about 0.2 ms a frame
  at 1080p on an Apple M4. When a camera has nothing transparent to draw
  after the smoke (Bevy then skips its transparent pass), they now draw
  straight into the resolved image; otherwise they draw as before, so
  that the transparent pass can continue from the multisampled target. At
  the best dial step, which marches every pixel, the volumetric composite
  also takes each pixel's own smoke texel instead of weighing four, and
  reads no depth. On an Apple M4 at 1920×1080 with MSAA (medians of five
  to seven alternated runs), a grown cloud close up takes 3.19 instead of
  3.66 ms at the best step (−13 %) and 0.85 instead of 1.04 at the
  cheapest (−18 %); at mid-range 0.59 instead of 0.94 (−38 %) and 0.19
  instead of 0.44 (−58 %); far away 0.21 instead of 0.46 (−55 %) and 0.07
  instead of 0.26 (−74 %). Halfway through the fade the best step close up
  takes 2.10 instead of 2.58 ms (−18 %). With `NoiseWithinRule::Capped`
  the best step close up takes 2.61 instead of 3.20 ms (−18 %), and with
  `Filled` 3.33 instead of 3.88 (−14 %). Faceted smoke takes 29.7 instead
  of 31.2 ms close up at the best step (−5 %) and 6.6 instead of 7.2 at
  mid-range (−8 %). Without MSAA only the best step gains (−8 % close up),
  and while something transparent is on screen its −10 % is all that's
  left.
- Volumetric smoke marches coarser close up, where marching every pixel
  paid for the same picture: the finest texel of the noise spans several
  pixels there. `VolumetricQuality` gains `resolution_by_distance`, on at
  every step of the dial. A step's resolution is now the finest it
  marches at: each camera marches as coarse as still gives every texel of
  the finest noise a raymarched pixel of its own at the far side of the
  farthest volumetric smoke on screen, down to a quarter. The quality
  controller picks it with the step (`quality::march_resolution`), and
  `SmokeQuality::volumetric_resolution` reports it. It goes coarser only
  once the smoke is within 0.8 of a resolution's distance limit, and back
  finer only past the limit, so a camera at a limit doesn't flip between
  two grains: in the demo's budget walk at the best step, four changes on
  the way in and out. On an Apple M4 at 1920×1080 (medians of four to
  eight alternated runs), close up at the best step a grown cloud takes
  1.45 instead of 3.26 ms (−55 %), and 0.97 instead of 2.13 halfway
  through the fade (−54 %); the high step takes 23 % less, and at
  mid-range the best step 0.44 instead of 0.61 ms (−28 %). With
  `NoiseWithinRule::Capped` the best step close up takes 1.34 instead of
  2.66 ms (−50 %), and with `Filled` 1.57 instead of 3.42 (−54 %). Far
  away, and at the steps that already march at a half or a quarter, nothing
  changes. The picture stays within 19 levels out of 255 (37 with
  `Capped`, along the edges the rule blocks), on at most 0.04 % of a frame
  by more than 8. `tools/fairness.py --volumetric` passes at every step as
  before: the worst view is still the cheapest step's, and the best step's
  close view lets more than a tenth more of the scene through on 0.02 % of
  the smoke's pixels. With `Capped` and `Filled`, nothing the rule blocks
  shows through. Close up the volumetric steps now cost much the same, so
  the budget controller's per-step cost ratios are measured again (the
  faceted ones too). Code that builds `VolumetricQuality` or
  `SmokeQuality` with a struct literal needs the new field. The demo's
  HUD shows the march's resolution when it is coarser than the step's, and
  its budget script logs it.
- `SmokeCamera` has a constant for each step of the dial: `MINIMUM`,
  `LOWER`, `LOW`, `MEDIUM`, `HIGH` and `ULTRA`. `LOW` (0.2) and `MEDIUM`
  (0.4) used to select the dial's "lower" and "low" steps; they now select
  the steps they are named after (0.4 and 0.6). Use `LOWER` and `LOW` for
  what they selected before.
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
- `tools/fairness.py --volumetric` checks the dial for volumetric smoke.
  Given the demo's agreement runs at the best step and at others, it
  compares how much of the scene shows through every smoke pixel, view by
  view, and fails if more than 1 % of a view's smoke pixels let more than
  0.3 more of it through at a cheaper step. In the demo's views, grown,
  blooming and halfway through the fade, at most 0.71 % do (12 of the far
  smoke's 1,693 pixels halfway through the fade, at the cheapest step).
  The README's and the design notes' figure, under 1 % of the pixels by
  more than a tenth, holds by default from the third step up; at the two
  cheapest, which march at a quarter of the resolution, up to 8.6 % of a
  far smoke's pixels let a tenth more through (2.3 % in front of it,
  1.1 % close up), and those docs now say so.
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
