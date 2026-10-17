//! Deterministic maths helpers.
//!
//! Only IEEE-754 operations whose results every platform agrees on are used
//! directly: `+ - * /`, `sqrt`, comparisons, `floor`, `abs`, `min`/`max` and
//! conversions. Everything transcendental goes through the pure-Rust `libm`
//! crate. Rust never fuses a multiply and an add on its own, and this crate
//! never asks it to (`clippy.toml` forbids `mul_add`, `powi` and friends).

use glam::{IVec3, Mat3, Vec3};

#[inline]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Sine and cosine, from `libm`: the same bits on every platform.
#[inline]
pub fn sin_cos(x: f32) -> (f32, f32) {
    libm::sincosf(x)
}

/// Cube root, from `libm`: the same bits on every platform.
#[inline]
pub fn cbrt(x: f32) -> f32 {
    libm::cbrtf(x)
}

/// A 32-bit integer hash (lowbias32).
#[inline]
pub fn hash_u32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

/// A hash mapped to 0..1 (24 bits).
#[inline]
pub(crate) fn hash01(x: u32) -> f32 {
    (hash_u32(x) >> 8) as f32 / (1u32 << 24) as f32
}

#[inline]
fn hash3(c: IVec3, seed: u32) -> f32 {
    let h = hash_u32(
        (c.x as u32).wrapping_mul(0x8da6_b343)
            ^ (c.y as u32).wrapping_mul(0xd816_3841)
            ^ (c.z as u32).wrapping_mul(0xcb1a_b31f)
            ^ seed.wrapping_mul(0x9e37_79b9),
    );
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Smooth 3D value noise in 0..1, seeded.
pub fn value_noise(p: Vec3, seed: u32) -> f32 {
    let i = p.floor();
    value_noise_lerp(&value_noise_corners(i.as_ivec3(), seed), p - i)
}

/// The eight corner values of the lattice cell at `i` (x fastest).
#[inline]
pub(crate) fn value_noise_corners(i: IVec3, seed: u32) -> [f32; 8] {
    std::array::from_fn(|n| {
        hash3(
            i + IVec3::new((n & 1) as i32, ((n >> 1) & 1) as i32, (n >> 2) as i32),
            seed,
        )
    })
}

/// [`value_noise`] at fractional position `f` (0..1 per axis) within a cell
/// with the given corner values.
#[inline]
pub(crate) fn value_noise_lerp(c: &[f32; 8], f: Vec3) -> f32 {
    let u = f * f * (Vec3::splat(3.0) - 2.0 * f);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(c[0], c[1], u.x);
    let x10 = lerp(c[2], c[3], u.x);
    let x01 = lerp(c[4], c[5], u.x);
    let x11 = lerp(c[6], c[7], u.x);
    lerp(lerp(x00, x10, u.y), lerp(x01, x11, u.y), u.z)
}

/// Rotation by `angle` radians about the unit vector `axis` (Rodrigues).
pub fn rotation(axis: Vec3, angle: f32) -> Mat3 {
    let (s, c) = sin_cos(angle);
    let t = 1.0 - c;
    let a = axis;
    Mat3::from_cols(
        Vec3::new(c + t * a.x * a.x, t * a.x * a.y + s * a.z, t * a.x * a.z - s * a.y),
        Vec3::new(t * a.x * a.y - s * a.z, c + t * a.y * a.y, t * a.y * a.z + s * a.x),
        Vec3::new(t * a.x * a.z + s * a.y, t * a.y * a.z - s * a.x, c + t * a.z * a.z),
    )
}

/// Ray/box slab test: `(entry, exit)` distances along the ray, given the
/// reciprocal of its direction.
#[inline]
pub(crate) fn slab(ro: Vec3, inv: Vec3, lo: Vec3, hi: Vec3) -> (f32, f32) {
    let t0 = (lo - ro) * inv;
    let t1 = (hi - ro) * inv;
    (t0.min(t1).max_element(), t0.max(t1).min_element())
}

/// Reciprocal of a direction, with zero components mapped to a huge value of
/// the same sign, so slab tests stay finite.
#[inline]
pub(crate) fn safe_recip(d: Vec3) -> Vec3 {
    let r = |v: f32| {
        if v.abs() > 1e-12 {
            1.0 / v
        } else if v.is_sign_negative() {
            -1e12
        } else {
            1e12
        }
    };
    Vec3::new(r(d.x), r(d.y), r(d.z))
}

/// Clip the segment `a..b` against a box. Returns `(t0, t1)` in 0..1.
pub fn clip_segment(a: Vec3, b: Vec3, min: Vec3, max: Vec3) -> Option<(f32, f32)> {
    let (near, far) = slab(a, safe_recip(b - a), min, max);
    let near = near.max(0.0);
    let far = far.min(1.0);
    (far > near).then_some((near, far))
}

/// Distance from `p` to the segment `a..b`.
#[inline]
pub fn distance_to_segment(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let len2 = ab.length_squared();
    let t = if len2 > 1e-12 {
        ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    p.distance(a + ab * t)
}

/// Do two boxes overlap?
#[inline]
pub(crate) fn boxes_overlap(a: (Vec3, Vec3), b: (Vec3, Vec3)) -> bool {
    a.0.cmple(b.1).all() && b.0.cmple(a.1).all()
}

/// `i / 255` for every byte, so texel decoding is a table lookup.
pub(crate) static BYTE_TO_UNIT: [f32; 256] = {
    let mut t = [0.0f32; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = i as f32 / 255.0;
        i += 1;
    }
    t
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_is_orthonormal_and_turns_the_right_way() {
        let axis = Vec3::new(0.3, -0.8, 0.52).normalize();
        let r = rotation(axis, 0.7);
        let should_be_identity = r * r.transpose();
        for (i, col) in [
            should_be_identity.x_axis,
            should_be_identity.y_axis,
            should_be_identity.z_axis,
        ]
        .iter()
        .enumerate()
        {
            let mut e = Vec3::ZERO;
            e[i] = 1.0;
            assert!((*col - e).abs().max_element() < 1e-6);
        }
        assert!((r * axis - axis).abs().max_element() < 1e-6);
        // A quarter turn about Z takes X to Y.
        let q = rotation(Vec3::Z, core::f32::consts::FRAC_PI_2) * Vec3::X;
        assert!((q - Vec3::Y).abs().max_element() < 1e-6, "{q:?}");
    }

    #[test]
    fn decode_table_matches_division() {
        for (i, v) in BYTE_TO_UNIT.iter().enumerate() {
            assert_eq!(*v, i as f32 / 255.0);
        }
    }
}
