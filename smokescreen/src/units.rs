//! The caller's units and up axis.
//!
//! Everything that crosses this crate's API (positions, lengths, volumes and
//! query results) is in the caller's units. Internally the simulation works in
//! metres with Y up, which is also the frame a renderer draws in; [`Units`]
//! converts at the boundary, with one multiplication or division per
//! component, so the conversion is as deterministic as everything else.

use glam::Vec3;

/// Which axis points up in the caller's world. Both frames are right-handed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum UpAxis {
    /// Y up (Bevy's convention).
    #[default]
    Y,
    /// Z up. The internal frame then has `x = x`, `y = z`, `z = -y`.
    Z,
}

/// How the caller measures the world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Units {
    pub up: UpAxis,
    /// Length of one caller unit in metres: 1 for metres, 0.0254 for inches.
    pub metres_per_unit: f32,
}

impl Default for Units {
    fn default() -> Self {
        Self::METRES_Y_UP
    }
}

impl Units {
    /// Metres, Y up.
    pub const METRES_Y_UP: Self = Self {
        up: UpAxis::Y,
        metres_per_unit: 1.0,
    };
    /// Inches, Z up.
    pub const INCHES_Z_UP: Self = Self {
        up: UpAxis::Z,
        metres_per_unit: 0.0254,
    };

    pub const fn new(up: UpAxis, metres_per_unit: f32) -> Self {
        Self { up, metres_per_unit }
    }

    /// A caller direction in the internal frame (rotation only).
    #[inline]
    pub fn dir_to_internal(&self, d: Vec3) -> Vec3 {
        match self.up {
            UpAxis::Y => d,
            UpAxis::Z => Vec3::new(d.x, d.z, -d.y),
        }
    }

    /// An internal direction in the caller's frame (rotation only).
    #[inline]
    pub fn dir_from_internal(&self, d: Vec3) -> Vec3 {
        match self.up {
            UpAxis::Y => d,
            UpAxis::Z => Vec3::new(d.x, -d.z, d.y),
        }
    }

    /// A caller position in internal metres, Y up.
    #[inline]
    pub fn to_internal(&self, p: Vec3) -> Vec3 {
        self.dir_to_internal(p) * self.metres_per_unit
    }

    /// An internal position in the caller's units and frame.
    #[inline]
    pub fn from_internal(&self, p: Vec3) -> Vec3 {
        self.dir_from_internal(p) / self.metres_per_unit
    }

    /// A caller length in metres.
    #[inline]
    pub fn length_to_internal(&self, l: f32) -> f32 {
        l * self.metres_per_unit
    }

    /// A length in metres in caller units.
    #[inline]
    pub fn length_from_internal(&self, l: f32) -> f32 {
        l / self.metres_per_unit
    }

    /// A caller volume in cubic metres.
    #[inline]
    pub fn volume_to_internal(&self, v: f32) -> f32 {
        let m = self.metres_per_unit;
        v * (m * m * m)
    }

    /// A volume in cubic metres in caller units.
    #[inline]
    pub fn volume_from_internal(&self, v: f32) -> f32 {
        let m = self.metres_per_unit;
        v / (m * m * m)
    }

    /// The caller's up direction, in the caller's frame.
    pub fn up_vector(&self) -> Vec3 {
        match self.up {
            UpAxis::Y => Vec3::Y,
            UpAxis::Z => Vec3::Z,
        }
    }

    /// Component of a caller position along the up axis.
    #[inline]
    pub fn height(&self, p: Vec3) -> f32 {
        match self.up {
            UpAxis::Y => p.y,
            UpAxis::Z => p.z,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z_up_round_trip_and_handedness() {
        let u = Units::INCHES_Z_UP;
        let p = Vec3::new(10.0, 20.0, 30.0);
        let i = u.to_internal(p);
        // Up (caller z) becomes internal y, forward (caller y) becomes -z.
        assert!((i.y - 30.0 * 0.0254).abs() < 1e-6);
        assert!((i.z + 20.0 * 0.0254).abs() < 1e-6);
        let back = u.from_internal(i);
        assert!((back - p).abs().max_element() < 1e-4, "{back:?}");
        // Right-handed: x × y = z holds in both frames.
        let (x, y) = (u.dir_to_internal(Vec3::X), u.dir_to_internal(Vec3::Y));
        assert_eq!(x.cross(y), u.dir_to_internal(Vec3::Z));
        assert_eq!(u.dir_to_internal(u.up_vector()), Vec3::Y);
    }
}
