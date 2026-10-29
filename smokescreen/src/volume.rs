//! The volumetric style's amount of cloud: density integrated along a ray.

use glam::Vec3;

use crate::{
    grid::{Grid, sample_open_near},
    holes::{HoleShape, hole_attenuation},
    math::{safe_recip, slab},
    timeline::DensityTable,
};

/// How far apart the volumetric rule's samples lie along a line
/// ([`crate::Tuning::volume_steps`]). Later versions may add spacings, so a
/// `match` on it needs a `_` arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VolumeSteps {
    /// A sample every half voxel: twice as many, so a check costs about 1.5
    /// times as much, and the amount of cloud it finds is a little more
    /// precise.
    HalfVoxel,
    /// A sample every voxel.
    #[default]
    Voxel,
}

impl VolumeSteps {
    /// The distance between samples, for voxels `voxel_size` long.
    pub(crate) fn step(self, voxel_size: f32) -> f32 {
        match self {
            Self::HalfVoxel => voxel_size * 0.5,
            Self::Voxel => voxel_size,
        }
    }
}

/// A volumetric cloud at one tick. Internal frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VolumeField<'a> {
    pub(crate) grid: &'a Grid,
    /// The density of each fill-distance byte at this tick (the cloud's
    /// [`crate::CloudState::density`]).
    pub(crate) density: &'a DensityTable,
    pub(crate) holes: &'a [HoleShape],
    /// Distance between samples along a line, metres
    /// ([`VolumeSteps::step`] of the grid's voxel size).
    pub(crate) step: f32,
}

impl VolumeField<'_> {
    /// Density (0..1) at a point: the coarse shape without noise detail,
    /// minus holes. The voxels' densities are interpolated, open voxels
    /// only (see [`Grid::sample_distance`]), as a renderer's sampler does
    /// with densities baked per voxel.
    pub(crate) fn density(&self, p: Vec3) -> f32 {
        self.density_near(&self.grid.sample_cells, self.density.empty_from().max(2), p)
    }

    /// [`Self::density`] with the grid's sample cells `cells`
    /// ([`Grid::sample_cells`]): the same value, found faster away from
    /// walls and where the sample reads nothing. Every byte from
    /// `zero_from` (at least 2) on must have density 0. Always inlined: as
    /// a call of its own per sample, it would cost `amount` dearly.
    #[inline(always)]
    fn density_near(&self, cells: &[u8], zero_from: u8, p: Vec3) -> f32 {
        let g = self.grid;
        let d = sample_open_near(
            &g.distance,
            self.density.values(),
            &g.occupancy,
            cells,
            zero_from,
            g.min,
            g.voxel_size,
            g.dims,
            p,
        );
        if d > 0.0 {
            d * hole_attenuation(p, self.holes)
        } else {
            0.0
        }
    }

    /// Add the density integrated along `ro + rd·t`, `t0 ≤ t ≤ t1` (metres of
    /// full-density cloud) to `acc`, about [`Self::step`] at a time
    /// (midpoint rule). Stops (and returns `true`) as soon as `stop(*acc)`
    /// holds. Skips what reads nothing: the parts of the line outside the
    /// grid's front bounds, its empty bricks and, from the grid's sample
    /// cells, the samples whose eight voxels all have density 0.
    pub(crate) fn amount(
        &self,
        ro: Vec3,
        rd: Vec3,
        t0: f32,
        t1: f32,
        acc: &mut f32,
        stop: &dyn Fn(f32) -> bool,
    ) -> bool {
        // Every byte from `empty_from` on has density 0. The cells store
        // bytes 0 and 1 alike, so only cells from 2 on read nothing.
        let zero_from = self.density.empty_from().max(2);
        // A slice, taken once: the samples don't reload it from the grid.
        let cells = self.grid.sample_cells.as_slice();
        self.amount_by(|p| self.density_near(cells, zero_from, p), ro, rd, t0, t1, acc, stop)
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
        let n = (len / self.step.max(1e-4)).ceil().max(1.0) as u32;
        let dt = len / n as f32;
        let inv = safe_recip(rd);
        // Every byte from this one on has density 0.
        let skip_from = self.density.empty_from();
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
                if self.grid.bricks[brick] >= skip_from && p.cmpge(lo).all() && p.cmple(hi).all() {
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

#[cfg(test)]
mod tests {
    use glam::UVec3;

    use super::*;
    use crate::{
        grid::{EMPTY_VOXEL, occupancy},
        math::hash_u32,
        timeline::shape_density,
    };

    /// A sample is the average of the densities of the open voxels around
    /// it, weighted as a trilinear filter weights them: the order a
    /// renderer's sampler works in with densities baked per voxel, not the
    /// density of the averaged distance. With the grid's sample cells it is
    /// the very same value.
    #[test]
    fn samples_average_the_voxels_densities() {
        let dims = UVec3::new(8, 8, 8);
        let n = (dims.x * dims.y * dims.z) as usize;
        let bytes = [0, 120, 150, 175, 204, EMPTY_VOXEL];
        let grid = Grid {
            min: Vec3::new(-1.0, 0.0, 2.0),
            voxel_size: 0.3,
            dims,
            distance: (0..n)
                .map(|i| bytes[hash_u32(i as u32) as usize % bytes.len()])
                .collect(),
            region: vec![255; n],
            occupancy: (0..n)
                .map(|i| match hash_u32(i as u32 ^ 0x9e37_79b9) % 7 {
                    0 => occupancy::SOLID,
                    1 => occupancy::PADDED,
                    _ => occupancy::OPEN,
                })
                .collect(),
            filled: n as u32,
            bounds_min: Vec3::ZERO,
            bounds_max: Vec3::ZERO,
            bricks: Vec::new(),
            front_bounds: Vec::new(),
            sample_cells: Vec::new(),
        };
        let mut celled = grid.clone();
        celled.build_sample_cells();
        let corner = |n: u32| UVec3::new(n & 1, (n >> 1) & 1, n >> 2);
        for (growth, dissolve) in [(1.2, 0.0), (0.8, 0.0), (1.2, 0.5), (1.2, 0.9)] {
            let table = DensityTable::new(growth, dissolve);
            let field = VolumeField {
                grid: &grid,
                density: &table,
                holes: &[],
                step: VolumeSteps::HalfVoxel.step(grid.voxel_size),
            };
            let with_cells = VolumeField { grid: &celled, ..field };
            for k in 0..2000u32 {
                let r = |s: u32| (hash_u32(k.wrapping_mul(7) ^ s) >> 8) as f32 / (1u32 << 24) as f32;
                // Between voxel centres, away from the grid's edges.
                let g = Vec3::new(r(1), r(2), r(3)) * (dims - 1).as_vec3() * 0.999;
                let p = grid.min + (g + 0.5) * grid.voxel_size;
                let (c, f) = (g.floor().as_uvec3(), g - g.floor());
                let (mut sum, mut weight) = (0.0, 0.0);
                for o in (0..8).map(corner) {
                    let i = grid.index(c + o);
                    if grid.occupancy[i] == occupancy::SOLID {
                        continue;
                    }
                    let w = Vec3::select(o.cmpeq(UVec3::ONE), f, 1.0 - f);
                    let w = w.x * w.y * w.z;
                    sum += w * shape_density(grid.distance[i] as f32 / 255.0, growth, dissolve);
                    weight += w;
                }
                let want = if weight > 0.0 { sum / weight } else { 0.0 };
                let got = field.density(p);
                assert!((got - want).abs() < 1e-5, "{got} against {want} at {p:?}");
                assert_eq!(with_cells.density(p).to_bits(), got.to_bits(), "at {p:?}");
            }
        }
    }
}
