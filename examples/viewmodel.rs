//! A first-person weapon that stays visible inside smoke.
//!
//! Smoke is drawn over everything the camera's main pass drew, including a
//! weapon held half a metre from the eye. The usual fix (as in Bevy's
//! `first_person_view_model` example) is a second camera that draws only the
//! weapon, after the world camera, with its own depth buffer and without a
//! `SmokeCamera`. Transparent objects in front of a smoke need nothing:
//! smoke is drawn before them. Behind or inside one they show through it
//! (see the README's limitations).
//!
//! ```text
//! cargo run --release --example viewmodel
//! SMOKE_CAPTURE=out cargo run --release --example viewmodel   # screenshot, then exit
//! ```

use std::time::Duration;

use bevy::{
    camera::{ClearColorConfig, visibility::RenderLayers},
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
    time::TimeUpdateStrategy,
};
use bevy_smokescreen::prelude::*;

/// Layer the weapon (and only the weapon camera) is on.
const VIEW_MODEL_LAYER: usize = 1;

#[derive(Resource)]
struct Capture(String);

fn main() {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            resolution: (960, 540).into(),
            ..default()
        }),
        ..default()
    }))
    .add_plugins(SmokePlugin::default())
    .insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.85)))
    .add_systems(Startup, setup);
    if let Ok(dir) = std::env::var("SMOKE_CAPTURE") {
        std::fs::create_dir_all(&dir).ok();
        app.insert_resource(Capture(dir))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(1.0 / 20.0)))
            .add_systems(Update, capture);
    }
    app.run();
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(60.0, 60.0))),
        MeshMaterial3d(materials.add(Color::srgb(0.6, 0.58, 0.54))),
        SmokeObstacle,
    ));
    commands.spawn((
        DirectionalLight {
            shadow_maps_enabled: true,
            ..default()
        },
        SmokeSun,
        Transform::from_xyz(-4.0, 10.0, -6.0).looking_at(Vec3::ZERO, Vec3::Y),
        // Light the world and the weapon.
        RenderLayers::from_layers(&[0, VIEW_MODEL_LAYER]),
    ));
    // The player stands at the edge of a smoke.
    commands.spawn((Smoke::default().with_seed(3), Transform::from_xyz(0.0, 0.0, 2.5)));

    commands
        .spawn((
            Transform::from_xyz(-1.0, 1.6, -3.5).looking_at(Vec3::new(0.0, 1.0, 4.0), Vec3::Y),
            Visibility::default(),
        ))
        .with_children(|player| {
            // The world camera: draws the level and the smoke.
            player.spawn((Camera3d::default(), bevy::camera::Hdr, SmokeCamera::HIGH));
            // The weapon camera: after the world camera, keeps its picture,
            // draws only the weapon layer, no smoke.
            player.spawn((
                Camera3d::default(),
                bevy::camera::Hdr,
                Camera {
                    order: 1,
                    clear_color: ClearColorConfig::None,
                    ..default()
                },
                RenderLayers::layer(VIEW_MODEL_LAYER),
            ));
            // A stand-in weapon.
            player.spawn((
                Mesh3d(meshes.add(Cuboid::new(0.08, 0.1, 0.6))),
                MeshMaterial3d(materials.add(Color::srgb(0.15, 0.15, 0.17))),
                Transform::from_xyz(0.22, -0.2, -0.45),
                RenderLayers::layer(VIEW_MODEL_LAYER),
            ));
        });
}

fn capture(mut commands: Commands, capture: Res<Capture>, mut frame: Local<u32>, mut exit: MessageWriter<AppExit>) {
    *frame += 1;
    if *frame == 100 {
        let name = format!("{}/viewmodel.png", capture.0);
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(name));
    }
    if *frame == 106 {
        exit.write(AppExit::Success);
    }
}
