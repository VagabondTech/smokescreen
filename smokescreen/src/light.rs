//! Optional lighting data for renderers: how much of the sun and of the sky
//! each voxel of a cloud can see past level geometry.
//!
//! Not part of the simulation (nothing here changes what hides what), so a
//! server never needs it and a client can compute it whenever it likes, for
//! example on a background thread.

use glam::{IVec3, Vec3};

use crate::{
    grid::{EMPTY_VOXEL, Grid, occupancy},
    level::Level,
    math::sin_cos,
    units::Units,
};

/// How far visibility rays travel, in the caller's units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightRays {
    /// Direction towards the sun in the caller's frame, or `None` for no sun.
    pub sun: Option<Vec3>,
    pub sun_length: f32,
    /// 0 disables the sky rays.
    pub sky_length: f32,
}

/// Per voxel `[sun, sky]` visibility, 0..255 each: smoke under a roof or deep
/// in an alley sees less sky; smoke in the shadow of a wall no sun.
///
/// Rays are cast from every second voxel along each axis (an 8× saving) and a
/// 3×3×3 average fills the voxels in between, which also softens shadow
/// edges inside the cloud.
pub fn visibility<L: Level + ?Sized>(grid: &Grid, level: &L, units: &Units, rays: &LightRays) -> Vec<[u8; 2]> {
    let count = grid.len();
    let mut out = vec![[255u8; 2]; count];
    if grid.is_empty() {
        return out;
    }
    let dims = grid.dims.as_ivec3();
    let decode = |i: usize| {
        let i = i as i32;
        let plane = dims.x * dims.y;
        IVec3::new(i % dims.x, (i % plane) / dims.x, i / plane)
    };
    let center = |c: IVec3| grid.min + (c.as_vec3() + 0.5) * grid.voxel_size;
    let filled: Vec<bool> = grid.distance.iter().map(|&d| d != EMPTY_VOXEL).collect();
    let (mut cmin, mut cmax) = (IVec3::MAX, IVec3::MIN);
    for (i, f) in filled.iter().enumerate() {
        if *f {
            let c = decode(i);
            cmin = cmin.min(c);
            cmax = cmax.max(c);
        }
    }
    let lo = (cmin - 1).max(IVec3::ZERO);
    let hi = (cmax + 1).min(dims - 1);
    // Not from padded voxels: a wall reaches into them, so in a voxelised
    // level every ray from one would start inside it. They take their
    // neighbours' light like any other voxel without rays.
    let sampled: Vec<bool> = (0..count)
        .map(|i| {
            let c = decode(i);
            let padded = grid.occupancy.get(i) == Some(&occupancy::PADDED);
            filled[i] && !padded && (c.x | c.y | c.z) & 1 == 0
        })
        .collect();
    let v = grid.voxel_size;
    let hit = |p: Vec3, d: Vec3, len: f32| {
        level.ray_hit(
            units.from_internal(p),
            units.dir_from_internal(d),
            units.length_from_internal(len),
        )
    };

    if let Some(sun) = rays.sun.map(|s| units.dir_to_internal(s).normalize_or_zero())
        && sun != Vec3::ZERO
    {
        let len = units.length_to_internal(rays.sun_length);
        let raw: Vec<u8> = (0..count)
            .map(|i| {
                if sampled[i] && hit(center(decode(i)) + sun * v * 0.75, sun, len) {
                    0
                } else {
                    255
                }
            })
            .collect();
        smooth(&mut out, 0, &raw, &sampled, dims, lo, hi);
    }
    if rays.sky_length > 0.0 {
        let dirs = sky_directions();
        let len = units.length_to_internal(rays.sky_length);
        let raw: Vec<u8> = (0..count)
            .map(|i| {
                if !sampled[i] {
                    return 255;
                }
                let c = center(decode(i));
                let open = dirs.iter().filter(|d| !hit(c + **d * v * 0.75, **d, len)).count();
                (open * 255 / dirs.len()) as u8
            })
            .collect();
        smooth(&mut out, 1, &raw, &sampled, dims, lo, hi);
    }
    out
}

/// Upper-hemisphere directions (internal frame) for sky visibility.
fn sky_directions() -> [Vec3; 6] {
    let mut dirs = [Vec3::Y; 6];
    let (elevation_sin, elevation_cos) = sin_cos(40.0 * core::f32::consts::PI / 180.0);
    for (i, d) in dirs.iter_mut().enumerate().skip(1) {
        let (s, c) = sin_cos((i as f32 - 1.0) * core::f32::consts::TAU / 5.0);
        *d = Vec3::new(elevation_cos * c, elevation_sin, elevation_cos * s);
    }
    dirs
}

/// Write a 3×3×3 average of `raw` (over voxels marked in `valid`) into one
/// channel. Voxels just outside the cloud get their filled neighbours'
/// average, so trilinear filtering at the boundary doesn't darken the edge;
/// voxels with no sample that near (a wall layer whose only sampled plane
/// lies inside the wall) get their neighbours' averages in a second pass,
/// so smoke pressed against a shaded wall isn't lit up along it.
fn smooth(out: &mut [[u8; 2]], channel: usize, raw: &[u8], valid: &[bool], dims: IVec3, lo: IVec3, hi: IVec3) {
    let index = |c: IVec3| (c.x + dims.x * (c.y + dims.y * c.z)) as usize;
    let neighbours = |c: IVec3| {
        (-1..=1)
            .flat_map(move |dz| (-1..=1).flat_map(move |dy| (-1..=1).map(move |dx| c + IVec3::new(dx, dy, dz))))
            .filter(move |nc| nc.cmpge(IVec3::ZERO).all() && nc.cmplt(dims).all())
    };
    let mut done = vec![false; out.len()];
    for z in lo.z..=hi.z {
        for y in lo.y..=hi.y {
            for x in lo.x..=hi.x {
                let c = IVec3::new(x, y, z);
                let (mut sum, mut n) = (0u32, 0u32);
                for nc in neighbours(c) {
                    let ni = index(nc);
                    if valid[ni] {
                        sum += raw[ni] as u32;
                        n += 1;
                    }
                }
                if let Some(average) = sum.checked_div(n) {
                    out[index(c)][channel] = average as u8;
                    done[index(c)] = true;
                }
            }
        }
    }
    let mut filled = Vec::new();
    for z in lo.z..=hi.z {
        for y in lo.y..=hi.y {
            for x in lo.x..=hi.x {
                let c = IVec3::new(x, y, z);
                if done[index(c)] {
                    continue;
                }
                let (mut sum, mut n) = (0u32, 0u32);
                for nc in neighbours(c) {
                    let ni = index(nc);
                    if done[ni] {
                        sum += out[ni][channel] as u32;
                        n += 1;
                    }
                }
                if let Some(average) = sum.checked_div(n) {
                    filled.push((index(c), average as u8));
                }
            }
        }
    }
    for (i, v) in filled {
        out[i][channel] = v;
    }
}
