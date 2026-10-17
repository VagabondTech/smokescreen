//! The volumetric style's amount of cloud: density integrated along a ray.

use glam::Vec3;

use crate::{
    grid::{Grid, sample_open_near},
    holes::{HoleShape, hole_attenuation},
    math::{BYTE_TO_UNIT, safe_recip, slab},
    timeline::shape_density,
};

/// A volumetric cloud at one tick. Internal frame.
#[derive(Clone, Copy, Debug)]
pub struct VolumeField<'a> {
    pub grid: &'a Grid,
    pub growth: f32,
    pub dissolve: f32,
    pub holes: &'a [HoleShape],
}

impl VolumeField<'_> {
    /// Density (0..1) at a point: the coarse shape without noise detail,
    /// minus holes.
    pub fn density(&self, p: Vec3) -> f32 {
        let d = shape_density(self.grid.sample_distance(p), self.growth, self.dissolve);
        if d > 0.0 {
            d * hole_attenuation(p, self.holes)
        } else {
            0.0
        }
    }

    /// [`Self::density`] with the grid's near-solid flags
    /// ([`crate::grid::near_solid_cells`]): the same value, found faster
    /// away from walls.
    #[inline]
    pub(crate) fn density_near(&self, near: &[u8], p: Vec3) -> f32 {
        let g = self.grid;
        let sampled = sample_open_near(&g.distance, &g.occupancy, near, g.min, g.voxel_size, g.dims, p);
        let d = shape_density(sampled, self.growth, self.dissolve);
        if d > 0.0 {
            d * hole_attenuation(p, self.holes)
        } else {
            0.0
        }
    }

    /// Add the density integrated along `ro + rd·t`, `t0 ≤ t ≤ t1` (metres of
    /// full-density cloud) to `acc`, a half-voxel step at a time (midpoint
    /// rule). Stops (and returns `true`) as soon as `stop(*acc)` holds.
    pub fn amount(&self, ro: Vec3, rd: Vec3, t0: f32, t1: f32, acc: &mut f32, stop: &dyn Fn(f32) -> bool) -> bool {
        self.amount_by(|p| self.density(p), ro, rd, t0, t1, acc, stop)
    }

    /// [`Self::amount`] with the grid's near-solid flags: the same result.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn amount_near(
        &self,
        near: &[u8],
        ro: Vec3,
        rd: Vec3,
        t0: f32,
        t1: f32,
        acc: &mut f32,
        stop: &dyn Fn(f32) -> bool,
    ) -> bool {
        self.amount_by(|p| self.density_near(near, p), ro, rd, t0, t1, acc, stop)
    }

    /// [`Self::amount`] with the density at a point given by `density`.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn amount_by(
        &self,
        density: impl Fn(Vec3) -> f32,
        ro: Vec3,
        rd: Vec3,
        t0: f32,
        t1: f32,
        acc: &mut f32,
        stop: &dyn Fn(f32) -> bool,
    ) -> bool {
        let len = t1 - t0;
        if len <= 0.0 {
            return false;
        }
        let step = self.grid.voxel_size * 0.5;
        let n = (len / step).ceil().max(1.0) as u32;
        let dt = len / n as f32;
        let inv = safe_recip(rd);
        // Density is exactly 0 at or past this distance.
        let cutoff = self.growth.min(1.2 * (1.0 - self.dissolve)).min(0.999);
        let skip_from = BYTE_TO_UNIT.partition_point(|&v| v < cutoff) as u16;
        if skip_from == 0 {
            return false;
        }
        // Only the samples that can see a voxel the front has reached (and
        // the fade hasn't eaten): the rest are exactly 0. A sample or so of
        // margin either side keeps rounding from mattering.
        let (mut i, mut end) = (0, n);
        if let Some(front) = self.grid.front_bounds.get(skip_from as usize - 1) {
            let Some((lo, hi)) = *front else {
                return false;
            };
            let (a0, a1) = slab(ro, inv, lo, hi);
            let (a0, a1) = (a0.max(t0), a1.min(t1));
            if a1 < a0 {
                return false;
            }
            i = ((a0 - t0) / dt - 1.5).max(0.0) as u32;
            end = (((a1 - t0) / dt + 1.5).max(0.0) as u32).min(n);
        }
        // Bricks the samples pass through: one that's empty, or that the
        // growth front hasn't reached (or the dissolve has already eaten),
        // is skipped whole.
        let mut brick_exit = f32::NEG_INFINITY;
        while i < end {
            let t = t0 + (i as f32 + 0.5) * dt;
            let p = ro + rd * t;
            if t >= brick_exit {
                let (brick, lo, hi) = self.grid.brick_at(p);
                let (_, exit) = slab(ro, inv, lo, hi);
                brick_exit = exit;
                if self.grid.bricks[brick] as u16 >= skip_from && p.cmpge(lo).all() && p.cmple(hi).all() {
                    // On to the first sample past the brick.
                    let next = ((exit - t0) / dt - 0.5).ceil();
                    i = if next > i as f32 { (next as u32).min(end) } else { i + 1 };
                    continue;
                }
            }
            let d = density(p);
            if d > 0.0 {
                *acc += d * dt;
                if stop(*acc) {
                    return true;
                }
            }
            i += 1;
        }
        false
    }
}
