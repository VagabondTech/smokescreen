# Contributing

Thanks for helping. This file covers building, testing and the rules that
keep the simulation deterministic. [CLAUDE.md](CLAUDE.md) has a tour of the
code and its invariants (it is written for AI coding sessions, but it is the
best map for anyone); [docs/design.md](docs/design.md) explains how it all
works; [ROADMAP.md](ROADMAP.md) lists what is left.

## Building

- `bevy_smokescreen` needs Rust 1.97.1 or newer (Bevy 0.20's minimum).
- `smokescreen` needs Rust 1.88 or newer, and nothing but `glam` and `libm`.
- On Linux, Bevy's examples need `libasound2-dev libudev-dev libwayland-dev
  libxkbcommon-dev` (or your distribution's equivalents).
- The smoke needs compute shaders: Vulkan, Metal, DX12 or WebGPU.

For quicker local builds, a `.cargo/config.toml` (ignored by git) can switch
to the `lld` or `mold` linker and drop debug info:

```toml
[profile.dev]
debug = 0
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=lld"]
```

## Before you send a change

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p smokescreen --release      # includes the determinism hash
cargo test -p bevy_smokescreen           # builds the examples, runs the README's examples
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

CI runs the same (`.github/workflows/`): the core on macOS (ARM), Linux and
Windows, the plugin on Linux, and the browser build of the demo.

If you changed anything visual, run the demo (`cargo run --release --example
demo`, or `presets`) and look. For the faceted and crystal looks, the
`agreement` and `tiers` captures with `tools/agreement.py` and
`tools/fairness.py` check that the picture never shows through what the
rule blocks (see the top of `examples/demo.rs`).

## Determinism

Every machine in a match must compute the same clouds and the same answers,
bit for bit. In `smokescreen`:

- only `+ - * /`, `sqrt`, comparisons, `floor` and integer maths on `f32`,
  in a fixed order; sines, cube roots and the like from `libm` (via
  `smokescreen::math`);
- no fused multiply-add, no SIMD types (`Vec3A`, `Vec4`, `Mat4`, `Quat`);
- no `HashMap` or `HashSet` iteration, no threads, clocks or unseeded
  randomness.

`smokescreen/clippy.toml` rejects the std functions and types that would
break this, so keep clippy clean.

`smokescreen/tests/determinism.rs` hashes 10,000 query results, and a level
voxelised from meshes with clouds in it. A change that shouldn't change
results (a speed-up, a refactor) must leave the hashes alone. A change that
changes results on purpose must record the new hash, check it
on x86-64 and on aarch64 (CI does both; `CLAUDE.md` shows how to run aarch64
locally under QEMU), and say so in `CHANGELOG.md`: it breaks lockstep with
older versions.

Two more rules the tests enforce:

- **A fill is the same however it is split.** The fill is a state machine
  charged in work units (about 10 ns each); indivisible pieces call
  `work.begin(n)`.
- **Queries don't allocate.** Use the stack buffers (`holes::HOLE_BUFFER`,
  `chords::Intervals`).

## Fairness

The picture must never show through anything the rule blocks, and the
performance dial may change how precisely smoke is drawn, never its shape.
Where the two can't agree exactly, draw more smoke, not less.

## Style

- `rustfmt.toml` sets a 120-column limit.
- Docs and comments: plain words, short sentences, British spelling. Say
  what a thing does and why, in the caller's terms (world units, seconds).
- Public items get doc comments. Numbers in docs (costs, sizes) say what
  they were measured on.

## Licence

By contributing you agree that your work is dual-licensed under MIT and
Apache 2.0, like the rest of the project.
