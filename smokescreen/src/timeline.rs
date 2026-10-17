//! A cloud's life, and the volumetric style's density model.

use crate::math::smoothstep;

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
    /// faceted one about 43 (at 128 Hz: 0.1 s and 0.33 s). If your grenade
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
pub fn shape_density(d: f32, growth: f32, dissolve: f32) -> f32 {
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
}
