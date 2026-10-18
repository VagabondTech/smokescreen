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

pub mod chords;
pub mod facets;
pub mod fill;
pub mod grid;
pub mod holes;
pub mod level;
pub mod light;
pub mod math;
pub mod timeline;
pub mod units;
pub mod volume;
mod world;

pub use chords::{CoreField, CrystalField};
pub use facets::{FacetField, FacetLimits, FacetRule, FacetSettings, LivePuff, Polyhedron, Puff, ShapeMix};
pub use fill::{CloudShape, Facets};
pub use grid::Grid;
pub use holes::{HoleKind, HoleSettings, HoleShape};
pub use level::{BoxObstacles, FlatGround, Level, ObstacleBox, VoxelObstacles};
pub use timeline::{DensityTable, Timeline};
pub use units::{Units, UpAxis};
pub use volume::{VolumeField, VolumeSteps};
pub use world::{
    CloudId, CloudInfo, CloudSource, CloudSpec, CloudState, CloudView, Config, Smokes, Style, Tuning, View, fill_cloud,
};

pub use glam;

/// Runs the README's example as a doc test.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
