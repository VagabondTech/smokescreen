//! Interactive test range for `bevy_smokescreen`.
//!
//! `cargo run --release --example demo`. In a browser with WebGPU: see
//! `web/README.md`.
//!
//! Controls
//! - Click to look around with the mouse (Esc lets go of it), or hold the
//!   right mouse button. WASD: move, Space/C: up/down, Shift: fast
//! - Left mouse: shoot (bullets carve tunnels through smoke)
//! - G: throw a smoke grenade; it pops where it comes to rest
//! - H: throw an HE grenade (it clears a sphere that refills)
//! - X: clear all smokes
//! - F: cycle the style: volumetric, faceted, crystal (re-throws current smokes)
//! - 1-6: the quality dial, cheapest to best
//! - B: budget mode (the dial turns itself to fit a GPU budget), [ and ]: budget -/+
//! - M: toggle MSAA, T: toggle TAA (enables temporal jitter on the smoke)
//! - F1: toggle the help
//!
//! The buttons at the top right throw, clear, and step the style and the dial.
//!
//! On a touch screen: drag to look, put a thumb down in the lower left for a
//! stick that moves (look up and push forward to rise), tap the scene to
//! shoot where you tap, and tap the buttons.
//!
//! Settings: environment variables natively, and in a browser the same names
//! in lower case in the page's address (`?style=crystal&msaa=0`; see
//! `web/README.md`).
//! - `SMOKE_DEMO_STYLE=faceted` or `crystal` starts in that style.
//! - `SMOKE_DEMO_LDR=1` draws without HDR; `SMOKE_DEMO_MSAA=0` without MSAA.
//! - `SMOKE_DEMO_SMOKE=0` leaves out the smoke that pops at the start.
//! - `SMOKE_DEMO_BUDGET_MS=<ms>` sets the budget for budget mode (default 2).
//! - `SMOKE_DEMO_GPU_TIMINGS=1` / `0` turns GPU timestamp diagnostics on or
//!   off (HUD timings, budget mode, the tier sweep). Default: on, except on
//!   macOS, where they have lost the Metal device.
//! - `SMOKE_DEMO_CAPTURE=<dir>` runs a scripted sequence that saves screenshots
//!   (and measurements) into `<dir>` and exits. It draws offscreen, without a
//!   window, so it also works on a locked screen or a machine without a
//!   display; `SMOKE_DEMO_WINDOW=1` draws into a window instead.
//!   `SMOKE_DEMO_SIZE=<width>x<height>` sets the size (default 960x540).
//!   `SMOKE_DEMO_SCRIPT` picks the sequence:
//!   `showcase` (default; `SMOKE_DEMO_HUD=1` keeps the HUD in the shots),
//!   `tiers` (cost of every dial step, plus silhouette masks for the fairness
//!   check; without GPU timings it times whole frames, each waiting for the
//!   GPU, against the same view without smoke), `budget` (budget mode while
//!   a smoke comes close and goes away again), `agreement` (faceted or
//!   crystal silhouette masks next to the CPU query for every pixel; compare
//!   with `tools/agreement.py`) or `video` (every frame of short captioned
//!   clips, 30 a second, into `<dir>/<clip>/`; `SMOKE_DEMO_CLIPS=hero,ledge`
//!   records only those).

use std::{fmt::Write as _, sync::Arc, time::Duration};

use bevy::{
    anti_alias::taa::TemporalAntiAliasing,
    app::ScheduleRunnerPlugin,
    asset::RenderAssetUsages,
    camera::{Exposure, RenderTarget, ShadowLodOrigin},
    diagnostic::{DiagnosticPath, DiagnosticsStore, FrameTimeDiagnosticsPlugin},
    gizmos::config::{DefaultGizmoConfigGroup, GizmoConfigStore},
    input::mouse::AccumulatedMouseMotion,
    light::{GlobalAmbientLight, light_consts::lux},
    mesh::{Indices, PrimitiveTopology},
    platform::time::Instant,
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        diagnostic::RenderDiagnosticsPlugin,
        render_resource::{PollType, TextureFormat},
        renderer::RenderDevice,
        view::screenshot::{Screenshot, save_to_disk},
    },
    time::TimeUpdateStrategy,
    ui::{ComputedNode, IsDefaultUiCamera, RelativeCursorPosition, UiGlobalTransform},
    window::{CursorGrabMode, CursorOptions, ExitCondition, PrimaryWindow},
    winit::WinitPlugin,
};
use bevy_smokescreen::{prelude::*, quality, smokescreen::math::hash_u32};

fn main() {
    // Captures write files, so not in a browser.
    let capture = setting("capture").filter(|_| !cfg!(target_arch = "wasm32"));
    let script = setting("script").unwrap_or_else(|| "showcase".into());
    // Captures draw offscreen unless asked for a window: a window's
    // screenshots come out black while the screen is locked.
    let offscreen = capture.is_some() && setting("window").is_none();
    let size = setting("size")
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some(UVec2::new(w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or(if capture.is_some() {
            UVec2::new(960, 540)
        } else {
            UVec2::new(1600, 900)
        });
    let style = match setting("style").as_deref() {
        Some("faceted") => SmokeStyle::Faceted,
        Some("crystal") => SmokeStyle::Crystal,
        _ => SmokeStyle::Volumetric,
    };
    let budget_ms = setting("budget_ms").and_then(|v| v.parse().ok()).unwrap_or(2.0);

    // GPU timings (HUD, budget mode, tier sweep) use timestamp queries, which
    // lost the device on one Metal setup and which Bevy can't use through
    // WebGPU; off by default on macOS and the web.
    let gpu_timings = setting("gpu_timings")
        .map(|v| v != "0")
        .unwrap_or(!cfg!(any(target_os = "macos", target_arch = "wasm32")));

    let mut app = App::new();
    if offscreen {
        app.add_plugins((
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    ..default()
                })
                .disable::<WinitPlugin>(),
            ScheduleRunnerPlugin::run_loop(Duration::ZERO),
        ));
        let image = Image::new_target_texture(size.x, size.y, TextureFormat::Rgba8UnormSrgb, None);
        let image = app.world_mut().resource_mut::<Assets<Image>>().add(image);
        app.insert_resource(ShotTarget::Image(image));
    } else {
        app.add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Smokescreen demo".into(),
                resolution: (size.x, size.y).into(),
                // On the web (see web/index.html): draw into the page's canvas,
                // sized by the page.
                canvas: cfg!(target_arch = "wasm32").then(|| "#bevy".into()),
                fit_canvas_to_parent: true,
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ShotTarget::Window);
    }
    app.add_plugins((SmokePlugin::default(), FrameTimeDiagnosticsPlugin::default()))
        .insert_resource(SmokeSettings { style, ..default() })
        .insert_resource(DemoBudget(budget_ms))
        .insert_resource(GlobalAmbientLight {
            color: Color::srgb(0.75, 0.82, 1.0),
            brightness: 350.0,
            ..default()
        })
        .insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.85)))
        .init_resource::<Effects>()
        .init_resource::<Actions>()
        .init_resource::<MouseLook>()
        .insert_resource(TouchControls {
            active: coarse_pointer(),
            ..default()
        })
        .configure_sets(Update, (Step::Input, Step::Script, Step::Act).chain())
        .add_systems(
            Startup,
            (
                setup_level,
                setup_camera_and_hud,
                setup_grenades,
                setup_toolbar,
                setup_stick,
            ),
        )
        .add_systems(
            Update,
            (mouse_capture, toolbar, touch_input).chain().in_set(Step::Input),
        )
        .add_systems(
            Update,
            (
                fly_camera,
                weapons,
                fly_grenades,
                quality_keys,
                style_key,
                clear_smokes,
                update_effects,
                update_hud,
                update_toolbar,
                update_stick,
                update_help,
            )
                .in_set(Step::Act),
        );
    #[cfg(target_arch = "wasm32")]
    app.add_systems(Update, (browser_pointer_lock.before(mouse_capture), mark_ready));

    if gpu_timings {
        app.add_plugins(RenderDiagnosticsPlugin);
    }
    app.insert_resource(GpuTimings(gpu_timings));

    if let Some(dir) = capture {
        std::fs::create_dir_all(&dir).ok();
        let step = if script == "video" { 1.0 / VIDEO_FPS } else { 1.0 / 20.0 };
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(step)))
            .init_resource::<Finish>()
            .add_systems(Update, finish.after(Step::Act));
        match script.as_str() {
            "tiers" => {
                app.insert_resource(Sweep::new(dir))
                    .add_systems(Update, tier_sweep.in_set(Step::Script));
                // Without GPU timings the sweep times whole frames instead:
                // make every frame wait for the GPU, so they include its work.
                if !gpu_timings && let Some(render_app) = app.get_sub_app_mut(RenderApp) {
                    render_app.add_systems(Render, wait_for_gpu.in_set(RenderSystems::Cleanup));
                }
            }
            "budget" => {
                app.insert_resource(BudgetRun::new(dir))
                    .add_systems(Update, budget_run.in_set(Step::Script));
            }
            "agreement" => {
                app.insert_resource(AgreementRun::new(dir))
                    .add_systems(Update, agreement_run.in_set(Step::Script));
            }
            "video" => {
                app.insert_resource(Video::new(dir, size))
                    .init_resource::<Sightlines>()
                    .add_systems(Startup, setup_bots)
                    .add_systems(
                        Update,
                        (video_script.in_set(Step::Script), draw_sightlines.in_set(Step::Act)),
                    );
            }
            _ => {
                app.insert_resource(Capture { dir, step: 0 })
                    .add_systems(Update, capture_script.in_set(Step::Script));
            }
        }
    } else if setting("smoke").as_deref() != Some("0") {
        app.add_systems(Update, opening_smoke);
    }
    app.run();
}

/// A demo setting: `SMOKE_DEMO_<NAME>` natively, `?<name>=<value>` (or just
/// `?<name>`) in the browser's address bar, so a phone can try settings too.
fn setting(name: &str) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let search = web_sys::window()?.location().search().ok()?;
        search.trim_start_matches('?').split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then(|| value.to_string())
        })
    }
    #[cfg(not(target_arch = "wasm32"))]
    std::env::var(format!("SMOKE_DEMO_{}", name.to_uppercase())).ok()
}

/// The order of the demo's own systems within a frame: input, then a capture
/// script, then everything that acts on what they asked for.
#[derive(SystemSet, Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Step {
    Input,
    Script,
    Act,
}

/// Where screenshots come from: the window, or the image an offscreen camera
/// draws into.
#[derive(Resource, Clone)]
enum ShotTarget {
    Window,
    Image(Handle<Image>),
}

/// Set by a capture script when it's done: the demo exits once every
/// screenshot has been written. (Offscreen, nothing holds the CPU back, so it
/// can be many frames ahead of the GPU.)
#[derive(Resource, Default)]
struct Finish(bool);

fn finish(finish: Res<Finish>, pending: Query<(), With<Screenshot>>, mut exit: MessageWriter<AppExit>) {
    if finish.0 && pending.is_empty() {
        exit.write(AppExit::Success);
    }
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
    let mut material = |r: f32, g: f32, b: f32, roughness: f32| {
        materials.add(StandardMaterial {
            base_color: Color::srgb(r, g, b),
            perceptual_roughness: roughness,
            ..default()
        })
    };
    let concrete = material(0.62, 0.6, 0.56, 0.9);
    let dark_concrete = material(0.42, 0.42, 0.43, 0.8);
    let plaster = material(0.78, 0.72, 0.62, 0.85);
    let cool_plaster = material(0.66, 0.7, 0.74, 0.85);
    let brick = material(0.56, 0.34, 0.26, 0.9);
    let crate_wood = material(0.52, 0.38, 0.22, 0.8);
    let pale_wood = material(0.64, 0.5, 0.32, 0.8);
    let red = material(0.7, 0.12, 0.1, 0.5);

    // Ground: concrete slabs in slightly different shades, so distance and
    // camera moves read on screen.
    commands.spawn((
        Mesh3d(meshes.add(slab_ground(90.0, 2.0))),
        MeshMaterial3d(concrete.clone()),
    ));

    // (centre, size, material) for every solid box. The same list drives the
    // meshes and the smoke collision, so they always agree.
    let mut boxes: Vec<(Vec3, Vec3, Handle<StandardMaterial>)> = Vec::new();
    let mut wall = |min: Vec3, max: Vec3, mat: &Handle<StandardMaterial>| {
        boxes.push(((min + max) * 0.5, max - min, mat.clone()));
        boxes.len() - 1
    };
    let h = 4.0;
    // Front wall of a building at z = 6: a 2 m doorway (lintel above) and,
    // to its left, a window.
    wall(Vec3::new(-9.0, 0.0, 5.8), Vec3::new(-6.5, h, 6.2), &plaster);
    wall(Vec3::new(-6.5, 0.0, 5.8), Vec3::new(-4.5, 1.1, 6.2), &plaster);
    wall(Vec3::new(-6.5, 2.3, 5.8), Vec3::new(-4.5, h, 6.2), &plaster);
    wall(Vec3::new(-4.5, 0.0, 5.8), Vec3::new(-1.0, h, 6.2), &plaster);
    wall(Vec3::new(1.0, 0.0, 5.8), Vec3::new(9.0, h, 6.2), &plaster);
    wall(Vec3::new(-1.0, 2.6, 5.8), Vec3::new(1.0, h, 6.2), &plaster);
    // Side walls, back wall and roof: rooms the sun can't reach.
    wall(Vec3::new(-9.0, 0.0, 6.2), Vec3::new(-8.6, h, 15.0), &plaster);
    wall(Vec3::new(8.6, 0.0, 6.2), Vec3::new(9.0, h, 15.0), &plaster);
    wall(Vec3::new(-9.0, 0.0, 14.6), Vec3::new(9.0, h, 15.0), &plaster);
    let roof = wall(Vec3::new(-9.0, h, 5.8), Vec3::new(9.0, h + 0.3, 15.0), &concrete);
    // A partition with a doorway at the back: a second, smaller room.
    wall(Vec3::new(3.0, 0.0, 6.2), Vec3::new(3.4, h, 10.5), &cool_plaster);
    wall(Vec3::new(3.0, 0.0, 12.0), Vec3::new(3.4, h, 14.6), &cool_plaster);
    wall(Vec3::new(3.0, 2.4, 10.5), Vec3::new(3.4, h, 12.0), &cool_plaster);
    // A 2.4 m corridor to the right, open at both ends.
    wall(Vec3::new(11.0, 0.0, -10.0), Vec3::new(11.4, 3.2, 4.0), &concrete);
    wall(Vec3::new(13.8, 0.0, -10.0), Vec3::new(14.2, 3.2, 4.0), &concrete);
    // A raised platform to the left, 1.5 m high with steps, for smoke to pour
    // off. (A cloud's grid reaches 1.8 m below where it pops: higher ledges
    // would cut the smoke off in mid-air.)
    wall(Vec3::new(-21.0, 0.0, -6.0), Vec3::new(-13.0, 1.5, 4.0), &brick);
    for k in 0..3 {
        let x = -13.0 + 0.5 * k as f32;
        wall(
            Vec3::new(x, 0.0, -6.0),
            Vec3::new(x + 0.5, 1.125 - 0.375 * k as f32, -4.0),
            &dark_concrete,
        );
    }
    wall(Vec3::new(-18.5, 1.5, 1.0), Vec3::new(-17.3, 2.7, 2.2), &pale_wood);
    // Cover: crates, a pillar and a low wall in the open.
    wall(Vec3::new(-4.6, 0.0, 0.4), Vec3::new(-3.4, 1.2, 1.6), &crate_wood);
    wall(Vec3::new(-4.4, 1.2, 0.6), Vec3::new(-3.6, 2.0, 1.4), &pale_wood);
    wall(Vec3::new(3.0, 0.0, -2.5), Vec3::new(4.2, 1.2, -1.3), &crate_wood);
    wall(Vec3::new(9.2, 0.0, -6.6), Vec3::new(10.4, 1.2, -5.4), &pale_wood);
    wall(Vec3::new(8.9, 0.0, -5.2), Vec3::new(9.9, 1.0, -4.2), &crate_wood);
    wall(Vec3::new(5.5, 0.0, 2.0), Vec3::new(6.3, 3.5, 2.8), &dark_concrete);
    wall(Vec3::new(-3.2, 0.0, 1.0), Vec3::new(-0.8, 0.9, 1.3), &concrete);
    // Something bright to hide behind the smoke.
    wall(Vec3::new(-0.4, 0.0, 9.6), Vec3::new(0.4, 1.8, 10.4), &red);

    let mut obstacle_boxes = Vec::new();
    for (i, (center, size, mat)) in boxes.into_iter().enumerate() {
        let mut entity = commands.spawn((
            Mesh3d(meshes.add(no_uvs(Cuboid::from_size(size)))),
            MeshMaterial3d(mat),
            Transform::from_translation(center),
        ));
        if i == roof {
            entity.insert(Roof);
        }
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

/// The building's roof, hidden for the video's cutaway (it still blocks smoke).
#[derive(Component)]
struct Roof;

/// A square of `size` metres made of `slab`-metre slabs, each a slightly
/// different shade (vertex colours times the material's colour).
fn slab_ground(size: f32, slab: f32) -> Mesh {
    let n = (size / slab) as u32;
    let half = size * 0.5;
    let (mut positions, mut colors, mut indices) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..n {
        for j in 0..n {
            let (x, z) = (-half + i as f32 * slab, -half + j as f32 * slab);
            let shade = 0.88 + 0.16 * (hash_u32(i * 7919 + j * 104_729) % 1024) as f32 / 1024.0;
            let first = positions.len() as u32;
            for (dx, dz) in [(0.0, 0.0), (slab, 0.0), (slab, slab), (0.0, slab)] {
                positions.push([x + dx, 0.0, z + dz]);
                colors.push([shade, shade, shade, 1.0]);
            }
            indices.extend([first, first + 2, first + 1, first, first + 3, first + 2]);
        }
    }
    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
        .with_inserted_indices(Indices::U32(indices))
}

/// A shape's mesh without UVs. Nothing here is textured, and Safari 26 can't
/// compile Bevy 0.20's PBR shader for meshes with UVs: it keeps textures in
/// `let`s, which needs WGSL's `texture_and_sampler_let`.
fn no_uvs(shape: impl Into<Mesh>) -> Mesh {
    shape.into().with_removed_attribute(Mesh::ATTRIBUTE_UV_0)
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

fn setup_camera_and_hud(mut commands: Commands, target: Res<ShotTarget>) {
    let eye = Vec3::new(-2.5, 1.7, -9.0);
    let look = Transform::from_translation(eye).looking_at(Vec3::new(0.0, 1.4, 5.0), Vec3::Y);
    let (yaw, pitch, _) = look.rotation.to_euler(EulerRot::YXZ);
    let msaa = if setting("msaa").as_deref() == Some("0") {
        Msaa::Off
    } else {
        Msaa::Sample4
    };
    let mut camera = commands.spawn((
        Camera3d::default(),
        Exposure::default(),
        msaa,
        SmokeCamera::HIGH,
        look,
        FlyCam { yaw, pitch },
        // The HUD's camera, window or not.
        IsDefaultUiCamera,
    ));
    // SMOKE_DEMO_LDR=1 (`?ldr` in a browser) exercises the non-HDR composite path.
    if setting("ldr").is_none() {
        camera.insert(bevy::camera::Hdr);
    }
    if let ShotTarget::Image(image) = target.as_ref() {
        // Without a window, Bevy wants telling which camera point light
        // shadows are detailed around.
        camera.insert((RenderTarget::Image(image.clone().into()), ShadowLodOrigin));
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
            "Click to look around (Esc lets go), or hold RMB  WASD move  Space/C up/down  Shift fast\n\
             LMB shoot  G smoke grenade  H HE grenade  X clear  F style\n\
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
    look: Res<MouseLook>,
    touch: Res<TouchControls>,
    mut cameras: Query<(&mut Transform, &mut FlyCam)>,
) {
    let Ok((mut transform, mut cam)) = cameras.single_mut() else {
        return;
    };
    if look.captured || buttons.pressed(MouseButton::Right) {
        cam.yaw -= motion.delta.x * 0.003;
        cam.pitch = (cam.pitch - motion.delta.y * 0.003).clamp(-1.5, 1.5);
    }
    // Dragging a finger turns the view as moving the mouse does.
    cam.yaw -= touch.look.x * TOUCH_LOOK;
    cam.pitch = (cam.pitch - touch.look.y * TOUCH_LOOK).clamp(-1.5, 1.5);
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
    // Not Ctrl: in a browser, Ctrl+W closes the tab.
    if keys.pressed(KeyCode::KeyC) {
        dir -= Vec3::Y;
    }
    let speed = if keys.pressed(KeyCode::ShiftLeft) { 12.0 } else { 4.0 };
    // The thumb stick moves as far as it's pushed. It's a fly camera, so
    // looking up and pushing forward rises.
    let push = dir.normalize_or_zero() + forward * -touch.stick.y + right * touch.stick.x;
    transform.translation += push.clamp_length_max(1.0) * speed * time.delta_secs();
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

/// What to do this frame, asked for by the toolbar or a capture script (the
/// keyboard is read where it's used).
#[derive(Resource, Default)]
struct Actions {
    /// Bullets from the camera, in these directions.
    shoot: Vec<Vec3>,
    /// Throw a smoke grenade from the camera.
    smoke: bool,
    /// Throw an HE grenade from the camera.
    explode: bool,
    /// An explosion right here.
    explode_at: Option<Vec3>,
    cycle_style: bool,
    cycle_quality: bool,
    clear: bool,
}

#[allow(clippy::too_many_arguments)]
fn weapons(
    mut commands: Commands,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    look: Res<MouseLook>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    level: Res<Level>,
    grenades: Res<GrenadeAssets>,
    mut actions: ResMut<Actions>,
    mut bullets: MessageWriter<SmokeBullet>,
    mut explosions: MessageWriter<SmokeExplosion>,
    mut effects: ResMut<Effects>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let origin = cam.translation();
    let forward = cam.forward().as_vec3();
    // The click that grabbed the mouse doesn't also shoot.
    if look.captured && !look.just_captured && buttons.just_pressed(MouseButton::Left) {
        actions.shoot.push(forward);
    }
    actions.smoke |= keys.just_pressed(KeyCode::KeyG);
    actions.explode |= keys.just_pressed(KeyCode::KeyH);

    for dir in std::mem::take(&mut actions.shoot) {
        shoot(&mut commands, &level, &mut bullets, &mut effects, origin, dir);
    }
    // Grenades leave from the right hand, a little below the eye, in an arc.
    let hand = origin + forward * 0.5 + cam.right().as_vec3() * 0.2 - Vec3::Y * 0.2;
    let velocity = forward * 13.0 + Vec3::Y * 2.5;
    if std::mem::take(&mut actions.smoke) {
        throw(&mut commands, &grenades, GrenadeKind::Smoke, hand, velocity);
    }
    if std::mem::take(&mut actions.explode) {
        throw(&mut commands, &grenades, GrenadeKind::He, hand, velocity);
    }
    if let Some(p) = actions.explode_at.take() {
        explode(&mut commands, &mut explosions, p);
    }
}

/// A bullet from `origin` along `dir` to the first wall, with a tracer and a
/// muzzle flash.
fn shoot(
    commands: &mut Commands,
    level: &Level,
    bullets: &mut MessageWriter<SmokeBullet>,
    effects: &mut Effects,
    origin: Vec3,
    dir: Vec3,
) {
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

/// An HE explosion at `p`: a cleared sphere in the smoke and a flash.
fn explode(commands: &mut Commands, explosions: &mut MessageWriter<SmokeExplosion>, p: Vec3) {
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

// ---------------------------------------------------------------------------
// Grenades: thrown, bounced off the level, then a smoke pops where one comes
// to rest, or an HE goes off after its fuse.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GrenadeKind {
    Smoke,
    He,
}

#[derive(Component)]
struct Grenade {
    kind: GrenadeKind,
    velocity: Vec3,
    age: f32,
    /// Seconds spent lying still.
    still: f32,
    /// Seconds left of counting as on a floor since it last touched one.
    on_floor: f32,
}

#[derive(Resource)]
struct GrenadeAssets {
    smoke: (Handle<Mesh>, Handle<StandardMaterial>),
    he: (Handle<Mesh>, Handle<StandardMaterial>),
}

const GRENADE_RADIUS: f32 = 0.07;
const GRAVITY: f32 = 9.81;
const HE_FUSE: f32 = 1.6;

fn setup_grenades(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut material = |color: Color| {
        materials.add(StandardMaterial {
            base_color: color,
            perceptual_roughness: 0.5,
            metallic: 0.3,
            ..default()
        })
    };
    commands.insert_resource(GrenadeAssets {
        smoke: (
            meshes.add(no_uvs(Cylinder::new(GRENADE_RADIUS * 0.75, GRENADE_RADIUS * 2.2))),
            material(Color::srgb(0.25, 0.32, 0.2)),
        ),
        he: (
            meshes.add(no_uvs(Sphere::new(GRENADE_RADIUS))),
            material(Color::srgb(0.2, 0.22, 0.2)),
        ),
    });
}

fn throw(commands: &mut Commands, assets: &GrenadeAssets, kind: GrenadeKind, from: Vec3, velocity: Vec3) {
    let (mesh, material) = match kind {
        GrenadeKind::Smoke => &assets.smoke,
        GrenadeKind::He => &assets.he,
    };
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(material.clone()),
        Transform::from_translation(from),
        Grenade {
            kind,
            velocity,
            age: 0.0,
            still: 0.0,
            on_floor: 0.0,
        },
    ));
}

/// The velocity that takes a grenade from `from` to `to` in `seconds`.
fn lob(from: Vec3, to: Vec3, seconds: f32) -> Vec3 {
    let d = to - from;
    Vec3::new(d.x, d.y + 0.5 * GRAVITY * seconds * seconds, d.z) / seconds
}

fn fly_grenades(
    mut commands: Commands,
    time: Res<Time>,
    level: Res<Level>,
    mut grenades: Query<(Entity, &mut Grenade, &mut Transform)>,
    mut explosions: MessageWriter<SmokeExplosion>,
) {
    let dt = time.delta_secs();
    for (entity, mut grenade, mut transform) in &mut grenades {
        grenade.age += dt;
        // Short steps, so a fast grenade can't pass through a thin wall.
        let steps = ((dt * 240.0).ceil() as u32).clamp(1, 32);
        let h = dt / steps as f32;
        let mut p = transform.translation;
        let mut v = grenade.velocity;
        let mut resting = false;
        for _ in 0..steps {
            v.y -= GRAVITY * h;
            let motion = v * h;
            if let Some((t, normal)) = sweep(&level.0, p, motion, GRENADE_RADIUS) {
                p += motion * t + normal * 0.001;
                let into = v.dot(normal);
                let along = v - normal * into;
                // A bounce loses most of the speed into the surface; a graze
                // just slides along it.
                v = if into < -1.0 {
                    along * 0.55 - normal * into * 0.25
                } else {
                    along
                };
                if normal.y > 0.7 {
                    grenade.on_floor = 0.05;
                }
            } else {
                p += motion;
            }
            // Rolling friction while on a floor (it hops off it between contacts).
            if grenade.on_floor > 0.0 {
                grenade.on_floor -= h;
                let damp = (1.0 - 6.0 * h).max(0.0);
                (v.x, v.z) = (v.x * damp, v.z * damp);
                if v.length() < 0.25 {
                    v = Vec3::ZERO;
                    resting = true;
                }
            }
        }
        // Tumble while flying.
        let spin = v.cross(Vec3::Y).normalize_or_zero();
        if spin != Vec3::ZERO {
            transform.rotate(Quat::from_axis_angle(spin, -v.length() * dt * 2.0));
        }
        transform.translation = p;
        grenade.velocity = v;
        grenade.still = if resting || v.length() < 0.05 {
            grenade.still + dt
        } else {
            0.0
        };
        let go_off = match grenade.kind {
            GrenadeKind::Smoke => grenade.still > 0.3 || grenade.age > 8.0,
            GrenadeKind::He => grenade.age > HE_FUSE,
        };
        if !go_off {
            continue;
        }
        commands.entity(entity).despawn();
        match grenade.kind {
            GrenadeKind::Smoke => {
                let floor = p - Vec3::Y * GRENADE_RADIUS;
                commands.spawn((Smoke::default(), Transform::from_translation(floor)));
            }
            GrenadeKind::He => explode(&mut commands, &mut explosions, p),
        }
    }
}

/// First contact of a sphere of radius `r` moving by `motion` from `from`
/// with the level's boxes and ground: how far along the motion (0 to 1), and
/// the surface's normal. Boxes the sphere starts inside are ignored.
fn sweep(level: &BoxObstacles, from: Vec3, motion: Vec3, r: f32) -> Option<(f32, Vec3)> {
    let mut best: Option<(f32, Vec3)> = None;
    let mut consider = |t: f32, normal: Vec3| {
        if best.is_none_or(|(b, _)| t < b) {
            best = Some((t, normal));
        }
    };
    if level.ground.is_some() && motion.y < 0.0 && from.y >= r - 1e-4 {
        let t = (r - from.y) / motion.y;
        if t <= 1.0 {
            consider(t.max(0.0), Vec3::Y);
        }
    }
    'boxes: for b in &level.boxes {
        let (min, max) = (b.min - r, b.max + r);
        let (mut near, mut far, mut normal) = (0.0f32, 1.0f32, Vec3::ZERO);
        for axis in 0..3 {
            let (o, d) = (from[axis], motion[axis]);
            if d.abs() < 1e-9 {
                if o < min[axis] || o > max[axis] {
                    continue 'boxes;
                }
                continue;
            }
            let (mut t0, mut t1) = ((min[axis] - o) / d, (max[axis] - o) / d);
            if t0 > t1 {
                std::mem::swap(&mut t0, &mut t1);
            }
            if t0 > near {
                near = t0;
                normal = Vec3::ZERO;
                normal[axis] = -d.signum();
            }
            far = far.min(t1);
            if near > far {
                continue 'boxes;
            }
        }
        if normal != Vec3::ZERO {
            consider(near, normal);
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Mouse capture and the toolbar.
// ---------------------------------------------------------------------------

/// Click to look around with the mouse; Esc lets go of it.
#[derive(Resource, Default)]
struct MouseLook {
    captured: bool,
    /// The mouse was captured by this frame's click (which shouldn't also shoot).
    just_captured: bool,
}

fn mouse_capture(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    toolbar: Query<&RelativeCursorPosition, With<ToolButton>>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut look: ResMut<MouseLook>,
) {
    look.just_captured = false;
    let Ok(mut cursor) = cursor.single_mut() else {
        return;
    };
    if look.captured && keys.just_pressed(KeyCode::Escape) {
        look.captured = false;
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    } else if !look.captured
        && mouse.just_pressed(MouseButton::Left)
        && !toolbar.iter().any(RelativeCursorPosition::cursor_over)
    {
        look.captured = true;
        look.just_captured = true;
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
    }
}

/// In a browser, the page decides: Esc ends the pointer lock without the app
/// hearing of it, and the page may refuse a lock (an unfocused window, say).
/// So the mouse counts as captured only while the page holds the lock, and
/// once a lock ends, or never comes, the cursor is let go so that the next
/// click asks again.
#[cfg(target_arch = "wasm32")]
fn browser_pointer_lock(
    time: Res<Time<Real>>,
    mut look: ResMut<MouseLook>,
    mut asked_at: Local<Option<f32>>,
    mut granted: Local<bool>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    let locked = web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| d.pointer_lock_element().is_some());
    look.captured = locked;
    let Ok(mut cursor) = cursor.single_mut() else {
        return;
    };
    if cursor.grab_mode == CursorGrabMode::None {
        *asked_at = None;
        *granted = false;
        return;
    }
    let now = time.elapsed_secs();
    let asked = *asked_at.get_or_insert(now);
    *granted |= locked;
    if !locked && (*granted || now - asked > 0.5) {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    }
}

/// Tells the page (web/index.html) the demo is drawing, so it can drop its
/// loading message.
#[cfg(target_arch = "wasm32")]
fn mark_ready(mut frames: Local<u32>) {
    *frames += 1;
    if *frames == 5
        && let Some(canvas) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("bevy"))
    {
        canvas.set_attribute("data-ready", "").ok();
    }
}

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum ToolButton {
    Smoke,
    He,
    Clear,
    Style,
    Quality,
}

/// The text of a toolbar button.
#[derive(Component)]
struct ToolLabel(ToolButton);

const BUTTON: Color = Color::srgba(0.08, 0.09, 0.11, 0.72);
const BUTTON_HOVERED: Color = Color::srgba(0.22, 0.25, 0.3, 0.85);

fn setup_toolbar(mut commands: Commands) {
    let buttons = [
        ToolButton::Smoke,
        ToolButton::He,
        ToolButton::Clear,
        ToolButton::Style,
        ToolButton::Quality,
    ];
    commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            top: px(10),
            right: px(12),
            // On a narrow screen the buttons wrap onto more rows.
            max_width: percent(50),
            flex_wrap: FlexWrap::Wrap,
            justify_content: JustifyContent::FlexEnd,
            column_gap: px(6),
            row_gap: px(6),
            ..default()
        })
        .with_children(|bar| {
            for button in buttons {
                bar.spawn((
                    button,
                    Node {
                        padding: UiRect::axes(px(10), px(6)),
                        border_radius: BorderRadius::all(px(5)),
                        ..default()
                    },
                    BackgroundColor(BUTTON),
                    RelativeCursorPosition::default(),
                ))
                .with_child((
                    // Written by `update_toolbar`.
                    Text::default(),
                    TextFont {
                        font_size: FontSize::Px(14.0),
                        ..default()
                    },
                    ToolLabel(button),
                ));
            }
        });
}

fn toolbar(
    mouse: Res<ButtonInput<MouseButton>>,
    look: Res<MouseLook>,
    mut buttons: Query<(&ToolButton, &RelativeCursorPosition, &mut BackgroundColor)>,
    mut actions: ResMut<Actions>,
) {
    for (button, cursor, mut color) in &mut buttons {
        let over = cursor.cursor_over() && !look.captured;
        let wanted = if over { BUTTON_HOVERED } else { BUTTON };
        if color.0 != wanted {
            color.0 = wanted;
        }
        if over && mouse.just_pressed(MouseButton::Left) {
            press(*button, &mut actions);
        }
    }
}

/// What a button does, clicked or tapped.
fn press(button: ToolButton, actions: &mut Actions) {
    match button {
        ToolButton::Smoke => actions.smoke = true,
        ToolButton::He => actions.explode = true,
        ToolButton::Clear => actions.clear = true,
        ToolButton::Style => actions.cycle_style = true,
        ToolButton::Quality => actions.cycle_quality = true,
    }
}

/// The buttons say what the style and the dial are on, more briefly on a
/// small screen, and without their keys on a touch screen.
fn update_toolbar(
    settings: Res<SmokeSettings>,
    touch: Res<TouchControls>,
    cameras: Query<(&SmokeQuality, &Camera)>,
    mut labels: Query<(&ToolLabel, &mut Text)>,
) {
    let (tier, small) = cameras
        .single()
        .map_or(("-", false), |(q, camera)| (q.tier_name(), small_screen(camera)));
    for (label, mut text) in &mut labels {
        let wanted = tool_label(label.0, settings.style, tier, small, touch.active);
        if text.0 != wanted {
            text.0 = wanted;
        }
    }
}

fn tool_label(button: ToolButton, style: SmokeStyle, tier: &str, small: bool, touch: bool) -> String {
    let (text, key) = match button {
        ToolButton::Smoke if small => ("Smoke".into(), "G"),
        ToolButton::Smoke => ("Smoke grenade".into(), "G"),
        ToolButton::He if small => ("HE".into(), "H"),
        ToolButton::He => ("HE grenade".into(), "H"),
        ToolButton::Clear => ("Clear".into(), "X"),
        ToolButton::Style if small => (capitalised(style_name(style)), "F"),
        ToolButton::Style => (format!("Style: {}", style_name(style)), "F"),
        ToolButton::Quality if small => (capitalised(tier), "1-6"),
        ToolButton::Quality => (format!("Quality: {tier}"), "1-6"),
    };
    if touch { text } else { format!("{text} ({key})") }
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// Narrower than this (in logical pixels: a phone, or a tablet held upright),
/// the buttons and the HUD say less.
const SMALL_SCREEN: f32 = 900.0;

fn small_screen(camera: &Camera) -> bool {
    camera.logical_viewport_size().is_some_and(|size| size.x < SMALL_SCREEN)
}

fn style_name(style: SmokeStyle) -> &'static str {
    match style {
        SmokeStyle::Volumetric => "volumetric",
        SmokeStyle::Faceted => "faceted",
        SmokeStyle::Crystal => "crystal",
    }
}

/// Smokes and grenades: what clearing removes.
type Clearable = Or<(With<Smoke>, With<Grenade>)>;

/// X (or the toolbar): every smoke and grenade goes.
fn clear_smokes(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut actions: ResMut<Actions>,
    things: Query<Entity, Clearable>,
) {
    if std::mem::take(&mut actions.clear) | keys.just_pressed(KeyCode::KeyX) {
        for entity in &things {
            commands.entity(entity).despawn();
        }
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
    mut cameras: Query<(
        Entity,
        &mut SmokeCamera,
        &SmokeQuality,
        &mut Msaa,
        Has<TemporalAntiAliasing>,
    )>,
    mut help: Query<&mut Visibility, With<Help>>,
    mut budget: ResMut<DemoBudget>,
    mut actions: ResMut<Actions>,
) {
    let Ok((entity, mut smoke, status, mut msaa, has_taa)) = cameras.single_mut() else {
        return;
    };
    // The toolbar steps the dial up, round to the cheapest after the best.
    if std::mem::take(&mut actions.cycle_quality) {
        smoke.quality = quality::quality_of_tier((status.tier + 1) % quality::TIERS);
        smoke.budget_ms = None;
    }
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
    mut actions: ResMut<Actions>,
    mut settings: ResMut<SmokeSettings>,
    smokes: Query<(Entity, &Smoke, &Transform)>,
) {
    if !(std::mem::take(&mut actions.cycle_style) | keys.just_pressed(KeyCode::KeyF)) {
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
        &Camera,
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
    let (Ok(mut text), Ok((smoke_cam, status, msaa, taa, cam, camera))) = (hud.single_mut(), cameras.single()) else {
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
        gpu = if cfg!(target_arch = "wasm32") {
            "  (no GPU timings in a browser)".into()
        } else {
            "  (no GPU timings: SMOKE_DEMO_GPU_TIMINGS=1 turns them on, if the adapter has timestamps)".into()
        };
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
    let count = smokes.iter().filter(|s| s.cloud().is_some()).count();
    text.0 = if small_screen(camera) {
        // Short lines, to fit beside the buttons on a phone held either way.
        format!(
            "FPS {fps:.0}   smokes {count}\n{dial}\nRed box visible: {:.0}%",
            seen * 100.0
        )
    } else {
        format!(
            "FPS {fps:.0}   smokes {count}   style {style}   {dial}   MSAA {}   TAA {}\nGPU{gpu}\nRed box visibility (CPU query): {:.0}%",
            msaa.samples(),
            if taa { "on" } else { "off" },
            seen * 100.0,
        )
    };
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

fn screenshot(commands: &mut Commands, target: &ShotTarget, dir: &str, name: &str) {
    let path = format!("{dir}/{name}.png");
    let shot = match target {
        ShotTarget::Window => Screenshot::primary_window(),
        ShotTarget::Image(image) => Screenshot::image(image.clone()),
    };
    commands.spawn(shot).observe(save_to_disk(path));
}

// ---------------------------------------------------------------------------
// Touch: drag to look, a thumb stick to move, tap to shoot or press a button.
// ---------------------------------------------------------------------------

/// How far a finger may move (logical pixels) and for how long it may stay
/// down (seconds) to count as a tap.
const TAP_SLOP: f32 = 10.0;
const TAP_TIME: f32 = 0.35;
/// How far the thumb stick reaches, and the size of its knob (logical pixels).
const STICK_RADIUS: f32 = 56.0;
const KNOB_RADIUS: f32 = 24.0;
/// Radians the view turns per logical pixel a finger drags.
const TOUCH_LOOK: f32 = 0.005;

const TOUCH_HELP: &str = "Drag to look, tap to shoot\n\
                          Thumb on the lower left: move\n\
                          Look up and push forward to rise";

/// Each finger gets a job when it lands: the button under it, the thumb stick
/// in the lower left, or else looking around. One that lifts again soon
/// without moving, anywhere but on a button, shoots where it landed.
#[derive(Resource, Default)]
struct TouchControls {
    /// A finger has touched the screen, or the browser says its main pointer
    /// is one: the help and the buttons switch to touch.
    active: bool,
    fingers: Vec<Finger>,
    /// The thumb stick, up to 1 each way: x to the right, y down the screen.
    stick: Vec2,
    /// How far looking fingers dragged this frame (logical pixels).
    look: Vec2,
}

struct Finger {
    id: u64,
    job: Job,
    /// When it landed (seconds), and whether it has since moved further than
    /// a tap may. Until it does, it neither looks nor moves.
    since: f32,
    moved: bool,
    /// Where the finger was last frame. (Bevy's `Touch::delta` repeats a
    /// finger's last move for as long as it holds still.)
    last: Vec2,
}

enum Job {
    Button,
    Stick { centre: Vec2 },
    Look,
}

/// Whether a browser's main pointer is a finger (a phone or a tablet), so that
/// the help shows touch hints from the start.
fn coarse_pointer() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.match_media("(pointer: coarse)").ok().flatten())
            .is_some_and(|query| query.matches())
    }
    #[cfg(not(target_arch = "wasm32"))]
    false
}

fn touch_input(
    touches: Res<Touches>,
    time: Res<Time<Real>>,
    camera: Query<(&Camera, &GlobalTransform), With<FlyCam>>,
    buttons: Query<(&ToolButton, &ComputedNode, &UiGlobalTransform, &InheritedVisibility)>,
    mut controls: ResMut<TouchControls>,
    mut actions: ResMut<Actions>,
) {
    let Ok((camera, eye)) = camera.single() else {
        return;
    };
    let size = camera.logical_viewport_size().unwrap_or(Vec2::ONE);
    let scale = camera.target_scaling_factor().unwrap_or(1.0);
    let now = time.elapsed_secs();
    let controls = &mut *controls;

    for touch in touches.iter_just_pressed() {
        controls.active = true;
        let at = touch.position();
        // UI nodes are laid out in physical pixels.
        let button = buttons
            .iter()
            .find(|(_, node, transform, visible)| visible.get() && node.contains_point(**transform, at * scale));
        let stick_free = !controls.fingers.iter().any(|f| matches!(f.job, Job::Stick { .. }));
        let job = if let Some((button, ..)) = button {
            press(*button, &mut actions);
            Job::Button
        } else if stick_free && at.x < size.x * 0.5 && at.y > size.y * 0.4 {
            Job::Stick { centre: at }
        } else {
            Job::Look
        };
        controls.fingers.push(Finger {
            id: touch.id(),
            job,
            since: now,
            moved: false,
            last: at,
        });
    }

    for touch in touches.iter_just_released() {
        let Some(i) = controls.fingers.iter().position(|f| f.id == touch.id()) else {
            continue;
        };
        let finger = controls.fingers.swap_remove(i);
        if !matches!(finger.job, Job::Button)
            && !finger.moved
            && touch.distance().length() <= TAP_SLOP
            && now - finger.since < TAP_TIME
            && let Ok(ray) = camera.viewport_to_world(eye, touch.start_position())
        {
            actions.shoot.push(*ray.direction);
        }
    }
    // Fingers the system took away (a cancelled touch) do nothing.
    controls.fingers.retain(|f| touches.get_pressed(f.id).is_some());

    controls.stick = Vec2::ZERO;
    controls.look = Vec2::ZERO;
    for finger in &mut controls.fingers {
        let Some(touch) = touches.get_pressed(finger.id) else {
            continue;
        };
        let at = touch.position();
        finger.moved |= touch.distance().length() > TAP_SLOP;
        if finger.moved {
            match finger.job {
                Job::Button => {}
                Job::Stick { centre } => controls.stick = ((at - centre) / STICK_RADIUS).clamp_length_max(1.0),
                Job::Look => controls.look += at - finger.last,
            }
        }
        finger.last = at;
    }
}

/// The thumb stick's ring and knob, shown while a thumb is on it.
#[derive(Component)]
enum StickPart {
    Ring,
    Knob,
}

fn setup_stick(mut commands: Commands) {
    for (part, radius, fill, edge) in [
        (
            StickPart::Ring,
            STICK_RADIUS,
            Color::srgba(0.0, 0.0, 0.0, 0.18),
            Color::srgba(1.0, 1.0, 1.0, 0.5),
        ),
        (
            StickPart::Knob,
            KNOB_RADIUS,
            Color::srgba(1.0, 1.0, 1.0, 0.45),
            Color::NONE,
        ),
    ] {
        commands.spawn((
            part,
            Node {
                position_type: PositionType::Absolute,
                width: px(2.0 * radius),
                height: px(2.0 * radius),
                border: UiRect::all(px(2)),
                border_radius: BorderRadius::MAX,
                ..default()
            },
            BackgroundColor(fill),
            BorderColor::all(edge),
            Visibility::Hidden,
        ));
    }
}

fn update_stick(controls: Res<TouchControls>, mut parts: Query<(&StickPart, &mut Node, &mut Visibility)>) {
    let centre = controls.fingers.iter().find_map(|f| match f.job {
        Job::Stick { centre } => Some(centre),
        _ => None,
    });
    for (part, mut node, mut visibility) in &mut parts {
        let Some(centre) = centre else {
            visibility.set_if_neq(Visibility::Hidden);
            continue;
        };
        let (at, radius) = match part {
            StickPart::Ring => (centre, STICK_RADIUS),
            StickPart::Knob => (centre + controls.stick * STICK_RADIUS, KNOB_RADIUS),
        };
        node.left = px(at.x - radius);
        node.top = px(at.y - radius);
        visibility.set_if_neq(Visibility::Inherited);
    }
}

/// Touch hints replace the keyboard's once a finger touches the screen.
fn update_help(controls: Res<TouchControls>, mut help: Query<&mut Text, With<Help>>, mut done: Local<bool>) {
    if controls.active && !*done {
        *done = true;
        for mut text in &mut help {
            text.0 = TOUCH_HELP.into();
        }
    }
}

// ---------------------------------------------------------------------------
// Scripted capture for automated visual checks.
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct Capture {
    dir: String,
    step: usize,
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn capture_script(
    mut commands: Commands,
    time: Res<Time>,
    target: Res<ShotTarget>,
    mut capture: ResMut<Capture>,
    mut actions: ResMut<Actions>,
    mut camera: Query<(Entity, &mut Transform, &mut FlyCam, &mut SmokeCamera, &mut Msaa)>,
    mut ui: Query<&mut Visibility, Or<(With<Hud>, With<Help>, With<Node>)>>,
    mut finish: ResMut<Finish>,
) {
    if setting("hud").is_none() {
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
                    actions.shoot.push(aim - eye);
                }
            }
            "explode" => {
                actions.explode_at = Some(Vec3::new(-0.6, 0.6, 3.6));
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
                finish.0 = true;
            }
            shot => screenshot(&mut commands, &target, &capture.dir, shot),
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
    /// Wall-clock milliseconds of each measured frame.
    frames: Vec<f64>,
    /// Per view, the median frame time without smoke.
    empty_ms: [f64; SWEEP_VIEWS.len()],
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
            frames: Vec::new(),
            empty_ms: [0.0; SWEEP_VIEWS.len()],
            timings: Timings::default(),
            csv: format!(
                "view,tier,tier_name,gpu_ms,samples,{},frame_ms,empty_frame_ms\n",
                CAMERA_PASSES.join(",")
            ),
        }
    }
}

/// Whether GPU timestamp diagnostics are on (`SMOKE_DEMO_GPU_TIMINGS`).
#[derive(Resource)]
struct GpuTimings(bool);

/// Blocks until the GPU has finished the frame (the tier sweep's frame times).
fn wait_for_gpu(device: Res<RenderDevice>) {
    let _ = device.poll(PollType::Wait {
        submission_index: None,
        timeout: None,
    });
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

const SWEEP_VIEWS: [(&str, Vec3, Vec3); 3] = [
    ("far", Vec3::new(-6.0, 2.2, -22.0), Vec3::new(0.0, 1.2, 4.6)),
    ("front", Vec3::new(-2.5, 1.7, -9.0), Vec3::new(0.0, 1.4, 5.0)),
    ("close", Vec3::new(-1.6, 1.6, -1.8), Vec3::new(0.0, 1.0, 4.6)),
];

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn tier_sweep(
    mut commands: Commands,
    target: Res<ShotTarget>,
    mut sweep: ResMut<Sweep>,
    mut camera: Query<(&mut Transform, &mut FlyCam, &mut SmokeCamera)>,
    mut ui: Query<&mut Visibility, Or<(With<Hud>, With<Help>, With<Node>)>>,
    mut virtual_time: ResMut<Time<Virtual>>,
    mut settings: ResMut<SmokeSettings>,
    diagnostics: Res<DiagnosticsStore>,
    (gpu, mut last_frame): (Res<GpuTimings>, Local<Option<Instant>>),
    world: Res<SmokeWorld>,
    clock: Res<SmokeClock>,
    mut finish: ResMut<Finish>,
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
        sweep.frames.clear();
    };
    // Wall-clock frame times too (`Time<Real>` follows the manual time step).
    // Offscreen, nothing else paces the frames, so a frame with smoke minus
    // the same view without it measures what the smoke costs, where GPU
    // timings aren't available (Metal, browsers).
    let now = Instant::now();
    let frame_ms = last_frame.map_or(0.0, |t| (now - t).as_secs_f64() * 1e3);
    *last_frame = Some(now);
    const FRAMES: usize = 60;
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
                screenshot(&mut commands, &target, &sweep.dir, &format!("{name}_empty"));
            }
            if sweep.stage_frame > 10 {
                sweep.frames.push(frame_ms);
            }
            if sweep.frames.len() == FRAMES {
                sweep.empty_ms[s - 1] = median(&sweep.frames);
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
            if sweep.stage_frame > 12 && sweep.stage_frame < 10_000 {
                sweep.frames.push(frame_ms);
                if let Some(ms) = fresh {
                    sweep.samples.push((ms, sweep.timings.passes));
                }
            }
            const WANTED: usize = 16;
            let measured = if gpu.0 {
                sweep.samples.len() == WANTED
            } else {
                sweep.frames.len() == FRAMES
            };
            if measured && sweep.stage_frame < 10_000 {
                let (gpu_ms, passes) = if gpu.0 {
                    let mut sorted = sweep.samples.clone();
                    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
                    let (median, passes) = sorted[WANTED / 2];
                    (format!("{median:.4}"), passes.map(|ms| format!("{ms:.3}")))
                } else {
                    (String::new(), Default::default())
                };
                let _ = writeln!(
                    sweep.csv,
                    "{name},{tier},{},{gpu_ms},{},{},{:.4},{:.4}",
                    quality::TIER_NAMES[tier],
                    sweep.samples.len(),
                    passes.join(","),
                    median(&sweep.frames),
                    sweep.empty_ms[k / tiers],
                );
                screenshot(&mut commands, &target, &sweep.dir, &format!("{name}_tier{tier}"));
                sweep.stage_frame = 10_000;
            }
            if sweep.stage_frame == 10_003 {
                settings.debug_silhouette = true;
            }
            if sweep.stage_frame == 10_006 {
                screenshot(&mut commands, &target, &sweep.dir, &format!("{name}_tier{tier}_mask"));
            }
            if sweep.stage_frame == 10_009 {
                settings.debug_silhouette = false;
                advance(sweep);
            }
            if sweep.stage_frame == 1500 {
                warn!("no GPU timings for {name} tier {tier}; skipping");
                let _ = writeln!(
                    sweep.csv,
                    "{name},{tier},{},,0,{},{:.4},{:.4}",
                    quality::TIER_NAMES[tier],
                    [""; CAMERA_PASSES.len()].join(","),
                    median(&sweep.frames),
                    sweep.empty_ms[k / tiers],
                );
                advance(sweep);
            }
        }
        _ => {
            if sweep.stage_frame == 1 {
                std::fs::write(format!("{}/tiers.csv", sweep.dir), &sweep.csv).ok();
                info!("tier sweep:\n{}", sweep.csv);
            }
            if sweep.stage_frame > 8 {
                finish.0 = true;
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
    target: Res<ShotTarget>,
    mut run: ResMut<BudgetRun>,
    mut camera: Query<(&mut Transform, &mut FlyCam, &mut SmokeCamera, &SmokeQuality)>,
    budget: Res<DemoBudget>,
    mut virtual_time: ResMut<Time<Virtual>>,
    diagnostics: Res<DiagnosticsStore>,
    world: Res<SmokeWorld>,
    clock: Res<SmokeClock>,
    mut finish: ResMut<Finish>,
) {
    let Ok((mut transform, mut cam, mut smoke_cam, status)) = camera.single_mut() else {
        return;
    };
    let run = run.as_mut();
    run.frame += 1;
    let f = run.frame;
    let fresh = run.timings.fresh(&diagnostics);
    let smoke_centre = Vec3::new(0.0, 1.2, 4.6);
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
    place(smoke_centre + dir * d, smoke_centre, &mut transform, &mut cam);
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
            screenshot(&mut commands, &target, &run.dir, name);
        }
    }
    if f == 1010 {
        std::fs::write(format!("{}/budget.csv", run.dir), &run.csv).ok();
    }
    if f > 1015 {
        finish.0 = true;
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
    target: Res<ShotTarget>,
    mut run: ResMut<AgreementRun>,
    mut camera: Query<(&mut Transform, &mut FlyCam, &mut SmokeCamera, &Camera, &GlobalTransform)>,
    mut ui: Query<&mut Visibility, Or<(With<Hud>, With<Help>, With<Node>)>>,
    mut virtual_time: ResMut<Time<Virtual>>,
    mut settings: ResMut<SmokeSettings>,
    mut world: ResMut<SmokeWorld>,
    clock: Res<SmokeClock>,
    level: Res<Level>,
    mut finish: ResMut<Finish>,
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
                12 => screenshot(&mut commands, &target, &run.dir, &format!("{name}_lit")),
                16 => settings.debug_silhouette = true,
                20 => screenshot(&mut commands, &target, &run.dir, &format!("{name}_gpu_mask")),
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
                finish.0 = true;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Video: every frame of short captioned clips, for the README's animations.
// ---------------------------------------------------------------------------

const VIDEO_FPS: f32 = 30.0;

/// A scripted clip: what happens, where the camera goes, what the caption says.
struct Clip {
    name: &'static str,
    style: SmokeStyle,
    /// Seconds before recording starts; events at negative times happen in it.
    pre_roll: f32,
    /// Seconds recorded.
    length: f32,
    camera: CameraPath,
    events: Vec<(f32, Event)>,
    /// From, to (seconds), text.
    captions: Vec<(f32, f32, &'static str)>,
}

enum CameraPath {
    /// Eye and look-at point at the given times, eased in between.
    Keys(Vec<(f32, Vec3, Vec3)>),
    /// Round `center`, `radius` away and at `height`, from angle `from` to
    /// `to` (radians; 0 is on the −z side) over the clip.
    Orbit {
        center: Vec3,
        radius: f32,
        height: f32,
        from: f32,
        to: f32,
    },
}

impl CameraPath {
    fn at(&self, t: f32, length: f32) -> (Vec3, Vec3) {
        let ease = |u: f32| {
            let u = u.clamp(0.0, 1.0);
            u * u * (3.0 - 2.0 * u)
        };
        match self {
            CameraPath::Keys(keys) => {
                let mut pose = (keys[0].1, keys[0].2);
                for pair in keys.windows(2) {
                    let ((t0, eye0, at0), (t1, eye1, at1)) = (pair[0], pair[1]);
                    if t >= t0 {
                        let s = ease((t - t0) / (t1 - t0));
                        pose = (eye0.lerp(eye1, s), at0.lerp(at1, s));
                    }
                }
                pose
            }
            &CameraPath::Orbit {
                center,
                radius,
                height,
                from,
                to,
            } => {
                let a = from + (to - from) * ease(t / length);
                let eye = Vec3::new(center.x + radius * a.sin(), height, center.z - radius * a.cos());
                (eye, center)
            }
        }
    }
}

#[derive(Clone)]
enum Event {
    /// A smoke pops here.
    Smoke(Smoke, Vec3),
    /// A grenade thrown from here to land there after so many seconds.
    Throw(GrenadeKind, Vec3, Vec3, f32),
    /// Bullets from the camera at these points.
    Shoot(Vec<Vec3>),
    /// A bullet from one point towards another.
    Bullet(Vec3, Vec3),
    Explode(Vec3),
    Roof(bool),
    /// Bots and their sight lines.
    Bots(bool),
}

/// The bots' eyes and the head they look at, for the sight line clip.
const BOT_EYES: [Vec3; 5] = [
    Vec3::new(12.0, 1.6, -22.5),
    Vec3::new(12.0, 1.6, -20.25),
    Vec3::new(12.0, 1.6, -18.0),
    Vec3::new(12.0, 1.6, -15.75),
    Vec3::new(12.0, 1.6, -13.5),
];
const SIGHT_TARGET: Vec3 = Vec3::new(28.0, 1.6, -18.0);

fn clips() -> Vec<Clip> {
    use SmokeStyle::{Crystal, Faceted, Volumetric};
    let doorway = Vec3::new(0.0, 0.0, 4.6);
    let volley = || {
        [(-1.5, 0.3), (-0.5, -0.2), (0.4, 0.6), (1.2, 0.0), (0.0, 1.4)]
            .map(|(dx, dy)| Vec3::new(dx, 1.2 + dy, 5.0))
            .to_vec()
    };
    let look = |name, style, caption| Clip {
        name,
        style,
        pre_roll: 3.4,
        length: 7.0,
        camera: CameraPath::Orbit {
            center: Vec3::new(0.0, 1.0, 4.6),
            radius: 9.5,
            height: 1.9,
            from: -0.5,
            to: 0.35,
        },
        events: vec![
            (-3.4, Event::Smoke(Smoke::default().with_seed(5), doorway)),
            (1.6, Event::Shoot(volley())),
            (3.8, Event::Explode(Vec3::new(-0.6, 0.6, 3.6))),
        ],
        captions: vec![(0.0, 7.0, caption)],
    };
    vec![
        Clip {
            name: "hero",
            style: Volumetric,
            pre_roll: 0.6,
            length: 15.0,
            camera: CameraPath::Keys(vec![
                (0.0, Vec3::new(-7.5, 1.6, -6.5), Vec3::new(0.0, 1.2, 4.0)),
                (7.5, Vec3::new(-3.0, 1.9, -8.5), Vec3::new(0.3, 1.3, 4.0)),
                (15.0, Vec3::new(2.0, 2.1, -8.0), Vec3::new(0.5, 1.3, 4.0)),
            ]),
            events: vec![
                (
                    0.2,
                    Event::Throw(
                        GrenadeKind::Smoke,
                        Vec3::new(-6.2, 1.3, -5.8),
                        Vec3::new(-0.3, GRENADE_RADIUS, 2.6),
                        1.4,
                    ),
                ),
                (5.0, Event::Shoot(volley())),
                (
                    8.0,
                    Event::Throw(
                        GrenadeKind::He,
                        Vec3::new(-3.5, 1.5, -5.0),
                        Vec3::new(-0.2, GRENADE_RADIUS, 1.9),
                        1.2,
                    ),
                ),
            ],
            captions: vec![
                (
                    0.0,
                    4.8,
                    "A thrown smoke grenade pops where it stops and floods the yard",
                ),
                (4.8, 8.0, "Bullets carve tunnels that close again"),
                (8.0, 12.8, "An HE grenade clears a sphere, which refills from the edges"),
            ],
        },
        look(
            "volumetric",
            Volumetric,
            "Volumetric: soft, lit by the sun, self-shadowed",
        ),
        look("faceted", Faceted, "Faceted: hard polyhedra, merged like metaballs"),
        look("crystal", Crystal, "Crystal: separate polyhedra, drawn as plain meshes"),
        Clip {
            name: "ledge",
            style: Volumetric,
            pre_roll: 0.5,
            length: 9.0,
            camera: CameraPath::Keys(vec![
                (0.0, Vec3::new(-4.0, 1.3, -8.0), Vec3::new(-13.0, 1.2, -0.5)),
                (9.0, Vec3::new(-5.0, 1.1, -6.0), Vec3::new(-13.0, 1.1, -0.5)),
            ]),
            events: vec![(
                0.0,
                Event::Smoke(
                    Smoke::default().with_grow_time(6.0).with_seed(9),
                    Vec3::new(-13.8, 1.5, -1.0),
                ),
            )],
            captions: vec![(0.0, 9.0, "It spreads like a fluid, pouring off ledges")],
        },
        Clip {
            name: "rooms",
            style: Volumetric,
            pre_roll: 0.5,
            length: 10.0,
            camera: CameraPath::Keys(vec![
                (0.0, Vec3::new(-7.6, 2.0, 13.8), Vec3::new(0.5, 1.0, 6.5)),
                (10.0, Vec3::new(-6.6, 1.8, 12.6), Vec3::new(0.5, 1.0, 6.5)),
            ]),
            events: vec![(
                0.0,
                Event::Smoke(
                    Smoke::default().with_grow_time(4.5).with_volume(140.0).with_seed(4),
                    Vec3::new(0.0, 0.0, 4.3),
                ),
            )],
            captions: vec![(
                0.0,
                10.0,
                "Indoors through the doorway, darker where the sky can't reach",
            )],
        },
        Clip {
            name: "cutaway",
            style: Volumetric,
            pre_roll: 0.5,
            length: 10.0,
            // Steep, so the 4 m front wall doesn't hide the smoke just inside
            // it; still, so that only the smoke changes from frame to frame
            // (much smaller animations).
            camera: CameraPath::Keys(vec![(0.0, Vec3::new(3.5, 32.0, -3.0), Vec3::new(3.5, 0.0, 3.5))]),
            events: vec![
                (-0.5, Event::Roof(false)),
                (
                    0.0,
                    Event::Smoke(
                        Smoke::default().with_grow_time(6.0).with_volume(160.0).with_seed(6),
                        Vec3::new(0.0, 0.0, 4.9),
                    ),
                ),
                (
                    0.8,
                    Event::Smoke(
                        Smoke::default().with_grow_time(6.0).with_seed(8),
                        Vec3::new(12.6, 0.0, -3.0),
                    ),
                ),
            ],
            captions: vec![(
                0.0,
                10.0,
                "The fill follows the level, round walls and down corridors (roof hidden)",
            )],
        },
        Clip {
            name: "sightlines",
            style: Volumetric,
            pre_roll: 0.5,
            length: 12.0,
            camera: CameraPath::Keys(vec![(0.0, Vec3::new(19.0, 10.0, -32.5), Vec3::new(20.0, 0.6, -18.0))]),
            events: vec![
                (-0.5, Event::Bots(true)),
                (
                    0.8,
                    Event::Smoke(Smoke::default().with_seed(2), Vec3::new(20.0, 0.0, -18.0)),
                ),
                (6.5, Event::Bullet(BOT_EYES[2], SIGHT_TARGET)),
                (9.0, Event::Bullet(BOT_EYES[3], SIGHT_TARGET)),
            ],
            captions: vec![
                (
                    0.0,
                    6.3,
                    "Gameplay sees the same smoke: blocks_sight() for each bot, red when hidden",
                ),
                (6.3, 12.0, "A bullet's tunnel opens a line of sight until it closes"),
            ],
        },
    ]
}

#[derive(Resource)]
struct Video {
    dir: String,
    clips: Vec<Clip>,
    clip: usize,
    /// Frames since the current clip's pre-roll began.
    frame: u32,
    /// Frames since the start, while every style's pipelines warm up.
    warmup: u32,
    caption_size: f32,
}

impl Video {
    fn new(dir: String, size: UVec2) -> Self {
        let only = setting("clips");
        let clips = clips()
            .into_iter()
            .filter(|c| only.as_deref().is_none_or(|o| o.split(',').any(|n| n == c.name)))
            .collect();
        Self {
            dir,
            clips,
            clip: 0,
            frame: 0,
            warmup: 0,
            caption_size: size.y as f32 * 0.034,
        }
    }
}

/// The caption box and its text.
#[derive(Component)]
struct Caption;

/// The sight line clip's bots and their target.
#[derive(Component)]
struct Bot;

/// Whether to draw the bots' sight lines.
#[derive(Resource, Default)]
struct Sightlines(bool);

fn setup_bots(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut gizmos: ResMut<GizmoConfigStore>,
    video: Res<Video>,
) {
    let (config, _) = gizmos.config_mut::<DefaultGizmoConfigGroup>();
    config.line.width = (video.caption_size * 0.15).max(2.0);
    let body = meshes.add(no_uvs(Capsule3d::new(0.25, 1.1)));
    let blue = materials.add(Color::srgb(0.3, 0.4, 0.62));
    let orange = materials.add(Color::srgb(0.92, 0.45, 0.12));
    for (eye, material) in BOT_EYES.iter().map(|e| (*e, &blue)).chain([(SIGHT_TARGET, &orange)]) {
        commands.spawn((
            Mesh3d(body.clone()),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(eye - Vec3::Y * 0.8),
            Visibility::Hidden,
            Bot,
        ));
    }
    let pad = video.caption_size * 0.5;
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(pad * 1.5),
                bottom: px(pad * 1.5),
                padding: UiRect::axes(px(pad), px(pad * 0.6)),
                border_radius: BorderRadius::all(px(pad * 0.5)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.04, 0.05, 0.07, 0.72)),
            Visibility::Hidden,
            Caption,
        ))
        .with_child((
            Text::new(""),
            TextFont {
                font_size: FontSize::Px(video.caption_size),
                ..default()
            },
            Caption,
        ));
}

/// Green while a bot can see the target's head, red once smoke hides it.
fn draw_sightlines(sightlines: Res<Sightlines>, mut vision: SmokeVision, mut gizmos: Gizmos) {
    if !sightlines.0 {
        return;
    }
    for eye in BOT_EYES {
        let color = if vision.blocks_sight(eye, SIGHT_TARGET) {
            Color::srgb(0.95, 0.18, 0.12)
        } else {
            Color::srgb(0.25, 0.9, 0.35)
        };
        gizmos.line(eye, SIGHT_TARGET, color);
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn video_script(
    mut commands: Commands,
    target: Res<ShotTarget>,
    mut video: ResMut<Video>,
    mut actions: ResMut<Actions>,
    mut settings: ResMut<SmokeSettings>,
    mut sightlines: ResMut<Sightlines>,
    mut camera: Query<(&mut Transform, &mut FlyCam, &mut SmokeCamera)>,
    mut ui: Query<&mut Visibility, (With<Node>, Without<Caption>)>,
    mut caption: Query<(&mut Visibility, Option<&mut Text>), With<Caption>>,
    mut props: Query<(&mut Visibility, Has<Roof>), (Or<(With<Roof>, With<Bot>)>, Without<Node>, Without<Caption>)>,
    things: Query<Entity, Clearable>,
    (grenades, level, mut bullets, mut effects): (
        Res<GrenadeAssets>,
        Res<Level>,
        MessageWriter<SmokeBullet>,
        ResMut<Effects>,
    ),
    mut finish: ResMut<Finish>,
) {
    let Ok((mut transform, mut cam, mut smoke_cam)) = camera.single_mut() else {
        return;
    };
    for mut v in &mut ui {
        *v = Visibility::Hidden;
    }
    let video = video.as_mut();
    // Warm up: draw every style once, so no pipeline compiles mid-clip.
    if video.warmup < 90 {
        video.warmup += 1;
        if video.warmup == 1 {
            smoke_cam.quality = 1.0;
            place(
                Vec3::new(-2.5, 1.7, -9.0),
                Vec3::new(0.0, 1.4, 5.0),
                &mut transform,
                &mut cam,
            );
            for (x, style) in [
                (-4.0, SmokeStyle::Volumetric),
                (0.0, SmokeStyle::Faceted),
                (4.0, SmokeStyle::Crystal),
            ] {
                commands.spawn((Smoke::default().with_style(style), Transform::from_xyz(x, 0.0, 2.0)));
            }
        } else if video.warmup == 80 {
            actions.clear = true;
        }
        return;
    }
    let Some(clip) = video.clips.get(video.clip) else {
        finish.0 = true;
        return;
    };
    let pre = (clip.pre_roll * VIDEO_FPS).round() as u32;
    let len = (clip.length * VIDEO_FPS).round() as u32;
    let f = video.frame;
    let t = (f as f32 - pre as f32) / VIDEO_FPS;
    if f == 0 {
        // A clean slate, before this frame's events: despawned here rather
        // than through `Actions::clear`, which would also catch the smokes
        // those events spawn.
        for entity in &things {
            commands.entity(entity).despawn();
        }
        settings.style = clip.style;
        sightlines.0 = false;
        for (mut v, roof) in &mut props {
            *v = if roof {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
        }
    }
    let (eye, at) = clip.camera.at(t.max(0.0), clip.length);
    place(eye, at, &mut transform, &mut cam);

    for (time, event) in &clip.events {
        if ((time + clip.pre_roll) * VIDEO_FPS).round() as u32 != f {
            continue;
        }
        match event.clone() {
            Event::Smoke(smoke, p) => {
                commands.spawn((smoke, Transform::from_translation(p)));
            }
            Event::Throw(kind, from, to, seconds) => {
                throw(&mut commands, &grenades, kind, from, lob(from, to, seconds));
            }
            Event::Shoot(points) => actions.shoot.extend(points.iter().map(|p| *p - eye)),
            Event::Bullet(from, to) => shoot(&mut commands, &level, &mut bullets, &mut effects, from, to - from),
            Event::Explode(p) => actions.explode_at = Some(p),
            Event::Roof(on) => {
                for (mut v, roof) in &mut props {
                    if roof {
                        *v = if on { Visibility::Inherited } else { Visibility::Hidden };
                    }
                }
            }
            Event::Bots(on) => {
                sightlines.0 = on;
                for (mut v, roof) in &mut props {
                    if !roof {
                        *v = if on { Visibility::Inherited } else { Visibility::Hidden };
                    }
                }
            }
        }
    }

    let text = clip
        .captions
        .iter()
        .find(|(from, to, _)| t >= *from && t < *to)
        .map_or("", |c| c.2);
    for (mut visibility, label) in &mut caption {
        match label {
            Some(mut label) if label.0 != text => label.0 = text.into(),
            Some(_) => {}
            None => {
                *visibility = if text.is_empty() {
                    Visibility::Hidden
                } else {
                    Visibility::Inherited
                }
            }
        }
    }

    if f >= pre {
        let dir = format!("{}/{}", video.dir, clip.name);
        if f == pre {
            std::fs::create_dir_all(&dir).ok();
        }
        screenshot(&mut commands, &target, &dir, &format!("{:05}", f - pre));
    }
    video.frame += 1;
    if video.frame >= pre + len {
        video.clip += 1;
        video.frame = 0;
    }
}
