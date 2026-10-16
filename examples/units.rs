//! The same scene in two unit systems: Y-up metres (Bevy's default) and Z-up
//! inches. Run both and compare the screenshots; they should match.
//!
//! ```text
//! SMOKE_UNITS=metres SMOKE_CAPTURE=out cargo run --example units
//! SMOKE_UNITS=inches SMOKE_CAPTURE=out cargo run --example units
//! ```
//!
//! `SMOKE_STYLE=faceted` or `crystal` draws that style. Without `SMOKE_CAPTURE` it just
//! shows the scene.

use std::{sync::Arc, time::Duration};

use bevy::{
    camera::Exposure,
    light::light_consts::lux,
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
    time::TimeUpdateStrategy,
};
use bevy_smokescreen::prelude::*;
use bevy_smokescreen::{Config, SmokeWorld};

#[derive(Resource, Clone, Copy)]
struct Setup {
    units: Units,
    style: SmokeStyle,
}

#[derive(Resource)]
struct Capture(String);

fn main() {
    let inches = std::env::var("SMOKE_UNITS").as_deref() == Ok("inches");
    let units = if inches { Units::INCHES_Z_UP } else { Units::METRES_Y_UP };
    let style = match std::env::var("SMOKE_STYLE").as_deref() {
        Ok("faceted") => SmokeStyle::Faceted,
        Ok("crystal") => SmokeStyle::Crystal,
        _ => SmokeStyle::Volumetric,
    };
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            resolution: (960, 540).into(),
            ..default()
        }),
        ..default()
    }))
    .add_plugins(SmokePlugin {
        config: Config::new(units, 128.0),
    })
    .insert_resource(SmokeSettings { style, ..default() })
    .insert_resource(Setup { units, style })
    .insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.75, 0.82, 1.0),
        brightness: 350.0,
        ..default()
    })
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

/// Boxes as (min, max) in metres, Y up.
const BOXES: [(Vec3, Vec3); 4] = [
    // A wall with a doorway.
    (Vec3::new(-8.0, 0.0, 5.8), Vec3::new(-1.0, 4.0, 6.2)),
    (Vec3::new(1.0, 0.0, 5.8), Vec3::new(8.0, 4.0, 6.2)),
    (Vec3::new(-1.0, 2.6, 5.8), Vec3::new(1.0, 4.0, 6.2)),
    // A crate.
    (Vec3::new(3.0, 0.0, -2.5), Vec3::new(4.2, 1.2, -1.3)),
];

fn setup(
    mut commands: Commands,
    setup: Res<Setup>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let u = setup.units;
    // Metres, Y up, to this world.
    let at = |p: Vec3| u.from_internal(p);
    let up = u.up_vector();
    let grey = materials.add(StandardMaterial {
        base_color: Color::srgb(0.62, 0.6, 0.56),
        perceptual_roughness: 0.9,
        ..default()
    });
    let wood = materials.add(StandardMaterial {
        base_color: Color::srgb(0.52, 0.38, 0.22),
        perceptual_roughness: 0.8,
        ..default()
    });

    // Ground: a thin box, so it needs no orientation of its own.
    let ground_min = at(Vec3::new(-40.0, -0.2, -40.0));
    let ground_max = at(Vec3::new(40.0, 0.0, 40.0));
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::from_size((ground_max - ground_min).abs()))),
        MeshMaterial3d(grey.clone()),
        Transform::from_translation((ground_min + ground_max) * 0.5),
    ));
    let mut obstacles = Vec::new();
    for (i, (min, max)) in BOXES.iter().enumerate() {
        let (a, b) = (at(*min), at(*max));
        let (lo, hi) = (a.min(b), a.max(b));
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::from_size(hi - lo))),
            MeshMaterial3d(if i == 3 { wood.clone() } else { grey.clone() }),
            Transform::from_translation((lo + hi) * 0.5),
        ));
        obstacles.push(ObstacleBox { min: lo, max: hi });
    }
    let level = BoxObstacles {
        boxes: obstacles,
        ground: Some(FlatGround::new(0.0, u.up)),
    };
    commands.insert_resource(SmokeLevel(Arc::new(level)));

    commands.spawn((
        DirectionalLight {
            illuminance: lux::OVERCAST_DAY * 3.0,
            shadow_maps_enabled: true,
            color: Color::srgb(1.0, 0.96, 0.9),
            ..default()
        },
        SmokeSun,
        Transform::from_translation(at(Vec3::new(-6.0, 12.0, -8.0))).looking_at(at(Vec3::ZERO), up),
    ));
    commands.spawn((
        Camera3d::default(),
        Exposure::default(),
        Msaa::Sample4,
        bevy::camera::Hdr,
        SmokeCamera::HIGH,
        Transform::from_translation(at(Vec3::new(-2.5, 1.7, -9.0))).looking_at(at(Vec3::new(0.0, 1.4, 5.0)), up),
    ));
    commands.spawn((
        Smoke::default().with_seed(7),
        Transform::from_translation(at(Vec3::new(0.0, 0.0, 4.6))),
    ));
}

fn capture(
    mut commands: Commands,
    capture: Res<Capture>,
    setup: Res<Setup>,
    mut frame: Local<u32>,
    world: Res<SmokeWorld>,
    mut exit: MessageWriter<AppExit>,
) {
    *frame += 1;
    // Software rasterisers need a while to compile pipelines; the smoke is
    // full after about 1.5 s (30 frames).
    if *frame == 100 {
        let name = format!(
            "{}/{}_{}.png",
            capture.0,
            if setup.units.up == UpAxis::Z {
                "inches"
            } else {
                "metres"
            },
            match setup.style {
                SmokeStyle::Volumetric => "volumetric",
                SmokeStyle::Faceted => "faceted",
                SmokeStyle::Crystal => "crystal",
            }
        );
        info!("{} clouds; saving {name}", world.clouds().count());
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(name));
    }
    if *frame == 106 {
        exit.write(AppExit::Success);
    }
}
