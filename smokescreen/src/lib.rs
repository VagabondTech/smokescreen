//! # smokescreen
//!
//! A deterministic smoke simulation with no engine dependency: smoke
//! grenades, smoke walls, gas, fog banks. What a cloud looks like at any
//! tick, and how much of it lies between two points. Servers, bots and
//! renderers all ask this crate, so they agree.
//!
//! - **Fill.** When a cloud pops, a Dijkstra flood fill spreads through a
//!   small voxel grid around its source (a point, a line or a box,
//!   [`CloudSource`]) until it reaches the cloud's volume, stopping at walls
//!   and flowing through doorways. It runs a little each tick
//!   ([`Config::fill_budget`]) and is the same however it is split.
//! - **Shape.** [`Style::Volumetric`] clouds are a soft density grown from
//!   the fill; [`Style::Faceted`] clouds are tumbling polyhedral puffs merged
//!   like metaballs; [`Style::Crystal`] clouds are tumbling polyhedra that
//!   don't merge.
//! - **Time.** The caller supplies ticks. A cloud at tick `t` is a pure
//!   function of its [`CloudSpec`] (origin, pop tick, seed, style, source,
//!   volume, timeline), the [`Level`], the [`Tuning`] and `t`.
//! - **Holes.** Bullets and explosions are events with a tick too
//!   ([`Smokes::bullet`], [`Smokes::explosion`]); `Tuning::holes = None` turns
//!   them off.
//! - **The rule.** [`Smokes::occlusion`] is the amount of cloud along a
//!   segment, every cloud added up; [`Smokes::blocks_sight`] is whether that
//!   reaches [`Tuning::sight_threshold`]. Queries allocate nothing and skip
//!   clouds the segment doesn't come near.
//! - **Units.** Positions, lengths and results are in the caller's units with
//!   the caller's up axis ([`Units`]). Internally everything is metres, Y up,
//!   which is also the frame renderers draw in.
//! - **Renderers.** What a renderer reads (each cloud's state and shape at a
//!   tick, in that internal frame) is public but left out of these docs:
//!   it follows how `bevy_smokescreen` draws, and may change in any release.
//!
//! ## Determinism
//!
//! Results are meant to be bit-identical on every platform, given the same
//! inputs and the same [`Level`] answers:
//!
//! - only `+ - * /`, `sqrt` and comparisons on `f32`, in a fixed order; sines,
//!   cube roots and the like come from the pure-Rust `libm` crate; no fused
//!   multiply-add (`clippy.toml` forbids the std functions that would break
//!   this);
//! - glam's scalar `Vec3` and `Mat3` (and integer vectors) only, no SIMD
//!   types or quaternions;
//! - no threads, clocks, unseeded randomness or hash-map iteration; the fill
//!   orders its work with integer keys.
//!
//! The `determinism` test hashes thousands of query results over the lives
//! of eight clouds; run it on each platform you ship and compare.
//!
//! ## Versions
//!
//! - **Results.** 0.2 releases avoid changing results where they can (a
//!   change shows in the `determinism` test's hashes, and the changelog
//!   says so), but to be safe, run the same version on every machine in a
//!   match.
//! - **The API.** Later 0.2 releases may add to it without breaking code
//!   written the way these docs write it. Enums marked `#[non_exhaustive]`
//!   may gain variants, so match them with a `_` arm. The settings structs
//!   ([`Config`], [`Tuning`], [`Timeline`], [`HoleSettings`],
//!   [`FacetSettings`], [`FacetLimits`]) may gain fields, so build them from
//!   their constructors or `Default` and set fields, or use `..` with one.
//!   [`CloudSpec`] gains fields only in a new 0.x: every machine must send
//!   them, so a new one should stop code that sends it field by field from
//!   compiling.
//! - **Hidden items** (left out of these docs) are for `bevy_smokescreen`
//!   and the tests, and may change in any release.
//!
//! ## Example
//!
//! ```
//! use smokescreen::glam::Vec3;
//! use smokescreen::*;
//!
//! // Z-up inches at 64 ticks a second; a flat floor at height 0.
//! let mut smokes = Smokes::new(Config::new(Units::INCHES_Z_UP, 64.0), FlatGround::new(0.0, UpAxis::Z));
//! let origin = Vec3::new(0.0, 0.0, 0.0);
//! smokes.pop(CloudSpec::new(origin, 0, CloudSpec::seed_from_origin(origin), Style::Crystal));
//!
//! // Two seconds later the cloud has grown: it hides a player behind it.
//! let tick = 2 * 64;
//! smokes.advance(tick);
//! let view = smokes.view(tick);
//! let eye = Vec3::new(-400.0, 0.0, 64.0);
//! let enemy = Vec3::new(400.0, 0.0, 64.0);
//! assert!(view.blocks_sight(eye, enemy));
//! ```

#![warn(missing_docs)]

// `grid`, `light` and `math` are hidden rather than private: the renderer
// and the tests use their paths.
mod chords;
mod facets;
mod fill;
#[doc(hidden)]
pub mod grid;
mod holes;
mod level;
#[doc(hidden)]
pub mod light;
#[doc(hidden)]
pub mod math;
mod timeline;
mod units;
mod volume;
mod world;

pub use facets::{FacetLimits, FacetRule, FacetSettings, Polyhedron, ShapeMix};
pub use fill::CloudShape;
pub use holes::{HoleSettings, Impact};
pub use level::{BoxObstacles, FlatGround, Level, ObstacleBox, VoxelBytesError, VoxelObstacles};
pub use timeline::Timeline;
pub use units::{Units, UpAxis};
pub use volume::VolumeSteps;
pub use world::{
    CloudId, CloudInfo, CloudSource, CloudSpec, Config, ConfigError, MAX_GRID_VOXELS, Smokes, Style, Tuning, View,
    fill_cloud,
};

// What bevy_smokescreen reads to draw, and the tests to check: public, but
// not part of the API. Each definition is `#[doc(hidden)]` too, as
// `missing_docs` looks there, not at the re-export.
#[doc(hidden)]
pub use facets::{LivePuff, POP_OVERSHOOT};
#[doc(hidden)]
pub use fill::Facets;
#[doc(hidden)]
pub use holes::HoleShape;
#[doc(hidden)]
pub use world::{CloudState, CloudView};

/// The glam this crate's API takes and returns vectors from.
pub use glam;

/// Runs the README's example as a doc test.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
