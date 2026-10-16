//! Every preset, and the three looks, side by side in a small yard whose
//! walls and floor are plain meshes marked [`SmokeObstacle`]: the plugin
//! voxelises them, so no collision code is needed.
//!
//! ```text
//! cargo run --release --example presets
//! SMOKE_CAPTURE=out cargo run --release --example presets   # screenshot, then exit
//! ```
//!
//! G re-throws everything.

use std::time::Duration;

use bevy::{
    camera::Exposure,
    light::light_consts::lux,
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
    time::TimeUpdateStrategy,
};
use bevy_smokescreen::prelude::*;

#[derive(Resource)]
struct Capture(String);

fn main() {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            resolution: (1280, 720).into(),
            ..default()
        }),
        ..default()
    }))
    .add_plugins(SmokePlugin::default())
    .insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.75, 0.82, 1.0),
        brightness: 400.0,
        ..default()
    })
    .insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.85)))
    .add_systems(Startup, (setup, throw))
    .add_systems(Update, rethrow);
    if let Ok(dir) = std::env::var("SMOKE_CAPTURE") {
        std::fs::create_dir_all(&dir).ok();
        app.insert_resource(Capture(dir))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(1.0 / 20.0)))
            .add_systems(Update, capture);
    }
    app.run();
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let ground = materials.add(StandardMaterial {
        base_color: Color::srgb(0.6, 0.58, 0.54),
        perceptual_roughness: 0.9,
        ..default()
    });
    let wall = materials.add(StandardMaterial {
        base_color: Color::srgb(0.72, 0.66, 0.58),
        perceptual_roughness: 0.85,
        ..default()
    });
    // The floor and walls are what the smoke flows around.
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(90.0, 90.0))),
        MeshMaterial3d(ground),
        SmokeObstacle,
    ));
    // A three-sided room at the back for the gas, and a low wall in front.
    let walls = [
        (Vec3::new(14.0, 2.0, 22.0), Vec3::new(12.0, 4.0, 0.4)),
        (Vec3::new(8.2, 2.0, 17.0), Vec3::new(0.4, 4.0, 10.0)),
        (Vec3::new(19.8, 2.0, 17.0), Vec3::new(0.4, 4.0, 10.0)),
        (Vec3::new(-14.0, 0.6, 4.0), Vec3::new(10.0, 1.2, 0.4)),
    ];
    for (center, size) in walls {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::from_size(size))),
            MeshMaterial3d(wall.clone()),
            Transform::from_translation(center),
            SmokeObstacle,
        ));
    }

    commands.spawn((
        DirectionalLight {
            illuminance: lux::OVERCAST_DAY * 3.0,
            shadow_maps_enabled: true,
            color: Color::srgb(1.0, 0.96, 0.9),
            ..default()
        },
        SmokeSun,
        Transform::from_xyz(-8.0, 14.0, -10.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Camera3d::default(),
        Exposure::default(),
        bevy::camera::Hdr,
        SmokeCamera::HIGH,
        Transform::from_xyz(0.0, 20.0, -32.0).looking_at(Vec3::new(0.0, 0.0, 9.0), Vec3::Y),
    ));
}

/// The presets, and a grenade in each look.
fn throw(mut commands: Commands) {
    let row = [
        // Left to right as the camera sees them: volumetric, faceted, crystal.
        (Smoke::grenade(), Vec3::new(16.0, 0.0, -4.0)),
        (
            Smoke::grenade().with_style(SmokeStyle::Faceted),
            Vec3::new(0.0, 0.0, -4.0),
        ),
        (
            Smoke::grenade().with_style(SmokeStyle::Crystal),
            Vec3::new(-16.0, 0.0, -4.0),
        ),
        // A wall of smoke along the low wall.
        (Smoke::wall(Vec3::new(12.0, 0.0, 0.0)), Vec3::new(-20.0, 0.3, 6.5)),
        // Gas in the room: it stays inside.
        (Smoke::gas(), Vec3::new(14.0, 0.0, 18.0)),
        (Smoke::fog_bank(Vec3::new(5.0, 1.2, 3.0)), Vec3::new(-14.0, 1.2, 17.0)),
        (Smoke::steam_vent(), Vec3::new(0.0, 0.0, 14.0)),
    ];
    for (i, (smoke, at)) in row.into_iter().enumerate() {
        commands.spawn((smoke.with_seed(i as u32 + 1), Transform::from_translation(at)));
    }
}

fn rethrow(mut commands: Commands, keys: Res<ButtonInput<KeyCode>>, smokes: Query<Entity, With<Smoke>>) {
    if keys.just_pressed(KeyCode::KeyG) {
        for e in &smokes {
            commands.entity(e).despawn();
        }
        commands.run_system_cached(throw);
    }
}

fn capture(mut commands: Commands, capture: Res<Capture>, mut frame: Local<u32>, mut exit: MessageWriter<AppExit>) {
    *frame += 1;
    // Software rasterisers need a while to compile pipelines; by frame 140
    // (7 s of simulated time) everything has grown.
    if *frame == 140 {
        let name = format!("{}/presets.png", capture.0);
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(name));
    }
    if *frame == 146 {
        exit.write(AppExit::Success);
    }
}
