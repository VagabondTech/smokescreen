//! Tiling 3D noise used to break the voxel cloud up into billows and wisps.
//!
//! Generated once at startup on a background thread, split across all cores
//! (about 0.6 s of CPU work in a release build), while the window and GPU
//! device are being created. Channels, all tiling over the cube:
//! - R: Perlin–Worley, the billowy "cauliflower" base shape
//! - G: Worley fBm, medium detail
//! - B: Worley fBm, fine detail
//! - A: Perlin fBm, used to swirl the detail lookups

use bevy::math::{IVec3, Vec3};

use smokescreen::math::hash_u32;

/// Edge length of the noise texture in texels.
pub const NOISE_SIZE: u32 = 64;

struct Worley {
    freq: i32,
    points: Vec<Vec3>,
}

impl Worley {
    fn new(freq: i32, seed: u32) -> Self {
        let n = (freq * freq * freq) as usize;
        let mut points = Vec::with_capacity(n);
        for i in 0..n as u32 {
            let h = |k: u32| hash_u32(i.wrapping_mul(0x9e37_79b9) ^ seed ^ k) as f32 / u32::MAX as f32;
            points.push(Vec3::new(h(0x68e3_1da4), h(0xb529_7a4d), h(0x1b56_c4e9)));
        }
        Self { freq, points }
    }

    /// Inverted distance to the nearest feature point: 1 at points, ~0 between.
    fn sample(&self, p: Vec3) -> f32 {
        let f = self.freq;
        let q = p * f as f32;
        let cell = q.floor().as_ivec3();
        let local = q - cell.as_vec3();
        let mut best = f32::MAX;
        for z in -1..=1 {
            for y in -1..=1 {
                for x in -1..=1 {
                    let o = IVec3::new(x, y, z);
                    let c = (cell + o).rem_euclid(IVec3::splat(f));
                    let point = self.points[(c.x + f * (c.y + f * c.z)) as usize];
                    let d = (o.as_vec3() + point - local).length_squared();
                    best = best.min(d);
                }
            }
        }
        (1.0 - best.sqrt()).clamp(0.0, 1.0)
    }
}

fn fade(t: Vec3) -> Vec3 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Periodic gradient noise with `period` lattice cells over the unit cube.
fn perlin(p: Vec3, period: i32, seed: u32) -> f32 {
    let q = p * period as f32;
    let cell = q.floor().as_ivec3();
    let f = q - cell.as_vec3();
    let u = fade(f);
    let grad = |c: IVec3, d: Vec3| {
        let c = c.rem_euclid(IVec3::splat(period));
        let h = hash_u32(
            (c.x as u32).wrapping_mul(0x8da6_b343)
                ^ (c.y as u32).wrapping_mul(0xd816_3841)
                ^ (c.z as u32).wrapping_mul(0xcb1a_b31f)
                ^ seed,
        );
        // 12 edge directions of a cube.
        let g = match h % 12 {
            0 => Vec3::new(1.0, 1.0, 0.0),
            1 => Vec3::new(-1.0, 1.0, 0.0),
            2 => Vec3::new(1.0, -1.0, 0.0),
            3 => Vec3::new(-1.0, -1.0, 0.0),
            4 => Vec3::new(1.0, 0.0, 1.0),
            5 => Vec3::new(-1.0, 0.0, 1.0),
            6 => Vec3::new(1.0, 0.0, -1.0),
            7 => Vec3::new(-1.0, 0.0, -1.0),
            8 => Vec3::new(0.0, 1.0, 1.0),
            9 => Vec3::new(0.0, -1.0, 1.0),
            10 => Vec3::new(0.0, 1.0, -1.0),
            _ => Vec3::new(0.0, -1.0, -1.0),
        };
        g.dot(d)
    };
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let mut corners = [0.0; 8];
    for (i, corner) in corners.iter_mut().enumerate() {
        let o = IVec3::new((i & 1) as i32, ((i >> 1) & 1) as i32, ((i >> 2) & 1) as i32);
        *corner = grad(cell + o, f - o.as_vec3());
    }
    let x00 = lerp(corners[0], corners[1], u.x);
    let x10 = lerp(corners[2], corners[3], u.x);
    let x01 = lerp(corners[4], corners[5], u.x);
    let x11 = lerp(corners[6], corners[7], u.x);
    // Roughly -1..1.
    lerp(lerp(x00, x10, u.y), lerp(x01, x11, u.y), u.z)
}

fn remap(v: f32, lo: f32, hi: f32, new_lo: f32, new_hi: f32) -> f32 {
    new_lo + (v - lo) / (hi - lo) * (new_hi - new_lo)
}

/// Generate the RGBA8 noise volume (`NOISE_SIZE`³ texels, x fastest), using
/// every available core.
pub fn generate_noise(seed: u32) -> Vec<u8> {
    let n = NOISE_SIZE as usize;
    let layers = NoiseLayers::new(seed);
    let mut out = vec![0u8; n * n * n * 4];
    let slice_bytes = n * n * 4;

    #[cfg(not(target_arch = "wasm32"))]
    {
        let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).clamp(1, 16);
        let slices_per_chunk = n.div_ceil(threads);
        std::thread::scope(|scope| {
            for (chunk_index, chunk) in out.chunks_mut(slice_bytes * slices_per_chunk).enumerate() {
                let layers = &layers;
                scope.spawn(move || {
                    for (k, slice) in chunk.chunks_mut(slice_bytes).enumerate() {
                        layers.fill_slice(chunk_index * slices_per_chunk + k, slice);
                    }
                });
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    for (z, slice) in out.chunks_mut(slice_bytes).enumerate() {
        layers.fill_slice(z, slice);
    }
    out
}

struct NoiseLayers {
    seed: u32,
    w4: Worley,
    w8: Worley,
    w16: Worley,
    w32: Worley,
    w12: Worley,
    w24: Worley,
}

impl NoiseLayers {
    fn new(seed: u32) -> Self {
        Self {
            seed,
            w4: Worley::new(4, seed ^ 0x01),
            w8: Worley::new(8, seed ^ 0x02),
            w16: Worley::new(16, seed ^ 0x03),
            w32: Worley::new(32, seed ^ 0x04),
            w12: Worley::new(12, seed ^ 0x05),
            w24: Worley::new(24, seed ^ 0x06),
        }
    }

    /// Fill one z slice (`NOISE_SIZE`² RGBA texels).
    fn fill_slice(&self, z: usize, out: &mut [u8]) {
        let n = NOISE_SIZE as usize;
        let seed = self.seed;
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        for y in 0..n {
            for x in 0..n {
                let p = (Vec3::new(x as f32, y as f32, z as f32) + 0.5) / n as f32;
                let a8 = self.w8.sample(p);
                let a16 = self.w16.sample(p);
                let a32 = self.w32.sample(p);

                let worley_base = self.w4.sample(p) * 0.625 + a8 * 0.25 + a16 * 0.125;
                let perlin_fbm =
                    perlin(p, 4, seed) * 0.5 + perlin(p, 8, seed ^ 0x10) * 0.25 + perlin(p, 16, seed ^ 0x20) * 0.125;
                let perlin01 = (perlin_fbm * 0.7 + 0.5).clamp(0.0, 1.0);
                // Perlin-Worley (after Schneider): Worley gives the round billows,
                // Perlin decides where billows merge and where creases cut deep.
                let perlin_worley = remap(worley_base, (1.0 - perlin01) * 0.45, 1.0, 0.0, 1.0).clamp(0.0, 1.0);

                let detail_mid = a8 * 0.625 + a16 * 0.25 + a32 * 0.125;
                let detail_fine = self.w12.sample(p) * 0.625 + self.w24.sample(p) * 0.25 + a32 * 0.125;
                let swirl = (perlin(p, 4, seed ^ 0x30) * 0.5 + perlin(p, 8, seed ^ 0x40) * 0.25) * 0.7 + 0.5;

                let i = (x + n * y) * 4;
                out[i] = to_u8(perlin_worley);
                out[i + 1] = to_u8(detail_mid);
                out[i + 2] = to_u8(detail_fine);
                out[i + 3] = to_u8(swirl);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worley_tiles() {
        let w = Worley::new(4, 3);
        let p = Vec3::new(0.13, 0.71, 0.42);
        assert!((w.sample(p) - w.sample(p + Vec3::X)).abs() < 1e-5);
        assert!((perlin(p, 4, 9) - perlin(p + Vec3::Z, 4, 9)).abs() < 1e-5);
    }

    #[test]
    fn noise_has_contrast() {
        let data = generate_noise(1);
        assert_eq!(data.len(), (NOISE_SIZE.pow(3) * 4) as usize);
        for channel in 0..4 {
            let values: Vec<f32> = data.iter().skip(channel).step_by(4).map(|&v| v as f32).collect();
            let mean = values.iter().sum::<f32>() / values.len() as f32;
            let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / values.len() as f32;
            assert!(var.sqrt() > 12.0, "channel {channel} is too flat (sd {})", var.sqrt());
            assert!(mean > 40.0 && mean < 215.0, "channel {channel} mean {mean}");
        }
    }
}
