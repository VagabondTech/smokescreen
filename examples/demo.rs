//! Interactive test range for `bevy_smokescreen`.
//!
//! `cargo run --release --example demo`. In a browser with WebGPU: see
//! `web/README.md`.
//!
//! Controls
//! - Hold right mouse: look around. WASD: move, Space/Ctrl: up/down, Shift: fast
//! - Left mouse: shoot (bullets carve tunnels through smoke)
//! - G: throw a smoke where you're aiming
//! - H: throw an HE grenade where you're aiming (clears a sphere that refills)
//! - F: cycle the style: volumetric, faceted, crystal (re-throws current smokes)
//! - 1-6: the quality dial, cheapest to best
//! - B: budget mode (the dial turns itself to fit a GPU budget), [ and ]: budget -/+
//! - M: toggle MSAA, T: toggle TAA (enables temporal jitter on the smoke)
//! - F1: toggle this help
//!
//! Environment variables
//! - `SMOKE_DEMO_STYLE=faceted` or `crystal` starts in that style.
//! - `SMOKE_DEMO_BUDGET_MS=<ms>` sets the budget for budget mode (default 2).
//! - `SMOKE_DEMO_GPU_TIMINGS=1` / `0` turns GPU timestamp diagnostics on or
//!   off (HUD timings, budget mode, the tier sweep). Default: on, except on
//!   macOS, where they have lost the Metal device.
//! - `SMOKE_DEMO_CAPTURE=<dir>` runs a scripted sequence that saves screenshots
//!   (and measurements) into `<dir>` and exits. `SMOKE_DEMO_SCRIPT` picks it:
//!   `showcase` (default; `SMOKE_DEMO_HUD=1` keeps the HUD in the shots),
//!   `tiers` (cost of every dial step, plus silhouette masks for the fairness
//!   check), `budget` (budget mode while a smoke comes close and goes away
//!   again) or `agreement` (faceted or crystal silhouette masks next to the
//!   CPU query for every pixel; compare with `tools/agreement.py`).

use std::{fmt::Write as _, sync::Arc, time::Duration};

use bevy::{
    anti_alias::taa::TemporalAntiAliasing,
    camera::Exposure,
    diagnostic::{DiagnosticPath, DiagnosticsStore, FrameTimeDiagnosticsPlugin},
    input::mouse::AccumulatedMouseMotion,
    light::{GlobalAmbientLight, light_consts::lux},
    platform::time::Instant,
    prelude::*,
    render::{
        diagnostic::RenderDiagnosticsPlugin,
        view::screenshot::{Screenshot, save_to_disk},
    },
    time::TimeUpdateStrategy,
};
use bevy_smokescreen::{prelude::*, quality};

fn main() {
    let capture = std::env::var("SMOKE_DEMO_CAPTURE").ok();
    let script = std::env::var("SMOKE_DEMO_SCRIPT").unwrap_or_else(|_| "showcase".into());
    let resolution = if capture.is_some() { (960, 540) } else { (1600, 900) };
    let style = match std::env::var("SMOKE_DEMO_STYLE").as_deref() {
        Ok("faceted") => SmokeStyle::Faceted,
        Ok("crystal") => SmokeStyle::Crystal,
        _ => SmokeStyle::Volumetric,
    };
    let budget_ms = std::env::var("SMOKE_DEMO_BUDGET_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2.0);

    // GPU timings (HUD, budget mode, tier sweep) use timestamp queries, which
    // lost the device on one Metal setup and aren't in every browser; off by
    // default on macOS and the web.
    let gpu_timings = std::env::var("SMOKE_DEMO_GPU_TIMINGS")
        .map(|v| v != "0")
        .unwrap_or(!cfg!(any(target_os = "macos", target_arch = "wasm32")));

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "Smokescreen demo".into(),
            resolution: resolution.into(),
            // On the web (see web/index.html): draw into the page's canvas,
            // sized by the page.
            canvas: cfg!(target_arch = "wasm32").then(|| "#bevy".into()),
            fit_canvas_to_parent: true,
            ..default()
        }),
        ..default()
    }))
    .add_plugins((SmokePlugin::default(), FrameTimeDiagnosticsPlugin::default()))
    .insert_resource(SmokeSettings { style, ..default() })
    .insert_resource(DemoBudget(budget_ms))
    .insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.75, 0.82, 1.0),
        brightness: 350.0,
        ..default()
    })
    .insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.85)))
    .init_resource::<Effects>()
    .add_systems(Startup, (setup_level, setup_camera_and_hud))
    .add_systems(
        Update,
        (fly_camera, weapons, quality_keys, style_key, update_effects, update_hud),
    );

    if gpu_timings {
        app.add_plugins(RenderDiagnosticsPlugin);
    }

    if let Some(dir) = capture {
        std::fs::create_dir_all(&dir).ok();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(1.0 / 20.0)));
        match script.as_str() {
            "tiers" => {
                app.insert_resource(Sweep::new(dir))
                    .add_systems(Update, tier_sweep.before(weapons));
            }
            "budget" => {
                app.insert_resource(BudgetRun::new(dir))
                    .add_systems(Update, budget_run.before(weapons));
            }
            "agreement" => {
                app.insert_resource(AgreementRun::new(dir))
                    .add_systems(Update, agreement_run.before(weapons));
            }
            _ => {
                app.insert_resource(Capture {
                    dir,
                    step: 0,
                    pending: Actions::default(),
                })
                .add_systems(Update, capture_script.before(weapons));
            }
        }
    } else {
        app.add_systems(Update, opening_smoke);
    }
    app.run();
}

/// Budget used when budget mode is switched on.
#[derive(Resource)]
struct DemoBudget(f32);

#[derive(Resource, Clone)]
struct Level(Arc<BoxObstacles>);

impl Level {
    fn raycast(&self, origin: Vec3, dir: Vec3, max: f32) -> Option<f32> {
        self.0.raycast(origin, dir, max)
    }
}

fn setup_level(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let concrete = materials.add(StandardMaterial {
        base_color: Color::srgb(0.62, 0.6, 0.56),
        perceptual_roughness: 0.9,
        ..default()
    });
    let plaster = materials.add(StandardMaterial {
        base_color: Color::srgb(0.78, 0.72, 0.62),
        perceptual_roughness: 0.85,
        ..default()
    });
    let crate_wood = materials.add(StandardMaterial {
        base_color: Color::srgb(0.52, 0.38, 0.22),
        perceptual_roughness: 0.8,
        ..default()
    });
    let red = materials.add(StandardMaterial {
        base_color: Color::srgb(0.7, 0.12, 0.1),
        ..default()
    });

    // Ground.
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(90.0, 90.0))),
        MeshMaterial3d(concrete.clone()),
    ));

    // (centre, size, material) for every solid box. The same list drives the
    // meshes and the smoke collision, so they always agree.
    let mut boxes: Vec<(Vec3, Vec3, Handle<StandardMaterial>)> = Vec::new();
    let mut wall = |min: Vec3, max: Vec3, mat: &Handle<StandardMaterial>| {
        boxes.push(((min + max) * 0.5, max - min, mat.clone()));
    };
    let h = 4.0;
    // Front wall of a building at z = 6, with a 2 m doorway (lintel above).
    wall(Vec3::new(-9.0, 0.0, 5.8), Vec3::new(-1.0, h, 6.2), &plaster);
    wall(Vec3::new(1.0, 0.0, 5.8), Vec3::new(9.0, h, 6.2), &plaster);
    wall(Vec3::new(-1.0, 2.6, 5.8), Vec3::new(1.0, h, 6.2), &plaster);
    // Side walls, back wall and roof: an indoor room the sun can't reach.
    wall(Vec3::new(-9.0, 0.0, 6.2), Vec3::new(-8.6, h, 15.0), &plaster);
    wall(Vec3::new(8.6, 0.0, 6.2), Vec3::new(9.0, h, 15.0), &plaster);
    wall(Vec3::new(-9.0, 0.0, 14.6), Vec3::new(9.0, h, 15.0), &plaster);
    wall(Vec3::new(-9.0, h, 5.8), Vec3::new(9.0, h + 0.3, 15.0), &concrete);
    // A 2.4 m corridor to the right, open at both ends.
    wall(Vec3::new(11.0, 0.0, -10.0), Vec3::new(11.4, 3.2, 4.0), &concrete);
    wall(Vec3::new(13.8, 0.0, -10.0), Vec3::new(14.2, 3.2, 4.0), &concrete);
    // Cover: crates and a pillar in the open.
    wall(Vec3::new(-4.6, 0.0, 0.4), Vec3::new(-3.4, 1.2, 1.6), &crate_wood);
    wall(Vec3::new(-4.4, 1.2, 0.6), Vec3::new(-3.6, 2.0, 1.4), &crate_wood);
    wall(Vec3::new(3.0, 0.0, -2.5), Vec3::new(4.2, 1.2, -1.3), &crate_wood);
    wall(Vec3::new(5.5, 0.0, 2.0), Vec3::new(6.3, 3.5, 2.8), &concrete);
    // Something bright to hide behind the smoke.
    wall(Vec3::new(-0.4, 0.0, 9.6), Vec3::new(0.4, 1.8, 10.4), &red);

    let mut obstacle_boxes = Vec::new();
    for (center, size, mat) in boxes {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::from_size(size))),
            MeshMaterial3d(mat),
            Transform::from_translation(center),
        ));
        obstacle_boxes.push(ObstacleBox::from_center_size(center, size));
    }
    let level = Arc::new(BoxObstacles::new(obstacle_boxes));

    // Pre-voxelise once, the way a real game would at map load.
    let voxels = VoxelObstacles::from_level(
        Vec3::new(-30.0, -1.0, -30.0),
        Vec3::new(30.0, 8.0, 30.0),
        SmokePlugin::default().config.tuning.voxel_size,
        level.as_ref(),
    );
    commands.insert_resource(SmokeLevel(Arc::new(voxels)));
    commands.insert_resource(Level(level));

    commands.spawn((
        DirectionalLight {
            illuminance: lux::OVERCAST_DAY * 3.0,
            shadow_maps_enabled: true,
            color: Color::srgb(1.0, 0.96, 0.9),
            ..default()
        },
        SmokeSun,
        Transform::from_xyz(-6.0, 12.0, -8.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

#[derive(Component)]
struct FlyCam {
    yaw: f32,
    pitch: f32,
}

#[derive(Component)]
struct Hud;

#[derive(Component)]
struct Help;

fn setup_camera_and_hud(mut commands: Commands) {
    let eye = Vec3::new(-2.5, 1.7, -9.0);
    let look = Transform::from_translation(eye).looking_at(Vec3::new(0.0, 1.4, 5.0), Vec3::Y);
    let (yaw, pitch, _) = look.rotation.to_euler(EulerRot::YXZ);
    let mut camera = commands.spawn((
        Camera3d::default(),
        Exposure::default(),
        Msaa::Sample4,
        SmokeCamera::HIGH,
        look,
        FlyCam { yaw, pitch },
    ));
    // SMOKE_DEMO_LDR=1 exercises the non-HDR composite path.
    if std::env::var("SMOKE_DEMO_LDR").is_err() {
        camera.insert(bevy::camera::Hdr);
    }

    // Crosshair.
    commands
        .spawn(Node {
            width: percent(100),
            height: percent(100),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(|p| {
            p.spawn((
                Node {
                    width: px(4),
                    height: px(4),
                    ..default()
                },
                BackgroundColor(Color::WHITE),
            ));
        });

    commands.spawn((
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(15.0),
            ..default()
        },
        TextShadow::default(),
        Node {
            position_type: PositionType::Absolute,
            top: px(10),
            left: px(12),
            ..default()
        },
        Hud,
    ));
    commands.spawn((
        Text::new(
            "RMB look  WASD move  Space/Ctrl up/down  Shift fast\n\
             LMB shoot  G smoke  H HE grenade  F style\n\
             1-6 quality dial  B budget mode  [ ] budget -/+  M MSAA  T TAA  F1 hide help",
        ),
        TextFont {
            font_size: FontSize::Px(14.0),
            ..default()
        },
        TextShadow::default(),
        Node {
            position_type: PositionType::Absolute,
            bottom: px(10),
            left: px(12),
            ..default()
        },
        Help,
    ));
}

fn opening_smoke(mut commands: Commands, time: Res<Time>, mut done: Local<bool>) {
    if !*done && time.elapsed_secs() > 0.5 {
        *done = true;
        commands.spawn((Smoke::default(), Transform::from_xyz(0.0, 0.0, 4.6)));
    }
}

fn fly_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut cameras: Query<(&mut Transform, &mut FlyCam)>,
) {
    let Ok((mut transform, mut cam)) = cameras.single_mut() else {
        return;
    };
    if buttons.pressed(MouseButton::Right) {
        cam.yaw -= motion.delta.x * 0.003;
        cam.pitch = (cam.pitch - motion.delta.y * 0.003).clamp(-1.5, 1.5);
    }
    transform.rotation = Quat::from_euler(EulerRot::YXZ, cam.yaw, cam.pitch, 0.0);
    let mut dir = Vec3::ZERO;
    let forward = transform.forward().as_vec3();
    let right = transform.right().as_vec3();
    if keys.pressed(KeyCode::KeyW) {
        dir += forward;
    }
    if keys.pressed(KeyCode::KeyS) {
        dir -= forward;
    }
    if keys.pressed(KeyCode::KeyD) {
        dir += right;
    }
    if keys.pressed(KeyCode::KeyA) {
        dir -= right;
    }
    if keys.pressed(KeyCode::Space) {
        dir += Vec3::Y;
    }
    if keys.pressed(KeyCode::ControlLeft) {
        dir -= Vec3::Y;
    }
    let speed = if keys.pressed(KeyCode::ShiftLeft) { 12.0 } else { 4.0 };
    transform.translation += dir.normalize_or_zero() * speed * time.delta_secs();
}

/// Short-lived visual effects: tracers and flash lights.
#[derive(Resource, Default)]
struct Effects {
    tracers: Vec<(Vec3, Vec3, f32)>,
}

#[derive(Component)]
struct Flash {
    age: f32,
    life: f32,
    peak: f32,
}

/// Inputs that the capture script can also trigger.
#[derive(Default)]
struct Actions {
    shoot: Vec<Vec3>,
    smoke: bool,
    explode: bool,
    explode_at: Option<Vec3>,
}

#[allow(clippy::too_many_arguments)]
fn weapons(
    mut commands: Commands,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    level: Res<Level>,
    mut bullets: MessageWriter<SmokeBullet>,
    mut explosions: MessageWriter<SmokeExplosion>,
    mut effects: ResMut<Effects>,
    capture: Option<ResMut<Capture>>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let origin = cam.translation();
    let forward = cam.forward().as_vec3();
    let mut actions = capture.map(|mut c| std::mem::take(&mut c.pending)).unwrap_or_default();
    if buttons.just_pressed(MouseButton::Left) {
        actions.shoot.push(forward);
    }
    actions.smoke |= keys.just_pressed(KeyCode::KeyG);
    actions.explode |= keys.just_pressed(KeyCode::KeyH);

    for dir in actions.shoot {
        let dir = dir.normalize();
        let dist = level.raycast(origin, dir, 120.0).unwrap_or(120.0);
        let end = origin + dir * dist;
        bullets.write(SmokeBullet { start: origin, end });
        effects.tracers.push((origin + dir * 0.6 - Vec3::Y * 0.1, end, 0.0));
        // Muzzle flash: lights smoke you're standing in.
        commands.spawn((
            PointLight {
                intensity: 0.0,
                range: 6.0,
                color: Color::srgb(1.0, 0.75, 0.4),
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::from_translation(origin + dir * 0.8),
            Flash {
                age: 0.0,
                life: 0.06,
                peak: 60_000.0,
            },
        ));
    }

    let target = || {
        let dist = level.raycast(origin, forward, 60.0).unwrap_or(30.0);
        let hit = origin + forward * (dist - 0.3).max(0.5);
        // Drop to the floor below the hit point.
        let floor = level.raycast(hit, Vec3::NEG_Y, 20.0).unwrap_or(hit.y);
        Vec3::new(hit.x, hit.y - floor, hit.z)
    };
    if actions.smoke {
        commands.spawn((Smoke::default(), Transform::from_translation(target())));
    }
    if actions.explode || actions.explode_at.is_some() {
        let p = actions.explode_at.unwrap_or_else(|| target() + Vec3::Y * 0.4);
        explosions.write(SmokeExplosion {
            center: p,
            radius: None,
        });
        commands.spawn((
            PointLight {
                intensity: 0.0,
                range: 18.0,
                color: Color::srgb(1.0, 0.6, 0.25),
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::from_translation(p + Vec3::Y * 0.5),
            Flash {
                age: 0.0,
                life: 0.45,
                peak: 4_000_000.0,
            },
        ));
    }
}

fn update_effects(
    mut commands: Commands,
    time: Res<Time>,
    mut effects: ResMut<Effects>,
    mut flashes: Query<(Entity, &mut Flash, &mut PointLight)>,
    mut gizmos: Gizmos,
) {
    let dt = time.delta_secs();
    effects.tracers.retain_mut(|(a, b, age)| {
        *age += dt;
        let alpha = 1.0 - *age / 0.12;
        if alpha > 0.0 {
            gizmos.line(*a, *b, Color::srgba(1.0, 0.85, 0.5, alpha));
        }
        alpha > 0.0
    });
    for (entity, mut flash, mut light) in &mut flashes {
        flash.age += dt;
        let k = 1.0 - flash.age / flash.life;
        if k <= 0.0 {
            commands.entity(entity).despawn();
        } else {
            light.intensity = flash.peak * k * k;
        }
    }
}

fn quality_keys(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut cameras: Query<(Entity, &mut SmokeCamera, &mut Msaa, Has<TemporalAntiAliasing>)>,
    mut help: Query<&mut Visibility, With<Help>>,
    mut budget: ResMut<DemoBudget>,
) {
    let Ok((entity, mut smoke, mut msaa, has_taa)) = cameras.single_mut() else {
        return;
    };
    let digits = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
    ];
    for (tier, key) in digits.iter().enumerate() {
        if keys.just_pressed(*key) {
            smoke.quality = quality::quality_of_tier(tier);
        }
    }
    if keys.just_pressed(KeyCode::KeyB) {
        smoke.budget_ms = match smoke.budget_ms {
            Some(_) => None,
            None => Some(budget.0),
        };
    }
    for (key, factor) in [(KeyCode::BracketLeft, 0.8), (KeyCode::BracketRight, 1.25)] {
        if keys.just_pressed(key) {
            budget.0 = (budget.0 * factor).clamp(0.05, 100.0);
            if smoke.budget_ms.is_some() {
                smoke.budget_ms = Some(budget.0);
            }
        }
    }
    if keys.just_pressed(KeyCode::KeyM) && !has_taa {
        *msaa = if *msaa == Msaa::Off { Msaa::Sample4 } else { Msaa::Off };
    }
    if keys.just_pressed(KeyCode::KeyT) {
        if has_taa {
            commands.entity(entity).remove::<TemporalAntiAliasing>();
            smoke.temporal_jitter = false;
        } else {
            // TAA needs MSAA off; it then averages the smoke's per-frame jitter.
            *msaa = Msaa::Off;
            commands.entity(entity).insert(TemporalAntiAliasing::default());
            smoke.temporal_jitter = true;
        }
    }
    if keys.just_pressed(KeyCode::F1) {
        for mut v in &mut help {
            v.toggle_visible_hidden();
        }
    }
}

/// F: cycle the style (volumetric, faceted, crystal) and re-throw the
/// current smokes in it.
fn style_key(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<SmokeSettings>,
    smokes: Query<(Entity, &Smoke, &Transform)>,
) {
    if !keys.just_pressed(KeyCode::KeyF) {
        return;
    }
    settings.style = match settings.style {
        SmokeStyle::Volumetric => SmokeStyle::Faceted,
        SmokeStyle::Faceted => SmokeStyle::Crystal,
        SmokeStyle::Crystal => SmokeStyle::Volumetric,
    };
    for (entity, smoke, transform) in &smokes {
        commands.entity(entity).despawn();
        // A clone pops a fresh cloud, here in the new default style.
        let mut fresh = smoke.clone();
        fresh.style = None;
        commands.spawn((fresh, *transform));
    }
}

/// Smoothed GPU time of a pass, if it ran recently.
fn gpu_ms(diagnostics: &DiagnosticsStore, pass: &str) -> Option<f64> {
    let path = DiagnosticPath::new(format!("render/{pass}/elapsed_gpu"));
    let d = diagnostics.get(&path)?;
    let recent = d.measurement()?.time.elapsed().as_secs_f32() < 2.0;
    recent.then(|| d.smoothed()).flatten()
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_hud(
    mut hud: Query<&mut Text, With<Hud>>,
    diagnostics: Res<DiagnosticsStore>,
    smokes: Query<&Smoke>,
    settings: Res<SmokeSettings>,
    cameras: Query<(
        &SmokeCamera,
        &SmokeQuality,
        &Msaa,
        Has<TemporalAntiAliasing>,
        &GlobalTransform,
    )>,
    mut vision: SmokeVision,
    time: Res<Time<Real>>,
    mut timer: Local<f32>,
) {
    *timer -= time.delta_secs();
    if *timer > 0.0 {
        return;
    }
    *timer = 0.25;
    let (Ok(mut text), Ok((smoke_cam, status, msaa, taa, cam))) = (hud.single_mut(), cameras.single()) else {
        return;
    };
    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);
    let mut gpu = String::new();
    for pass in std::iter::once("smoke_bake").chain(CAMERA_PASSES) {
        if let Some(ms) = gpu_ms(&diagnostics, pass) {
            let _ = write!(gpu, "  {}: {:.3} ms", &pass[6..], ms);
        }
    }
    if gpu.is_empty() {
        gpu = "  (no GPU timings: SMOKE_DEMO_GPU_TIMINGS=1 turns them on, if the adapter has timestamps)".into();
    }
    let dial = match smoke_cam.budget_ms {
        Some(b) => format!(
            "budget {b:.2} ms -> {} (predicted {:.2} ms)",
            status.tier_name(),
            status.predicted_ms
        ),
        None => format!("dial {} ({}/6)", status.tier_name(), status.tier + 1),
    };
    // A gameplay query: can the camera see the red box behind the smoke?
    let target = Vec3::new(0.0, 0.9, 10.0);
    let seen = vision.transmittance(cam.translation(), target);
    let style = match settings.style {
        SmokeStyle::Volumetric => "volumetric",
        SmokeStyle::Faceted => "faceted",
        SmokeStyle::Crystal => "crystal",
    };
    text.0 = format!(
        "FPS {fps:.0}   smokes {}   style {style}   {dial}   MSAA {}   TAA {}\nGPU{gpu}\nRed box visibility (CPU query): {:.0}%",
        smokes.iter().filter(|s| s.cloud().is_some()).count(),
        msaa.samples(),
        if taa { "on" } else { "off" },
        seen * 100.0,
    );
}

/// Has any smoke been growing for longer than `seconds`?
fn grown(world: &SmokeWorld, clock: &SmokeClock, seconds: f32) -> bool {
    let rate = world.config().tick_rate;
    world.clouds().any(|c| {
        c.age(clock.tick, rate)
            .is_some_and(|a| a > c.timeline().start + seconds)
    })
}

fn place(eye: Vec3, at: Vec3, transform: &mut Transform, cam: &mut FlyCam) {
    *transform = Transform::from_translation(eye).looking_at(at, Vec3::Y);
    let (yaw, pitch, _) = transform.rotation.to_euler(EulerRot::YXZ);
    cam.yaw = yaw;
    cam.pitch = pitch;
}

fn screenshot(commands: &mut Commands, dir: &str, name: &str) {
    let path = format!("{dir}/{name}.png");
    commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
}

// ---------------------------------------------------------------------------
// Scripted capture for automated visual checks.
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct Capture {
    dir: String,
    step: usize,
    pending: Actions,
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn capture_script(
    mut commands: Commands,
    time: Res<Time>,
    mut capture: ResMut<Capture>,
    mut camera: Query<(Entity, &mut Transform, &mut FlyCam, &mut SmokeCamera, &mut Msaa)>,
    mut ui: Query<&mut Visibility, Or<(With<Hud>, With<Help>, With<Node>)>>,
    mut exit: MessageWriter<AppExit>,
) {
    if std::env::var("SMOKE_DEMO_HUD").is_err() {
        for mut v in &mut ui {
            *v = Visibility::Hidden;
        }
    }
    let t = time.elapsed_secs();
    let Ok((entity, mut transform, mut cam, mut smoke_cam, mut msaa)) = camera.single_mut() else {
        return;
    };
    let front = (Vec3::new(-2.5, 1.7, -9.0), Vec3::new(0.0, 1.4, 5.0));
    let side = (Vec3::new(-11.0, 3.0, -1.0), Vec3::new(2.0, 1.0, 5.0));
    let corridor = (Vec3::new(12.6, 1.7, -12.5), Vec3::new(12.6, 1.2, 0.0));

    struct Step {
        at: f32,
        name: &'static str,
    }
    // Software rasterisers need a couple of seconds to compile pipelines, so
    // the script starts after a warm-up.
    let w = 2.0;
    let steps = [
        Step { at: w, name: "spawn" },
        Step {
            at: w + 0.5,
            name: "01_growing",
        },
        Step {
            at: w + 3.0,
            name: "02_full",
        },
        Step {
            at: w + 3.05,
            name: "shoot",
        },
        Step {
            at: w + 3.3,
            name: "03_bullets",
        },
        Step {
            at: w + 4.0,
            name: "explode",
        },
        Step {
            at: w + 4.1,
            name: "04_explosion_flash",
        },
        Step {
            at: w + 4.6,
            name: "05_explosion_hole",
        },
        Step {
            at: w + 7.0,
            name: "06_refilling",
        },
        Step {
            at: w + 7.1,
            name: "side",
        },
        Step {
            at: w + 7.5,
            name: "07_side_view",
        },
        Step {
            at: w + 7.6,
            name: "corridor",
        },
        Step {
            at: w + 8.0,
            name: "08_corridor",
        },
        Step {
            at: w + 8.1,
            name: "inside",
        },
        Step {
            at: w + 8.5,
            name: "09_inside_smoke",
        },
        Step {
            at: w + 8.6,
            name: "room",
        },
        Step {
            at: w + 9.0,
            name: "10_from_the_room",
        },
        Step {
            at: w + 9.1,
            name: "low_quality",
        },
        Step {
            at: w + 9.5,
            name: "11_low_quality_msaa_off",
        },
        Step {
            at: w + 9.6,
            name: "restore",
        },
        Step {
            at: w + 9.7,
            name: "overlap",
        },
        Step {
            at: w + 11.0,
            name: "12_overlapping_smokes",
        },
        Step {
            at: w + 11.1,
            name: "taa",
        },
        Step {
            at: w + 12.0,
            name: "13_taa_temporal_jitter",
        },
        Step {
            at: w + 12.1,
            name: "taa_off",
        },
        Step {
            at: w + 16.8,
            name: "14_dissolving",
        },
        Step {
            at: w + 17.4,
            name: "exit",
        },
    ];
    while capture.step < steps.len() && t >= steps[capture.step].at {
        let name = steps[capture.step].name;
        capture.step += 1;
        match name {
            "spawn" => {
                commands.spawn((Smoke::default(), Transform::from_xyz(0.0, 0.0, 4.6)));
                commands.spawn((Smoke::default().with_seed(3), Transform::from_xyz(12.6, 0.0, -2.0)));
                place(front.0, front.1, &mut transform, &mut cam);
            }
            "shoot" => {
                let eye = transform.translation;
                for (dx, dy) in [(-1.5, 0.3), (-0.5, -0.2), (0.4, 0.6), (1.2, 0.0), (0.0, 1.4)] {
                    let aim = Vec3::new(dx, 1.2 + dy, 5.0);
                    capture.pending.shoot.push(aim - eye);
                }
            }
            "explode" => {
                capture.pending.explode_at = Some(Vec3::new(-0.6, 0.6, 3.6));
            }
            "side" => place(side.0, side.1, &mut transform, &mut cam),
            "corridor" => place(corridor.0, corridor.1, &mut transform, &mut cam),
            "inside" => place(
                Vec3::new(0.6, 1.5, 3.4),
                Vec3::new(0.0, 1.4, 8.0),
                &mut transform,
                &mut cam,
            ),
            "room" => place(
                Vec3::new(-5.5, 1.7, 12.5),
                Vec3::new(0.0, 1.0, 6.0),
                &mut transform,
                &mut cam,
            ),
            "low_quality" => {
                place(front.0, front.1, &mut transform, &mut cam);
                *smoke_cam = SmokeCamera::LOW;
                *msaa = Msaa::Off;
            }
            "restore" => {
                *smoke_cam = SmokeCamera::HIGH;
                *msaa = Msaa::Sample4;
            }
            "overlap" => {
                // A second, short-lived smoke overlapping the first one.
                commands.spawn((
                    Smoke::default().with_lifetime(6.0).with_seed(11),
                    Transform::from_xyz(3.2, 0.0, 2.6),
                ));
            }
            "taa" => {
                *msaa = Msaa::Off;
                smoke_cam.temporal_jitter = true;
                commands.entity(entity).insert(TemporalAntiAliasing::default());
            }
            "taa_off" => {
                smoke_cam.temporal_jitter = false;
                commands.entity(entity).remove::<TemporalAntiAliasing>();
                *msaa = Msaa::Sample4;
            }
            "exit" => {
                exit.write(AppExit::Success);
            }
            shot => screenshot(&mut commands, &capture.dir, shot),
        }
    }
}

// ---------------------------------------------------------------------------
// Tier sweep: what every step of the dial costs, and silhouette masks for the
// fairness check (a lower step must never show less smoke).
// ---------------------------------------------------------------------------

/// GPU passes that belong to a camera, both styles.
const CAMERA_PASSES: [&str; 7] = [
    quality::VOLUMETRIC_PASSES[0],
    quality::VOLUMETRIC_PASSES[1],
    quality::FACET_PASSES[0],
    quality::FACET_PASSES[1],
    quality::FACET_PASSES[2],
    quality::FACET_PASSES[3],
    quality::FACET_PASSES[4],
];

/// Collects fresh GPU timings of the camera passes.
#[derive(Default)]
struct Timings {
    last_seen: [Option<Instant>; CAMERA_PASSES.len()],
    /// Per pass, the newest timing (ms) of the last fresh frame.
    passes: [f64; CAMERA_PASSES.len()],
}

impl Timings {
    /// Sum of the camera passes' newest timings, if any arrived since last call.
    fn fresh(&mut self, diagnostics: &DiagnosticsStore) -> Option<f64> {
        let mut sum = None;
        for (i, pass) in CAMERA_PASSES.iter().enumerate() {
            let path = DiagnosticPath::new(format!("render/{pass}/elapsed_gpu"));
            let Some(m) = diagnostics.get(&path).and_then(|d| d.measurement()) else {
                continue;
            };
            if self.last_seen[i].is_some_and(|t| t >= m.time) {
                continue;
            }
            self.last_seen[i] = Some(m.time);
            self.passes[i] = m.value;
            *sum.get_or_insert(0.0) += m.value;
        }
        sum
    }
}

#[derive(Resource)]
struct Sweep {
    dir: String,
    frame: u32,
    stage: usize,
    stage_frame: u32,
    /// Total and per-pass milliseconds of each measured frame.
    samples: Vec<(f64, [f64; CAMERA_PASSES.len()])>,
    timings: Timings,
    csv: String,
}

impl Sweep {
    fn new(dir: String) -> Self {
        Self {
            dir,
            frame: 0,
            stage: 0,
            stage_frame: 0,
            samples: Vec::new(),
            timings: Timings::default(),
            csv: format!("view,tier,tier_name,gpu_ms,samples,{}\n", CAMERA_PASSES.join(",")),
        }
    }
}

const SWEEP_VIEWS: [(&str, Vec3, Vec3); 3] = [
    ("far", Vec3::new(-6.0, 2.2, -22.0), Vec3::new(0.0, 1.2, 4.6)),
    ("front", Vec3::new(-2.5, 1.7, -9.0), Vec3::new(0.0, 1.4, 5.0)),
    ("close", Vec3::new(-1.6, 1.6, -1.8), Vec3::new(0.0, 1.0, 4.6)),
];

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn tier_sweep(
    mut commands: Commands,
    mut sweep: ResMut<Sweep>,
    mut camera: Query<(&mut Transform, &mut FlyCam, &mut SmokeCamera)>,
    mut ui: Query<&mut Visibility, Or<(With<Hud>, With<Help>, With<Node>)>>,
    mut virtual_time: ResMut<Time<Virtual>>,
    mut settings: ResMut<SmokeSettings>,
    diagnostics: Res<DiagnosticsStore>,
    world: Res<SmokeWorld>,
    clock: Res<SmokeClock>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok((mut transform, mut cam, mut smoke_cam)) = camera.single_mut() else {
        return;
    };
    let sweep = sweep.as_mut();
    sweep.frame += 1;
    sweep.stage_frame += 1;
    for mut v in &mut ui {
        *v = Visibility::Hidden;
    }
    let fresh = sweep.timings.fresh(&diagnostics);
    let views = SWEEP_VIEWS.len();
    let tiers = quality::TIERS;
    // Stages: 0 warm-up; 1..=views empty references; then grow; then for
    // every view and tier (best first): settle, measure, shot (twice: lit and
    // silhouette); then exit.
    let grow_stage = 1 + views;
    let first_measure = grow_stage + 1;
    let measure_stages = views * tiers;
    let advance = |sweep: &mut Sweep| {
        sweep.stage += 1;
        sweep.stage_frame = 0;
        sweep.samples.clear();
    };
    match sweep.stage {
        0 => {
            if sweep.stage_frame > 40 {
                advance(sweep);
            }
        }
        s if s < grow_stage => {
            let (name, eye, at) = SWEEP_VIEWS[s - 1];
            if sweep.stage_frame == 1 {
                place(eye, at, &mut transform, &mut cam);
            }
            if sweep.stage_frame == 6 {
                screenshot(&mut commands, &sweep.dir, &format!("{name}_empty"));
            }
            if sweep.stage_frame > 10 {
                advance(sweep);
            }
        }
        s if s == grow_stage => {
            if sweep.stage_frame == 1 {
                commands.spawn((Smoke::default().with_seed(7), Transform::from_xyz(0.0, 0.0, 4.6)));
            }
            // Freeze the smoke once fully grown, so every tier sees the same shape.
            if grown(&world, &clock, 3.5) {
                virtual_time.pause();
                advance(sweep);
            }
        }
        s if s < first_measure + measure_stages => {
            let k = s - first_measure;
            let (name, eye, at) = SWEEP_VIEWS[k / tiers];
            let tier = tiers - 1 - k % tiers;
            if sweep.stage_frame == 1 {
                place(eye, at, &mut transform, &mut cam);
                smoke_cam.quality = quality::quality_of_tier(tier);
                smoke_cam.budget_ms = None;
                settings.debug_silhouette = false;
                sweep.timings.passes = Default::default();
            }
            // Let pipelines compile and timings settle, then collect.
            if sweep.stage_frame > 12
                && let Some(ms) = fresh
            {
                sweep.samples.push((ms, sweep.timings.passes));
            }
            const WANTED: usize = 16;
            if sweep.samples.len() == WANTED && sweep.stage_frame < 10_000 {
                let mut sorted = sweep.samples.clone();
                sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
                let (median, passes) = sorted[WANTED / 2];
                let passes: Vec<String> = passes.iter().map(|ms| format!("{ms:.3}")).collect();
                let _ = writeln!(
                    sweep.csv,
                    "{name},{tier},{},{median:.4},{WANTED},{}",
                    quality::TIER_NAMES[tier],
                    passes.join(",")
                );
                screenshot(&mut commands, &sweep.dir, &format!("{name}_tier{tier}"));
                sweep.stage_frame = 10_000;
            }
            if sweep.stage_frame == 10_003 {
                settings.debug_silhouette = true;
            }
            if sweep.stage_frame == 10_006 {
                screenshot(&mut commands, &sweep.dir, &format!("{name}_tier{tier}_mask"));
            }
            if sweep.stage_frame == 10_009 {
                settings.debug_silhouette = false;
                advance(sweep);
            }
            if sweep.stage_frame == 1500 {
                warn!("no GPU timings for {name} tier {tier}; skipping");
                let _ = writeln!(sweep.csv, "{name},{tier},{},,0", quality::TIER_NAMES[tier]);
                advance(sweep);
            }
        }
        _ => {
            if sweep.stage_frame == 1 {
                std::fs::write(format!("{}/tiers.csv", sweep.dir), &sweep.csv).ok();
                info!("tier sweep:\n{}", sweep.csv);
            }
            if sweep.stage_frame > 8 {
                exit.write(AppExit::Success);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Budget run: budget mode while the camera walks up to a smoke and back.
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct BudgetRun {
    dir: String,
    frame: u32,
    timings: Timings,
    csv: String,
}

impl BudgetRun {
    fn new(dir: String) -> Self {
        Self {
            dir,
            frame: 0,
            timings: Timings::default(),
            csv: "frame,eye_distance,coverage,tier,predicted_ms,measured_ms\n".into(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn budget_run(
    mut commands: Commands,
    mut run: ResMut<BudgetRun>,
    mut camera: Query<(&mut Transform, &mut FlyCam, &mut SmokeCamera, &SmokeQuality)>,
    budget: Res<DemoBudget>,
    mut virtual_time: ResMut<Time<Virtual>>,
    diagnostics: Res<DiagnosticsStore>,
    world: Res<SmokeWorld>,
    clock: Res<SmokeClock>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok((mut transform, mut cam, mut smoke_cam, status)) = camera.single_mut() else {
        return;
    };
    let run = run.as_mut();
    run.frame += 1;
    let f = run.frame;
    let fresh = run.timings.fresh(&diagnostics);
    let target = Vec3::new(0.0, 1.2, 4.6);
    if f == 40 {
        commands.spawn((Smoke::default().with_seed(7), Transform::from_xyz(0.0, 0.0, 4.6)));
        smoke_cam.budget_ms = Some(budget.0);
    }
    if f > 40 && grown(&world, &clock, 3.5) {
        virtual_time.pause();
    }
    // Walk: far (frames < 300), in to 3 m (300..600), hold, back out (700..900).
    let d = match f {
        0..300 => 24.0,
        300..600 => 24.0 - (f - 300) as f32 / 300.0 * 21.0,
        600..700 => 3.0,
        700..900 => 3.0 + (f - 700) as f32 / 200.0 * 21.0,
        _ => 24.0,
    };
    let dir = Vec3::new(-0.25, 0.05, -1.0).normalize();
    place(target + dir * d, target, &mut transform, &mut cam);
    if f > 40 {
        let _ = writeln!(
            run.csv,
            "{f},{d:.2},{:.4},{},{:.4},{}",
            status.coverage,
            status.tier,
            status.predicted_ms,
            fresh.map_or(String::new(), |m| format!("{m:.4}"))
        );
    }
    for (frame, name) in [(280, "budget_far"), (650, "budget_close"), (1000, "budget_far_again")] {
        if f == frame {
            screenshot(&mut commands, &run.dir, name);
        }
    }
    if f == 1010 {
        std::fs::write(format!("{}/budget.csv", run.dir), &run.csv).ok();
    }
    if f > 1015 {
        exit.write(AppExit::Success);
    }
}

// ---------------------------------------------------------------------------
// Agreement run: does the picture match the gameplay query, pixel by pixel?
// For a frozen faceted (or crystal) smoke seen from several places, saves the GPU's
// silhouette mask and, for every pixel, what the CPU says about the segment
// from the eye to the scene behind that pixel: blocked or not, and how much
// cloud the segment passes through.
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct AgreementRun {
    dir: String,
    stage: usize,
    stage_frame: u32,
}

impl AgreementRun {
    fn new(dir: String) -> Self {
        Self {
            dir,
            stage: 0,
            stage_frame: 0,
        }
    }
}

const AGREEMENT_VIEWS: [(&str, Vec3, Vec3); 4] = [
    ("far", Vec3::new(-6.0, 2.2, -22.0), Vec3::new(0.0, 1.2, 4.6)),
    ("front", Vec3::new(-2.5, 1.7, -9.0), Vec3::new(0.0, 1.4, 5.0)),
    ("close", Vec3::new(-1.6, 1.6, -1.8), Vec3::new(0.0, 1.0, 4.6)),
    // Along the cloud's edge towards the concrete pillar.
    ("edge", Vec3::new(-8.0, 1.6, -1.5), Vec3::new(5.9, 1.4, 2.4)),
];

/// Distance along a ray to the demo level (boxes and the ground plane).
fn scene_distance(level: &Level, eye: Vec3, dir: Vec3) -> f32 {
    let far = 500.0;
    let mut t = level.raycast(eye, dir, far).unwrap_or(far);
    if dir.y < -1e-6 {
        t = t.min(-eye.y / dir.y);
    }
    t
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn agreement_run(
    mut commands: Commands,
    mut run: ResMut<AgreementRun>,
    mut camera: Query<(&mut Transform, &mut FlyCam, &mut SmokeCamera, &Camera, &GlobalTransform)>,
    mut ui: Query<&mut Visibility, Or<(With<Hud>, With<Help>, With<Node>)>>,
    mut virtual_time: ResMut<Time<Virtual>>,
    mut settings: ResMut<SmokeSettings>,
    mut world: ResMut<SmokeWorld>,
    clock: Res<SmokeClock>,
    level: Res<Level>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok((mut transform, mut cam, mut smoke_cam, camera, global)) = camera.single_mut() else {
        return;
    };
    for mut v in &mut ui {
        *v = Visibility::Hidden;
    }
    let run = run.as_mut();
    run.stage_frame += 1;
    let advance = |run: &mut AgreementRun| {
        run.stage += 1;
        run.stage_frame = 0;
    };
    match run.stage {
        0 => {
            // Hard-edged styles only: faceted unless crystal was asked for.
            if settings.style == SmokeStyle::Volumetric {
                settings.style = SmokeStyle::Faceted;
            }
            smoke_cam.quality = 1.0;
            smoke_cam.budget_ms = None;
            if run.stage_frame > 40 {
                commands.spawn((Smoke::default().with_seed(7), Transform::from_xyz(0.0, 0.0, 4.6)));
                advance(run);
            }
        }
        1 => {
            if grown(&world, &clock, 3.5) {
                virtual_time.pause();
                advance(run);
            }
        }
        s if s < 2 + AGREEMENT_VIEWS.len() => {
            let (name, eye, at) = AGREEMENT_VIEWS[s - 2];
            match run.stage_frame {
                1 => {
                    place(eye, at, &mut transform, &mut cam);
                    settings.debug_silhouette = false;
                }
                12 => screenshot(&mut commands, &run.dir, &format!("{name}_lit")),
                16 => settings.debug_silhouette = true,
                20 => screenshot(&mut commands, &run.dir, &format!("{name}_gpu_mask")),
                24 => {
                    settings.debug_silhouette = false;
                    // The core's answers at the tick on screen.
                    let view = world.view(clock.drawn());
                    let size = camera.logical_viewport_size().unwrap_or(Vec2::new(960.0, 540.0));
                    let (w, h) = (size.x as usize, size.y as usize);
                    let eye = global.translation();
                    // Per pixel: any cloud on the segment (0/1), the amount of
                    // cloud (m), the core's rule (0/1), scene distance (m).
                    let mut blocked = vec![0u8; w * h];
                    let mut chords = vec![0f32; w * h];
                    let mut rule = vec![0u8; w * h];
                    let mut depth = vec![0f32; w * h];
                    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
                    let rows_per = h.div_ceil(threads);
                    std::thread::scope(|scope| {
                        let mut b_rest = blocked.as_mut_slice();
                        let mut c_rest = chords.as_mut_slice();
                        let mut r_rest = rule.as_mut_slice();
                        let mut d_rest = depth.as_mut_slice();
                        for k in 0..threads {
                            let rows = rows_per.min(h.saturating_sub(k * rows_per));
                            let (b, br) = b_rest.split_at_mut(rows * w);
                            let (c, cr) = c_rest.split_at_mut(rows * w);
                            let (r, rr) = r_rest.split_at_mut(rows * w);
                            let (d, dr) = d_rest.split_at_mut(rows * w);
                            (b_rest, c_rest, r_rest, d_rest) = (br, cr, rr, dr);
                            let (view, level) = (&view, level.as_ref());
                            let y0 = k * rows_per;
                            scope.spawn(move || {
                                for row in 0..rows {
                                    for x in 0..w {
                                        let px = Vec2::new(x as f32 + 0.5, (y0 + row) as f32 + 0.5);
                                        let Ok(ray) = camera.viewport_to_world(global, px) else {
                                            continue;
                                        };
                                        let dir: Vec3 = ray.direction.into();
                                        let t = scene_distance(level, eye, dir);
                                        let i = row * w + x;
                                        let target = eye + dir * t;
                                        let amount = view.occlusion(eye, target);
                                        d[i] = t;
                                        c[i] = amount;
                                        b[i] = (amount > 0.0) as u8;
                                        r[i] = view.blocks_sight(eye, target) as u8;
                                    }
                                }
                            });
                        }
                    });
                    let write = |suffix: &str, bytes: &[u8]| {
                        std::fs::write(format!("{}/{name}_{suffix}", run.dir), bytes).ok();
                    };
                    write("cpu_blocked.u8", &blocked);
                    write("rule_blocked.u8", &rule);
                    write(
                        "chord.f32",
                        &chords.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>(),
                    );
                    write(
                        "depth.f32",
                        &depth.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>(),
                    );
                    write("size.txt", format!("{w} {h}").as_bytes());
                }
                30 => advance(run),
                _ => {}
            }
        }
        _ => {
            if run.stage_frame > 4 {
                exit.write(AppExit::Success);
            }
        }
    }
}
