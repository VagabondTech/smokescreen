//! The smallest integration: a grenade (G), bullets (F), an explosion (H)
//! and a bot's line of sight, in a level made of boxes. The README's quick
//! start is a shorter version of this, with a mesh level instead.
//!
//! `cargo run --release --example minimal`

use std::sync::Arc;

use bevy::prelude::*;
use bevy_smokescreen::prelude::*;

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, SmokePlugin::default()))
        .add_systems(Startup, setup)
        .add_systems(Update, (throw_smoke, shoot, bot_vision))
        .run();
}

fn setup(mut commands: Commands) {
    // HDR is recommended; LDR cameras work with an approximate tonemap.
    commands.spawn((
        Camera3d::default(),
        bevy::camera::Hdr,
        // The performance dial; or `SmokeCamera::budget(1.5)` to fit 1.5 ms.
        SmokeCamera::HIGH,
        Transform::from_xyz(0.0, 1.7, -10.0).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight::default(),
        SmokeSun,
        Transform::from_xyz(-4.0, 10.0, -6.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // Static level geometry, voxelised once at load. `BoxObstacles` is fine for
    // blockouts; for real levels call `VoxelObstacles::build(min, max, 0.3, |center, half| ..)`
    // with your physics engine's box-overlap query inside the closure.
    let walls = BoxObstacles::new(vec![ObstacleBox::from_center_size(
        Vec3::new(0.0, 1.5, 6.0),
        Vec3::new(10.0, 3.0, 0.3),
    )]);
    let level = VoxelObstacles::from_level(Vec3::new(-50.0, -2.0, -50.0), Vec3::new(50.0, 12.0, 50.0), 0.3, &walls);
    commands.insert_resource(SmokeLevel(Arc::new(level)));
}

/// Call this where your grenade comes to rest.
fn throw_smoke(mut commands: Commands, keys: Res<ButtonInput<KeyCode>>) {
    if keys.just_pressed(KeyCode::KeyG) {
        commands.spawn((Smoke::default(), Transform::from_xyz(0.0, 0.0, 3.0)));
    }
}

/// Forward your hitscan traces and explosions.
fn shoot(
    keys: Res<ButtonInput<KeyCode>>,
    mut bullets: MessageWriter<SmokeBullet>,
    mut explosions: MessageWriter<SmokeExplosion>,
) {
    if keys.just_pressed(KeyCode::KeyF) {
        bullets.write(SmokeBullet {
            start: Vec3::new(0.0, 1.6, -10.0),
            end: Vec3::new(0.0, 1.0, 10.0),
        });
    }
    if keys.just_pressed(KeyCode::KeyH) {
        explosions.write(SmokeExplosion {
            center: Vec3::new(0.0, 0.5, 3.0),
            radius: None,
        });
    }
}

/// Gameplay sees the same smoke: bots, server-side visibility, audio. (A
/// server without Bevy asks `smokescreen::Smokes` the same question.)
fn bot_vision(mut vision: SmokeVision) {
    let bot_eye = Vec3::new(0.0, 1.6, -10.0);
    let player = Vec3::new(0.0, 1.6, 10.0);
    if vision.blocks_sight(bot_eye, player) {
        // Bot can't see the player.
    }
}
