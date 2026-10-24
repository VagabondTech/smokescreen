//! Holes punched by bullets and explosions.
//!
//! A hole is an event with a tick, like a pop. Its shape at any later tick is
//! a pure function of the event and that tick, so every machine that is told
//! about the event carves the same hole at the same moment.

use glam::Vec3;

use crate::math::{distance_to_segment, slab, smoothstep};

/// Timing and size of holes. Lengths in the caller's units, times in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HoleSettings {
    /// Radius of a bullet tunnel.
    pub bullet_radius: f32,
    /// Seconds a bullet tunnel stays fully open.
    pub bullet_hold: f32,
    /// Seconds (from the shot) until the tunnel has closed again.
    pub bullet_close: f32,
    /// Default radius of an explosion's cleared sphere.
    pub explosion_radius: f32,
    /// Seconds the explosion hole stays fully clear.
    pub explosion_hold: f32,
    /// Seconds (from the blast) until the smoke has refilled the hole.
    pub explosion_refill: f32,
    /// Fraction of density removed at the centre of an explosion.
    pub explosion_strength: f32,
}

impl HoleSettings {
    /// Defaults, in metres.
    pub fn metric() -> Self {
        Self {
            bullet_radius: 0.38,
            bullet_hold: 0.35,
            bullet_close: 1.8,
            explosion_radius: 3.5,
            explosion_hold: 0.5,
            explosion_refill: 5.0,
            explosion_strength: 0.98,
        }
    }

    /// The same settings with every length passed through `f` (for unit
    /// conversions).
    pub(crate) fn map_lengths(&self, f: impl Fn(f32) -> f32) -> Self {
        Self {
            bullet_radius: f(self.bullet_radius),
            explosion_radius: f(self.explosion_radius),
            ..*self
        }
    }

    /// Seconds after which no hole has any effect.
    pub(crate) fn max_duration(&self) -> f32 {
        self.bullet_close.max(self.explosion_refill)
    }
}

/// What made a hole. Internally in metres, Y up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum HoleKind {
    /// A tunnel along a shot.
    Bullet { start: Vec3, end: Vec3 },
    /// A cleared sphere.
    Explosion { center: Vec3, radius: f32 },
}

/// A hole at one moment: a capsule from `a` to `b` (a sphere when `a == b`).
#[derive(Clone, Copy, Debug, PartialEq)]
#[doc(hidden)]
pub struct HoleShape {
    pub a: Vec3,
    pub b: Vec3,
    pub radius: f32,
    /// 0..1, how much density is removed at the core.
    pub strength: f32,
}

impl HoleShape {
    /// A hole that does nothing.
    pub(crate) const NONE: Self = Self {
        a: Vec3::ZERO,
        b: Vec3::ZERO,
        radius: 0.0,
        strength: 0.0,
    };

    /// Could the hole, as a capsule `grow` wider than itself, touch the ray
    /// `ro + t·dir` (`inv` = 1/dir) for `t0 ≤ t ≤ t1`? A box test: `false`
    /// only when it certainly can't.
    #[inline]
    pub(crate) fn near_ray(&self, ro: Vec3, inv: Vec3, t0: f32, t1: f32, grow: f32) -> bool {
        let r = Vec3::splat(self.radius + grow);
        let (near, far) = slab(ro, inv, self.a.min(self.b) - r, self.a.max(self.b) + r);
        near.max(t0) <= far.min(t1)
    }
}

/// Most holes a query filters on the stack; beyond this it uses them all.
pub(crate) const HOLE_BUFFER: usize = 32;

/// The holes that could touch a ray (see [`HoleShape::near_ray`]), copied
/// into `buf`; or all of them if there are more than it holds. Order is kept,
/// so results are exactly those of using every hole.
#[inline]
pub(crate) fn holes_near<'a>(
    holes: &'a [HoleShape],
    ro: Vec3,
    inv: Vec3,
    t0: f32,
    t1: f32,
    grow: impl Fn(&HoleShape) -> f32,
    buf: &'a mut [HoleShape; HOLE_BUFFER],
) -> &'a [HoleShape] {
    if holes.len() > HOLE_BUFFER {
        return holes;
    }
    let mut n = 0;
    for h in holes {
        if h.near_ray(ro, inv, t0, t1, grow(h)) {
            buf[n] = *h;
            n += 1;
        }
    }
    &buf[..n]
}

impl HoleKind {
    /// The hole `age` seconds after the event, or `None` once it has closed.
    pub(crate) fn shape(&self, age: f32, s: &HoleSettings) -> Option<HoleShape> {
        let t = age;
        if t < 0.0 {
            return None;
        }
        match *self {
            HoleKind::Bullet { start, end } => {
                if t >= s.bullet_close {
                    return None;
                }
                let open = smoothstep(0.0, 0.05, t);
                let strength = 1.0 - smoothstep(s.bullet_hold, s.bullet_close, t);
                Some(HoleShape {
                    a: start,
                    b: end,
                    radius: s.bullet_radius * (0.55 + 0.45 * strength) * (0.5 + 0.5 * open),
                    strength: strength * open,
                })
            }
            HoleKind::Explosion { center, radius } => {
                if t >= s.explosion_refill {
                    return None;
                }
                // Own tuning: the blast opens the hole in about a tenth of a
                // second; after the hold, smoke creeps back in from the rim,
                // slowly at first and faster towards the end (a squared
                // smoothstep), shrinking and thinning the hole together.
                let blast = smoothstep(0.0, 0.12, t);
                let p = smoothstep(s.explosion_hold, s.explosion_refill, t);
                let refill = p * p;
                Some(HoleShape {
                    a: center,
                    b: center,
                    radius: radius * (0.3 + 0.7 * blast) * (1.0 - 0.6 * refill),
                    strength: s.explosion_strength * (1.0 - refill),
                })
            }
        }
    }

    /// Bounds of the hole at its largest.
    pub(crate) fn bounds(&self, s: &HoleSettings) -> (Vec3, Vec3) {
        match *self {
            HoleKind::Bullet { start, end } => {
                let r = Vec3::splat(s.bullet_radius);
                (start.min(end) - r, start.max(end) + r)
            }
            HoleKind::Explosion { center, radius } => (center - Vec3::splat(radius), center + Vec3::splat(radius)),
        }
    }
}

/// Multiplier (0..1) on the volumetric style's density at `p`. Renderers
/// mirror this exactly.
pub(crate) fn hole_attenuation(p: Vec3, holes: &[HoleShape]) -> f32 {
    let mut keep = 1.0;
    for h in holes {
        let d = distance_to_segment(p, h.a, h.b);
        if d < h.radius {
            let core = 1.0 - smoothstep(h.radius * 0.45, h.radius, d);
            keep *= 1.0 - h.strength * core;
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holes_open_and_close() {
        let s = HoleSettings::metric();
        let bullet = HoleKind::Bullet {
            start: Vec3::new(-5.0, 1.0, 0.0),
            end: Vec3::new(5.0, 1.0, 0.0),
        };
        let shape = bullet.shape(0.1, &s).unwrap();
        assert!(hole_attenuation(Vec3::new(0.0, 1.0, 0.0), &[shape]) < 0.05);
        assert_eq!(hole_attenuation(Vec3::new(0.0, 2.0, 0.0), &[shape]), 1.0);
        assert!(bullet.shape(10.0, &s).is_none());
        assert!(bullet.shape(-0.01, &s).is_none());

        let blast = HoleKind::Explosion {
            center: Vec3::ZERO,
            radius: 3.5,
        };
        let shape = blast.shape(0.3, &s).unwrap();
        assert!(hole_attenuation(Vec3::new(1.0, 0.0, 0.0), &[shape]) < 0.1);
        // Refills: smaller and thinner over time, gone at the end.
        let later = blast.shape(3.0, &s).unwrap();
        assert!(later.radius < shape.radius && later.strength < shape.strength);
        assert!(blast.shape(s.explosion_refill, &s).is_none());
    }
}
