//! A cloud's life, and the volumetric style's density model.

use crate::math::{BYTE_TO_UNIT, smoothstep};

/// Timing of a cloud's life, in seconds after the pop. Defaults: starts
/// growing 0.1 s after the pop, full after another 1.4 s, dissolves over the
/// last 3 s of an 18 s life.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timeline {
    /// Seconds from the pop until the cloud starts to grow. The flood fill
    /// runs in this window, a little each tick (see
    /// [`crate::Config::fill_budget`]), with a bigger budget if that is what
    /// it takes to finish in time. 0 fills at the pop.
    ///
    /// With the default budget a volumetric fill takes about 13 ticks and a
    /// faceted one about 33 (at 128 Hz: 0.1 s and 0.26 s). If your grenade
    /// has a fuse, pop it when it comes to rest and set this to the fuse
    /// time: the fill then runs while the grenade sits there.
    pub start: f32,
    /// Seconds for the cloud to bloom out to full size.
    pub grow_time: f32,
    /// Seconds from the pop until the cloud is gone.
    pub lifetime: f32,
    /// Seconds at the end of the lifetime spent dissolving.
    pub fade_time: f32,
}

impl Default for Timeline {
    fn default() -> Self {
        Self {
            start: 0.1,
            grow_time: 1.4,
            lifetime: 18.0,
            fade_time: 3.0,
        }
    }
}

impl Timeline {
    /// Radius of the growth front in normalised fill-distance units, 0 → 1.2.
    pub fn growth(&self, age: f32) -> f32 {
        let x = ((age - self.start) / self.grow_time.max(1e-3)).clamp(0.0, 1.0);
        let y = 1.0 - x;
        1.2 * (1.0 - y * y * y)
    }

    /// 0 while alive, rising to 1 over the fade.
    pub fn dissolve(&self, age: f32) -> f32 {
        ((age - (self.lifetime - self.fade_time)) / self.fade_time.max(1e-3)).clamp(0.0, 1.0)
    }

    /// Has the cloud started to grow?
    pub fn has_started(&self, age: f32) -> bool {
        age >= self.start
    }

    /// Still blooming out (plus a little margin)?
    pub fn is_growing(&self, age: f32) -> bool {
        age < self.start + self.grow_time + 0.05
    }

    /// Gone (past its lifetime)?
    pub fn is_finished(&self, age: f32) -> bool {
        age >= self.lifetime
    }
}

/// Density (0..1) of the volumetric style at a voxel with normalised fill
/// distance `d` (1 = empty).
///
/// `growth` comes from [`Timeline::growth`], `dissolve` from
/// [`Timeline::dissolve`]. The core is solid, the outer shell feathers, the
/// growth front sweeps out from the grenade along the fill's paths, and the
/// dissolve eats the cloud from the outside in. Renderers mirror this exactly.
pub(crate) fn shape_density(d: f32, growth: f32, dissolve: f32) -> f32 {
    if d >= 0.999 {
        return 0.0;
    }
    let k = 1.2 * (1.0 - dissolve);
    if d <= 0.55 && d <= growth - 0.25 && d <= k - 0.3 {
        // Solid core: every smoothstep below is exactly 0.
        return 1.0 - 0.35 * dissolve;
    }
    let shell = 1.0 - smoothstep(0.55, 1.0, d);
    let front = 1.0 - smoothstep(growth - 0.25, growth, d);
    let keep = 1.0 - smoothstep(k - 0.3, k, d);
    shell * front * keep * (1.0 - 0.35 * dissolve)
}

/// A volumetric cloud's density (0..1) for every fill-distance byte at one
/// age: [`shape_density`] of each byte's distance (`byte / 255`).
///
/// A sample interpolates these values between voxels, the way a renderer's
/// linear sampler interpolates a texture of densities baked per voxel. Taking
/// the density of the interpolated distance instead would give a different
/// answer wherever the density curve bends (the feathered shell, the growth
/// front and the fade): this way the rule counts the density the picture
/// draws.
#[derive(Clone, PartialEq)]
pub(crate) struct DensityTable {
    values: [f32; 256],
    growth: f32,
    dissolve: f32,
    empty_from: u8,
}

impl Default for DensityTable {
    /// No density anywhere, as before a cloud starts to grow.
    fn default() -> Self {
        Self {
            values: [0.0; 256],
            growth: 0.0,
            dissolve: 0.0,
            empty_from: 0,
        }
    }
}

impl core::fmt::Debug for DensityTable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DensityTable")
            .field("growth", &self.growth)
            .field("dissolve", &self.dissolve)
            .field("empty_from", &self.empty_from)
            .finish_non_exhaustive()
    }
}

impl DensityTable {
    /// The table of a cloud at the age with this [`Timeline::growth`] and
    /// [`Timeline::dissolve`].
    #[cfg(test)]
    pub(crate) fn new(growth: f32, dissolve: f32) -> Self {
        let mut table = Self::default();
        table.set(growth, dissolve);
        table
    }

    /// Make this the table for `growth` and `dissolve`. Nothing to do if it
    /// already is: the table depends on nothing else.
    pub(crate) fn set(&mut self, growth: f32, dissolve: f32) {
        if growth.to_bits() == self.growth.to_bits() && dissolve.to_bits() == self.dissolve.to_bits() {
            return;
        }
        // Density is exactly 0 at or past this distance: past the growth
        // front, where the fade has eaten, or empty.
        let cutoff = growth.min(1.2 * (1.0 - dissolve)).min(0.999);
        let empty_from = BYTE_TO_UNIT.partition_point(|&v| v < cutoff);
        for (value, &d) in self.values[..empty_from].iter_mut().zip(&BYTE_TO_UNIT) {
            *value = shape_density(d, growth, dissolve);
        }
        self.values[empty_from..].fill(0.0);
        self.growth = growth;
        self.dissolve = dissolve;
        // At most 255: the cutoff is below 1, byte 255's distance.
        self.empty_from = empty_from as u8;
    }

    /// The density at a voxel with this fill-distance byte.
    #[cfg(test)]
    pub(crate) fn get(&self, byte: u8) -> f32 {
        self.values[byte as usize]
    }

    /// Every byte's density, by byte.
    #[inline]
    pub(crate) fn values(&self) -> &[f32; 256] {
        &self.values
    }

    /// Every byte from this one on has density 0: the growth front hasn't
    /// reached its distance yet, or the fade has eaten it. 0 when the cloud
    /// has no density at all.
    #[inline]
    pub(crate) fn empty_from(&self) -> u8 {
        self.empty_from
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_model_behaves() {
        let tl = Timeline::default();
        // Before growth nothing, after growth a solid core.
        assert_eq!(shape_density(0.0, tl.growth(0.0), 0.0), 0.0);
        assert_eq!(tl.growth(tl.start), 0.0);
        assert!(shape_density(0.2, tl.growth(5.0), 0.0) > 0.99);
        // Empty voxels are always empty.
        assert_eq!(shape_density(1.0, tl.growth(5.0), 0.0), 0.0);
        // Fully dissolved at the end of the lifetime.
        assert_eq!(shape_density(0.1, tl.growth(18.0), tl.dissolve(18.0)), 0.0);
        // Outer layers go first while dissolving.
        let half = tl.dissolve(16.5);
        assert!(shape_density(0.1, 1.2, half) > shape_density(0.7, 1.2, half));
    }

    /// The table holds `shape_density` of every byte, to the bit, nothing
    /// from `empty_from` on, and a table set again equals a new one.
    #[test]
    fn density_table_is_shape_density_of_every_byte() {
        let tl = Timeline::default();
        let mut reused = DensityTable::default();
        let ages = (0..=190).map(|i| (i as f32 * 0.1, &tl));
        let beyond = Timeline { grow_time: 0.0, ..tl };
        let extremes = [0.0, 0.1, 17.9, 18.0, 25.0].map(|a| (a, &beyond));
        for (age, tl) in ages.chain(extremes) {
            let (growth, dissolve) = (tl.growth(age), tl.dissolve(age));
            let table = DensityTable::new(growth, dissolve);
            for b in 0..=255u8 {
                let want = shape_density(b as f32 / 255.0, growth, dissolve);
                assert_eq!(table.get(b).to_bits(), want.to_bits(), "byte {b} at {age} s");
                assert!(b < table.empty_from() || table.get(b) == 0.0, "byte {b} at {age} s");
            }
            reused.set(growth, dissolve);
            assert!(reused == table, "at {age} s");
        }
        assert!(DensityTable::default() == DensityTable::new(0.0, 0.0));
    }
}
