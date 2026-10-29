# Changelog

Changes that alter simulation results (and so break lockstep with older
versions) are marked **results**.

## 0.2.0 (2026-10-29)

Simulation results change in this release: a 0.2 server and a 0.1 client
are not in lockstep.

What 0.2.0 buys, against 0.1.1, on an Apple M4 (Mac mini). On the GPU, the
demo's tier sweep at 1920×1080 with MSAA: the medians of five alternated
runs of both versions in one batch. On the CPU, `cargo bench -p
smokescreen`: the medians of 11 alternated runs.

| | 0.1.1 | 0.2.0 |
|---|---|---|
| Volumetric smoke close up, the dial's best step | 5.56 ms | 0.76 ms (−86 %) |
| The same halfway through the fade | 3.43 ms | 0.62 ms (−82 %) |
| Volumetric smoke at mid-range, best step | 1.22 ms | 0.34 ms (−72 %) |
| Volumetric smoke far away, best step | 0.52 ms | 0.19 ms (−64 %) |
| Faceted smoke close up, best step | 30.7 ms | 29.6 ms (−4 %) |
| 250 volumetric sight checks, grown | 0.070 ms | 0.033 ms (−53 %) |
| The same halfway through the fade | 0.220 ms | 0.088 ms (−60 %) |
| 250 crystal sight checks (0.8 m rule), grown | 0.060 ms | 0.030 ms (−50 %) |
| A faceted or crystal fill, all at once | 1.53 / 1.48 ms | 1.14 / 1.09 ms (−25 %) |

Close up, 0.2.0's best step marches at a quarter of the resolution, where
0.1.1's marched every pixel: as coarse as the rays' depth into the smoke
allows. `tools/fairness.py` holds every step of the dial to the same bound
as before. Faceted sight checks, volumetric fills and the per-tick state
of grown clouds cost what they did.

**The faceted style is experimental in 0.2.0.** It costs 20–40 times what
a volumetric smoke costs to draw (13 times far away), and bullet tunnels
make it about three times dearer again. At the dial's cheaper steps,
tunnel mouths you can see right through come out a few pixels wider than
at the best, so players there see a little more through them. Its
simulation and rule are as finished as the other styles'; the label is
about drawing it. The API is unchanged.

**The API is smaller.** Both crates document only what games, servers and
bots use, and both now warn about public items without a doc comment.
What only drew the smoke, tested it or filled it is private now, or
hidden from the docs where `bevy_smokescreen` or the tests still need it.
Hidden items are not part of the API and may change in any release, so
`bevy_smokescreen` depends on exactly this version of `smokescreen`.

- `smokescreen`: the modules are private; everything a game uses is at
  the crate's root, as before. Hidden: `CloudState`, `CloudView`,
  `View::clouds`, `Smokes::internal_tuning`, what a `CloudShape` holds
  (`Facets`, `Puff`, `LivePuff`, `Grid`, `Bins`), `HoleShape`,
  `Polyhedron::face_normals` and `corner_ratio`, and the `grid`, `light`
  and `math` modules. Private: `ChordField`, `CoreField`, `CrystalField`,
  `FacetField` and `VolumeField` (and the `CloudView` methods that made
  them), `DensityTable`, `HoleKind`, `VolumeSteps::step`, the settings'
  `map_lengths`, `FacetSettings::max_reach`, `HoleSettings::max_duration`,
  the geometry helpers (`polyhedron_interval`, `inside_polyhedron`, `smin`,
  `tunnel_sdf`, `hole_sdf`, `hole_attenuation`, `shape_density`,
  `triangle_overlaps_box`, most of `math`), and `CloudId`'s number:
  handles count up per world, so they aren't shared between machines.
  Removed, as nothing called them: `FacetField::normal`,
  `Grid::distance_at`, `holes::Hole` and `math::value_noise`.
- `bevy_smokescreen`: private: `SmokeConfig`, `polyhedron_mesh`,
  `quality::{BudgetController, BudgetInputs, FACET_RELATIVE_COST,
  VOLUMETRIC_RELATIVE_COST}`, and `SmokeWorld`'s field (it derefs to
  `Smokes`, as before).

**The plugin names common setup mistakes.** Each is logged once, with
what to do, and costs nothing per frame:

- no active camera has `SmokeCamera`, so volumetric and faceted smoke
  isn't drawn; or a `SmokeCamera` is orthographic;
- `SmokeObstacle` on an entity without a mesh, such as a glTF scene's
  root (it marks only an entity's own mesh); a marked mesh that failed to
  load, which keeps smokes waiting for the level;
- several directional lights and none marked `SmokeSun` (or more than
  one); a smoke camera lit by its own `AmbientLight` or an environment
  map while `SmokeSettings::ambient_override` is unset;
- a smoke popped inside solid level geometry, whose cloud is empty; a
  `Line` or `Box` source, or a volume, bigger than the fill grid; more
  smokes alive than `Tuning::max_clouds`, which leaves the oldest out;
- `SmokePlugin` added before Bevy's `DefaultPlugins`: a panic that says
  so, where Bevy's own panic named a missing resource.

The warning for a smoke popped with no level now names `SmokeObstacle`
first, and `FlatGround` for open ground on purpose; those for full slots
and for `budget_ms` without GPU timings fire once per app and say more
about the fix. On a device without compute shaders (WebGL2) the game now
keeps running, with crystal smoke and the simulation, and the log says
why volumetric and faceted smoke isn't drawn; before, the smoke's GPU
setup failed wgpu's validation and Bevy quit. No results change.

**`llms.txt`, for coding assistants.** The API of both crates on one page,
in the llmstxt.org layout, with recipes: grenades, walls, gas, fog and
steam; bullets and explosions; bot vision, gas damage and sound; levels
from meshes, boxes, physics engines and glTF scenes; the quality dial and
budget mode; the looks; other units; multiplayer in Bevy; a first-person
weapon; a server on the core alone. It also lists the gotchas and every
setup warning with its fix. Its Rust code is compiled as doc tests, like
the README's, and `bevy_smokescreen`'s package carries it (and no longer
carries `CLAUDE.md`, the notes for sessions working on this repository).
The browser demo's site serves it too.

**One smoke's density at a point.** `density_at(p)` adds up every smoke,
so gas damage counted plain smoke too. `density_of(cloud, p)`, on `View`,
`Smokes` (at a tick) and `SmokeVision`, gives one cloud's share of it, to
the bit; in Bevy, `Smoke::cloud()` gives a smoke's `CloudId`. `llms.txt`'s
gas damage recipe uses it. No results change.

**Networked pops in Bevy.** A `Smoke` always popped on the current tick,
so a cloud a client heard of late ran behind the server's all its life;
clouds popped on `SmokeWorld` with the server's tick were drawn untinted,
with no entity. `Smoke::from_spec(spec)` spawns exactly the cloud a server
announced (its origin, pop tick and every rule, whatever the entity's
position or the clock), with the entity's look and lifecycle.
`Smoke::with_pop_tick(tick)` (the field `pop_tick`) pops a smoke at the
tick of the event that threw it, for games where every machine spawns the
same smoke. A smoke whose entity despawns when it is done now leaves its
cloud in `SmokeWorld` for `Config::history_ticks` more, as the core keeps
every cloud, so views of past ticks (lag compensation) still see it;
despawning the entity sooner still removes the cloud at once. `llms.txt`'s
multiplayer recipe uses `from_spec`. No results change.

**An API that can grow in 0.2 releases, and four changes that couldn't
wait for 0.3.** No results change: the determinism hashes are the same,
and the demo's showcase draws the same pixels.

- `Style` (`SmokeStyle`), `CloudSource`, `SmokeSource`, `NoiseWithinRule`,
  `SmokeResolution`, `VolumeSteps`, `Polyhedron`, `ShapeMix`,
  `FacetShadows`, `FacetOcclusion` and `SmokeSystems` are
  `#[non_exhaustive]`, as `FacetRule` already was, and so is
  `CrystalPuff`. A `match` on one of them needs a `_` arm, so a new style,
  source, mode or shape can come in a 0.2 release. Building them is
  unchanged.
- The style is out of `SmokeSettings`. Its docs said the look could change
  at any time without changing what hides what, but the style decides what
  hides what. Smokes without a style of their own now take
  `DefaultSmokeStyle`, a resource read at each pop and documented as a
  rule: write `DefaultSmokeStyle(style)` where you wrote
  `SmokeSettings { style, .. }`. `SmokeSettings` can now be a player's
  graphics settings as it stands.
- `SmokeBullet` and `SmokeExplosion` take a `tick` (`None` lands them on
  the current tick, as before) and implement `Default`: build them with
  `..default()`. A message used to land on whatever tick the clock showed
  when the plugin read it, so machines in lockstep could stamp one shot on
  different ticks, shots from two fixed steps in one frame shared a tick,
  and networked games had to call `SmokeWorld::bullet` instead.
  `llms.txt`'s multiplayer recipe now sends ticked messages.
- `VoxelObstacles::from_bytes` returns `Result<VoxelObstacles,
  VoxelBytesError>` instead of an `Option`: not voxels, a format this
  version can't read (a newer one's), cut short, or a header that makes no
  sense (or bytes after the voxels). A corrupt header used to panic in
  debug builds, an overflow working out its size; whatever the bytes hold,
  the answer is now the voxels or an error. Building a `VoxelObstacles`
  with a voxel size that isn't above 0, or of more than 2^32 - 1 voxels
  (512 MB of bits), panics with a message that says so, where it
  overflowed before.
- `SmokePlugin::new(config)`, so code needn't spell out the plugin's
  fields: later versions may add some.
- Hidden: `Timeline::growth`, `dissolve` and `is_growing`, the density
  model renderers mirror.
- The docs say what 0.2 releases may change (a "Versions" section in the
  README, and on both crates' front pages): variants of non-exhaustive
  enums, and fields of structs built from a constructor or with
  `..default()`. `CloudSpec` gains fields only in a new 0.x. Releases avoid
  changing results where they can, but every machine in a match should
  run the same version. They also say that a `CloudShape` has no wire
  format (`fill_cloud` is for filling off the main thread; send the
  `CloudSpec`), that a smoke source's offset and box are in world axes,
  and that the look's lengths are in metres in every unit system.

**Dedicated servers in Bevy, gameplay queries that only read, checked
configurations.** No results change: the determinism hashes are the same,
and the demo's showcase draws the same pixels.

- The plugin runs on a Bevy server: `MinimalPlugins` and `TransformPlugin`
  are enough, with no renderer and no assets. It used to panic at startup
  without Bevy's renderer (it registered its shaders anyway), and to refuse
  to start without `AssetPlugin`. Without a renderer it now adds none of
  its drawing systems, so a server does none of their work; before, every
  new cloud's grid also queued for a GPU upload that nothing took, so a
  server would have kept every cloud it ever popped. Without
  `TransformPlugin`, smokes placed by their entity pop at the origin, and
  the plugin now says so, once. The warnings about more smokes than
  `Tuning::max_clouds` and smokes popped inside solid geometry reach
  servers too. The README and `llms.txt` have a server recipe.
- `SmokeVision` only reads (`Res<SmokeWorld>`, `&self` methods), so
  systems that use it run in parallel with each other and with any system
  that only reads `SmokeWorld`. It answers from views the plugin prepares
  when the clock moves in `PreUpdate` and again at the end of
  `SmokeSystems::Simulate`, so every system in `Update` sees the same
  smoke, whatever order they run in. Two things change. A change made
  straight to `SmokeWorld` in `Update` shows from the end of `Simulate`.
  A clock moved after `PreUpdate` (a tick per fixed step, say) needs the
  new `SmokeWorld::prepare_vision(&clock)` after it, or `SmokeVision`
  answers for the tick before, and the plugin warns once; a clock written
  in `First` needs nothing. `mut vision: SmokeVision` now draws an
  `unused_mut` warning: drop the `mut`. When `SmokeClock::draw_tick` is
  behind `tick`, the plugin now works out both ticks' state every frame
  (about 20 ns per faceted or crystal puff), where it worked out the
  current tick's only when asked.
- `SmokeVision::view()` and `drawn_view()` give the current and the drawn
  tick's `SmokeView` (the core's `View`, now in the prelude), which every
  thread can ask at once: `par_iter` over many bots, and helpers of your
  own. Other ticks (lag compensation) stay with `SmokeWorld::view(tick)`.
- `Config::validate()` returns a `ConfigError` (which setting, and what it
  must be) for values that silently made nonsense: a number that isn't
  finite; a tick rate, unit length, voxel size or volume not above 0; a
  `floor_fraction` outside 0 to 1; or a grid of more than
  `MAX_GRID_VOXELS` (2^24) voxels. `Smokes::new`, `fill_cloud` and
  `SmokePlugin` panic with it.
- Builders: `Smoke::with_density`, `with_despawn_when_done`, `with_start`
  (a fuse for one smoke) and `with_fade_time`, with new fields `start` and
  `fade_time`; `CloudSpec::with_volume` and `with_timeline`. A smoke's
  density and `despawn_when_done` could only be set on a binding before:
  `Smoke { density, ..default() }` doesn't compile outside the crate.
- `SmokeLevel::new(level)`, without an `Arc` to make. `Debug` on `Smokes`
  (so on `SmokeWorld`), `View` and `CloudInfo`, as summaries rather than
  their voxels; `PartialEq` on `SmokeSettings` and `SmokeCamera`.

**For networking crates, editors and late joiners: a `serde` feature,
reflection, `Smokes::impacts`, and pop and end events.** No results
change: the determinism hashes are the same, and the demo's showcase
draws the same pixels.

- A `serde` feature in both crates, off by default: `Serialize` and
  `Deserialize` on `CloudSpec`, `CloudSource`, `Style`, `Timeline` and the
  new `Impact` in the core, and on `SmokeBullet` and `SmokeExplosion` in
  the plugin, as networking crates' events need (bevy_replicon,
  lightyear). Without it the core still depends on `glam` and `libm` only.
  Binary formats such as postcard and bincode carry every number exactly,
  and so does `serde_json` for every finite `f32` (all of them checked).
  The serialised form follows the types: send it between machines running
  the same version.
- `Smokes::impacts()` gives back every bullet and explosion a world keeps,
  exactly as it was given, in your units, by tick and in the same order
  within a tick on every machine. With the specs of the clouds still
  alive, that is all a late joiner needs; `Smokes::impact(impact)` adds
  each one back, whatever kinds of impact later versions add. A server
  used to keep its own log: the world kept impacts in metres, which don't
  always convert back to the same bits. Each kept impact takes 40 bytes
  more.
- `SmokePopped` and `SmokeEnded`, entity events for observers, once per
  smoke, for sounds, effects and UI: when its cloud pops (a pop heard late
  at once, with its own tick), and when the cloud is gone, before
  `despawn_when_done` despawns the entity. Each carries the entity, the
  cloud and the tick. They come in `PostUpdate` before Bevy propagates
  transforms, so what an observer spawns is placed that frame. Before, a
  game watched `Changed<Smoke>` for `Smoke::cloud()`.
- Reflection: `Smoke`, `SmokeSource`, `SmokeCamera` and the quality types
  it holds, `SmokeQuality`, `SmokeSettings`, `NoiseWithinRule`,
  `SmokeClock`, `SmokeSun`, `SmokeObstacle`, `CrystalPuff`, `SmokeBullet`
  and `SmokeExplosion` are reflected and registered, for inspectors,
  editors, scene files, and crates that turn glTF extras from Blender into
  components. A smoke's style is left out: it is the core's type, which
  the plugin can't reflect, so a smoke from a scene takes
  `DefaultSmokeStyle`.
- `llms.txt` has recipes for sounds, effects and UI on pops and ends, and
  for a player who joins mid-match.

- **results** The volumetric rule works out density in the same order as
  the picture. A sample used to take the density of the distance
  interpolated between voxels, while the GPU bakes each voxel's density
  and its sampler interpolates those. The two differ wherever the density
  curve bends, most at a cloud's edge: drawn without its noise, the
  picture showed 1.2 to 6 times the amount of cloud the rule counted along
  lines through less than 0.1 m of it, and 3–84 % more along thicker ones.
  Now each cloud's snapshot holds a table of the density of every
  fill-distance byte at that tick, and samples interpolate its values over
  the open voxels. The two now agree to within 1 % halfway through the
  fade from 0.05 m of cloud up, and to within 10 % on a grown cloud, where
  the rest is the march stepping over the start of a cloud's edge (see the
  roadmap). The rule counts more smoke near clouds' edges: 1–3 % more
  along a typical line through a grown cloud, 6–16 % halfway through the
  fade, and 1–4 % more of the lines in the demo's views are blocked.
  Checks are faster, as samples no longer work out three smoothsteps each:
  on an Apple M4, 250 volumetric checks through 4 of 32 clouds take 0.183
  instead of 0.250 ms halfway through the fade and 0.061 instead of
  0.078 ms once the clouds are grown (medians of 11 alternated runs). The
  snapshot builds a table for each volumetric cloud that is growing or
  fading, about 0.15 µs each; a grown cloud keeps its table. The
  determinism hash (`EXPECTED`) changed, while the one for fills in a
  level voxelised from meshes did not: update servers and clients
  together.
- **results** The volumetric rule samples a line every voxel instead of
  every half voxel: `Tuning::volume_steps` (`VolumeSteps::Voxel` by
  default; `HalfVoxel` for the old spacing). Checks cost about a third
  less: on an Apple M4, 250 sight checks through 4 of 32 clouds take 0.040
  instead of 0.059 ms once the clouds are grown (−32 %), 0.116 instead of
  0.178 ms halfway through the fade (−35 %) and 0.030 instead of 0.040 ms
  late in it (−25 %) (medians of 11 alternated runs), and `occlusion` over
  the same lines costs 37–38 % less. The amount of cloud along a line is a
  little less precise. Against samples 1/64 of a voxel apart, over 8,600
  random lines through smoke (on open ground, and in a level with walls,
  bullets and explosions), the median line is within 0.1 % instead of
  0.03 %, the 99th percentile of lines through at least 0.1 m of cloud
  within 13–17 % instead of 8–9 %, and the rule counts 0.2–0.8 % less
  cloud on average. 0.31 % of the lines get a different answer from the
  fine count's, instead of 0.19 %, every one within 10 cm of the
  threshold; 21 of them are clear where the fine count blocks, instead
  of 10. The largest errors are where a line crosses a wall or the ground,
  up to half a step of full-density cloud per crossing. Against the
  picture nothing that matters changes: without the noise the two agree as
  closely, and with `noise_within_rule` capped or filled nothing the rule
  blocks shows through, at the best and the cheapest dial steps.
  `EXPECTED` changed again and now covers both settings; `EXPECTED_MESHES`
  did not. The demo's `SMOKE_DEMO_VOLUME_STEPS=half` samples every half
  voxel.
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
  of its room (inside the fill's space, so clear of walls), minus a
  capsule around each hole. It is the lower bound the exact rule's
  `blocks_sight` already used, as a rule of its own. Like `Core` it never
  blocks a line the picture leaves clear, and its chords are exact, with
  no tracing; it also counts each puff's corners and edges, which `Core`'s
  spheres leave out. Each puff's chord is worked out once per line, and
  what it adds to the length already counted counts at once, so a line
  that is blocked is found early. On an Apple M4, 250 sight checks through
  4 of 32 clouds take 0.042 ms grown, 0.131 ms halfway through the fade
  and 0.075 ms late in it, against `Core`'s 0.044, 0.130 and 0.060 and the
  exact rule's 0.046, 0.73 and 0.36 (medians of 11 alternated runs). It
  counts more than `Core`, though not much more: along the demo's views it
  counts 66–70 % of the exact rule's amount in a grown cloud and 48–64 %
  halfway through the fade, against `Core`'s 59–64 % and 47–58 %; at the
  0.8 m threshold it blocks 78 % of the close view's pixels that the exact
  rule blocks in a grown cloud, against `Core`'s 73 %. Most of the rest is
  the blend between puffs, which only the exact rule counts. `FacetRule`
  is now `#[non_exhaustive]`: a `match` on it needs a `_` arm. The new
  variant breaks exhaustive matches anyway, and later ones won't. The
  determinism hashes are unchanged; the chord rule's results have a hash
  of their own (`EXPECTED_CHORDS`). The demo's
  `SMOKE_DEMO_FACET_RULE=core` or `chords` sets the rule its `agreement`
  script compares with the picture, and `tools/agreement.py` names it.
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
- The byte per cell above is part of each volumetric cloud's fill, and
  the fill builds it within its budget, as a stage of its own after the
  bricks. In 0.1 the flags it replaces were kept apart and built outside
  the budget on the tick the fill finished. Faceted and crystal clouds
  have none. A shape without the cells gives the same answers, more
  slowly. `Smokes::insert` uses a shape as it is, so a server that sends
  shapes should send the cells too, or have its clients rebuild them
  (`fill_cloud` builds them). Results are
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
- Surfaces inside volumetric smoke no longer show a pattern of bands. A
  ray stops between two samples, at the scene or where it leaves its
  smoke's box, and its last sample stood for a whole step, so it reached
  anywhere from nothing to a whole step past a surface inside the smoke,
  as each ray's jitter fell. The jitter's pattern showed on such surfaces:
  bands 4 pixels apart where the march runs at a quarter resolution (the
  two cheapest steps), a fine diagonal grain at a half. The last sample now
  stands for the stretch up to half a step past where the ray stops, the
  same for every ray: what the old overshoot came to on average, so the
  smoke in front of surfaces keeps its weight. Walking past the demo's
  crate 6–7 m from a grown cloud at the high step, a quarter and a half
  resolution now differ by more than 2 levels out of 255 on 0.14 % of the
  pixels instead of 0.46 %, and the picture changes a little less from
  frame to frame. Against the old picture, at most 17 levels change, on at
  most 0.4 % of a frame by more than 2. `tools/fairness.py --volumetric`
  passes as before (its worst view 0.65 % instead of 0.71 %), and with
  `noise_within_rule` capped or filled nothing the rule blocks shows
  through, at the best and the cheapest step. Stopping exactly at the
  surface removed the bands as well, but let a few pixels the rule blocks
  show through at the cheapest steps with `noise_within_rule` set. It
  costs nothing measurable on an Apple M4.
- The volumetric composite reads the depth buffer for fewer pixels. It
  weighs the 2×2 smoke texels around each pixel by position and by how
  close each one's depth is to the pixel's, which reads five depths. Where
  the four agree within 0.03 in every channel, any blend of them agrees
  with any other within that, so no depth weights could move the pixel by
  more: it now blends them by position alone and reads no depth. Close up
  at the best step that is about nine pixels in ten, and a third to two
  thirds at the two cheapest steps, whose texels lie 4 pixels apart. On an
  Apple M4 at 1920×1080 with MSAA (medians of five alternated runs), a
  grown cloud close up takes 1.19 instead of 1.45 ms at the best step
  (−18 %), 1.11 instead of 1.30 at the high step (−15 %) and 0.83 instead
  of 0.87 at the cheapest (−5 %); halfway through the fade the best step
  takes 0.94 instead of 0.99 (−5 %). Without MSAA, close up at the best
  step 1.14 instead of 1.21 ms (−6 %), and 11–13 % less at the steps
  below. Mid-range and far away little changes. No pixel of the demo's
  sweep changes by more than 5 levels out of 255, nor more than 0.009 % of
  a frame by more than 1, and with `noise_within_rule` capped or filled
  nothing the rule blocks shows through.
- Volumetric smoke marches coarser close up, as coarse as where its rays
  actually see smoke allows. The march went as coarse as still gives every
  texel of the finest noise a pixel at the far side of the farthest
  volumetric smoke's box, but rays end inside a dense cloud long before
  that. `SmokeCamera` gains `measured_depth`, on by default: each ray
  carries the end of the last step that held smoke while more than a
  fifth of the scene still showed through, and on every fourth frame the
  march writes it out. A small pass counts each 8×8 tile's farthest into a
  histogram, read back from the GPU, and the quality controller takes its
  97th percentile from three frames back. It takes a resolution at its
  limit and keeps it to 1.25 times the limit, keeps its choice between
  measurements, falls back on the far side of the boxes after the camera
  jumps, and measures only where a coarser resolution is in reach of the
  nearest smoke. `SmokeQuality::measured_depth` reports the depth in use.
  In the demo's close view rays see smoke within 7.0–8.25 m, against
  12.1 m to the far side of the box, so close up every step marches at a
  quarter. On an Apple M4 at 1920×1080 with MSAA (medians of five or six
  alternated runs), close up at the best step a grown cloud takes 0.98
  instead of 1.46 ms (−33 %), and 0.67 instead of 0.99 halfway through the
  fade (−32 %); two overlapping grenades take 1.27 instead of 2.69 (−53 %);
  with `NoiseWithinRule::Capped` 0.95 instead of 1.34 (−29 %), with
  `Filled` 1.03 instead of 1.55 (−33 %). Close up the high, medium and low
  steps take 20–27 % less, and at mid-range the best step takes 0.34
  instead of 0.43 ms (−20 %). Far away the best step takes 0.22 instead of
  0.18 ms, at the edge of what the batch tells apart: the nearest corner of
  a grown cloud's box is just within reach of two-thirds there, so the
  camera measures every fourth frame for nothing. The cheapest steps,
  which already march at a quarter, cost what they did. Close up the
  picture changes by at most 16 levels out of 255, on about 1 % of the
  pixels by more than 1. `tools/fairness.py --volumetric` passes at every
  step (its worst view 0.25 % at 1920×1080), and with `noise_within_rule`
  capped or filled nothing the rule blocks shows through. The readback
  comes a frame or two after the march, so the frame a change of
  resolution lands on depends on the GPU: two players at one tick may march
  at different resolutions for a few frames, within what the dial's
  cheapest steps always do. The demo's captures wait for the GPU every
  frame, so they draw the same pixels every run. Close up every step now
  costs much the same, so the budget controller's per-step cost ratios are
  measured again. Code that builds `SmokeCamera` or `SmokeQuality` with a
  struct literal needs the new fields.
- Close up, the volumetric dial's two cheapest steps no longer cost more
  than its best. With the composite's shortcut and the march at a quarter
  close up (the two entries above) they had: their march takes
  longer steps, which leaves neighbouring smoke texels noisier, so fewer of
  them agreed within the 0.03 that lets the composite blend them without
  reading depth (close up, 29 % of the smoke's pixels at the cheapest step
  against 82 % at the best). Past the medium step's 0.11 m the tolerance
  now grows with the march's step, to 0.071 at the cheapest step, and close
  up 81–83 % of the pixels skip the depth reads at every step. On an Apple
  M4 at 1920×1080 with MSAA (medians of five alternated runs), a grown cloud
  close up takes 0.61 instead of 0.73 ms at the lower step (−17 %) and 0.64
  instead of 0.82 at the cheapest (−22 %); the other steps and views cost
  what they did. The top three steps draw the same pixels; at the three
  cheapest a pixel changes by at most 12 levels out of 255, on at most
  0.02 % of a frame by more than 2. `tools/fairness.py --volumetric` gives
  the same report, and with `noise_within_rule` capped or filled nothing
  the rule blocks shows through.
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
- `cargo bench -p smokescreen` prints the per-tick state as a game pays
  it, viewing the clouds every tick: for 32 grown clouds 0.047 ms faceted,
  0.030 ms crystal and 0.0005 ms volumetric (0.005 ms while all 32
  volumetric clouds fade, as each builds its density table again). It
  used to print only the first state of a tick in a fresh slot of the
  cache, which still follows: about twice as much, and 0.01 ms for
  volumetric clouds, whose tables it all builds. The README's snapshot
  column, which quoted that first figure, is gone; its figures are in the
  text below the table.

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
