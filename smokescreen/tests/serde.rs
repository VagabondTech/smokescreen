//! The `serde` feature: what machines send each other comes back exactly
//! through a binary format, and a late joiner fed from the bytes computes
//! the same clouds.

#![cfg(feature = "serde")]

mod common;

use serde::Serialize;
use serde::de::DeserializeOwned;
use smokescreen::glam::Vec3;
use smokescreen::*;

/// `value` through postcard and back, with its bytes.
fn round_trip<T: Serialize + DeserializeOwned>(value: &T) -> (T, Vec<u8>) {
    let bytes = postcard::to_allocvec(value).expect("serialises");
    (postcard::from_bytes(&bytes).expect("deserialises"), bytes)
}

/// Every style and source, with and without a volume and a timeline, and
/// both kinds of impact, come back equal and in the same bits (the bytes of
/// the copy are the bytes of the original).
#[test]
fn specs_and_impacts_come_back_exactly() {
    let odd = Vec3::new(-0.0, 1.0e-40, f32::MAX);
    let mut specs = Vec::new();
    for style in [Style::Volumetric, Style::Faceted, Style::Crystal] {
        for source in [
            CloudSource::Point,
            CloudSource::Line {
                end: Vec3::new(3.25, -0.1, 7.0),
            },
            CloudSource::Box {
                half_extents: Vec3::new(2.0, 1.5, 0.3),
            },
        ] {
            let origin = Vec3::new(12.345_679, -0.0, 1.0e-3);
            let spec = CloudSpec::new(origin, 1 << 40, CloudSpec::seed_from_origin(origin), style).with_source(source);
            specs.push(spec);
            specs.push(spec.with_volume(123.456).with_timeline(Timeline {
                start: 2.5,
                grow_time: 0.1,
                lifetime: 1.0e7,
                fade_time: 0.0,
            }));
        }
    }
    specs.push(CloudSpec::new(odd, u64::MAX, u32::MAX, Style::Crystal));
    for spec in &specs {
        let (back, bytes) = round_trip(spec);
        assert_eq!(back, *spec);
        assert_eq!(round_trip(&back).1, bytes, "{spec:?}");
    }

    let impacts = [
        Impact::Bullet {
            start: Vec3::new(-393.7, 11.1, 47.3),
            end: odd,
            tick: 0,
        },
        Impact::Explosion {
            center: Vec3::new(12.3, -45.6, 7.8),
            radius: None,
            tick: u64::MAX,
        },
        Impact::Explosion {
            center: odd,
            radius: Some(-0.0),
            tick: 77,
        },
    ];
    for impact in &impacts {
        let (back, bytes) = round_trip(impact);
        assert_eq!(back, *impact);
        assert_eq!(round_trip(&back).1, bytes, "{impact:?}");
    }
}

/// A late joiner sent the clouds still alive and the impacts as bytes
/// computes the clouds of the world it joins, to the bit, in Z-up inches.
#[test]
fn a_late_joiner_fed_from_bytes_agrees() {
    let inches = common::z_up_inches();
    let level = common::level_in(inches);
    let config = Config::new(inches, 64.0);
    let mut server = Smokes::new(config.clone(), &level);
    let styles = [Style::Volumetric, Style::Faceted, Style::Crystal];
    for (k, (origin, seed)) in common::pops().into_iter().take(6).enumerate() {
        server.pop(CloudSpec::new(
            inches.from_internal(origin),
            10 * k as u64,
            seed,
            styles[k % 3],
        ));
    }
    let (lo, hi) = (Vec3::new(-15.0, 0.2, -10.0), Vec3::new(18.0, 3.0, 12.0));
    let mut rng = common::Rng(31);
    let point = |rng: &mut common::Rng| inches.from_internal(rng.point(lo, hi));
    for tick in 0..400_u64 {
        if tick % 5 == 0 {
            let (a, b) = (point(&mut rng), point(&mut rng));
            server.bullet(a, b, tick);
        }
        if tick % 61 == 0 {
            server.explosion(point(&mut rng), (tick % 2 == 0).then_some(90.0), tick);
        }
        server.advance(tick);
    }

    let join = 399;
    let sent: (Vec<CloudSpec>, Vec<Impact>) = (
        server
            .clouds()
            .filter(|c| c.end_tick() > join)
            .map(|c| *c.spec())
            .collect(),
        server.impacts().collect(),
    );
    let ((specs, impacts), _) = round_trip(&sent);
    let mut client = Smokes::new(config, &level);
    for spec in specs {
        client.pop(spec);
    }
    for impact in impacts {
        client.impact(impact);
    }
    let mut smoky = 0;
    for tick in join..join + 128 {
        server.advance(tick);
        client.advance(tick);
        if tick % 8 != 0 {
            continue;
        }
        for _ in 0..40 {
            let (a, b) = (point(&mut rng), point(&mut rng));
            let (s, c) = (server.occlusion(a, b, tick), client.occlusion(a, b, tick));
            assert_eq!(s.to_bits(), c.to_bits(), "tick {tick}: {a} to {b}");
            smoky += (s > 0.0) as u32;
        }
    }
    assert!(smoky > 50, "{smoky} lines through smoke");
}
