// Types and helpers shared by the smoke shaders. Layouts must match the
// `Gpu*` structs in `render.rs`.

const MAX_SLOTS: u32 = 32u;
const MAX_HOLES: u32 = 512u;
const MAX_LIGHTS: u32 = 8u;
const PI: f32 = 3.14159265358979;

struct SmokeSlot {
    // xyz: world-space min corner of the voxel grid, w: voxel size (m)
    grid_min: vec4<f32>,
    // xyz: draw bounds min, w: atlas layer
    bounds_min: vec4<f32>,
    // xyz: draw bounds max, w: dissolve 0..1
    bounds_max: vec4<f32>,
    // x: growth front, y: density scale, z: extra erosion, w: noise seed offset
    params: vec4<f32>,
    // rgb: albedo tint
    tint: vec4<f32>,
    // x: first hole, y: hole count, z: style (1 = faceted)
    holes: vec4<u32>,
    // Faceted only. xyz: puff bins min corner, w: bin cell size (m)
    facet_grid: vec4<f32>,
    // xyz: bins per axis, w: first cell in the cell buffer
    facet_dims: vec4<u32>,
    // x: first index in the index buffer, y: first puff, z: puff count
    facet_ofs: vec4<u32>,
    // x: smooth-union radius (m), y: region clip padding (m)
    facet_shape: vec4<f32>,
    // xyz: what the smoke occupies at this tick (`CloudState::active`): a
    // sample outside reads nothing, and a voxel whose centre is outside
    // holds nothing
    drawn_min: vec4<f32>,
    drawn_max: vec4<f32>,
}

const STYLE_FACETED: u32 = 1u;

struct SmokeHole {
    // xyz: capsule start, w: radius
    a: vec4<f32>,
    // xyz: capsule end, w: strength
    b: vec4<f32>,
}

struct SmokeLight {
    // xyz: position, w: range
    position_range: vec4<f32>,
    // rgb: luminous intensity (cd) times colour
    color: vec4<f32>,
}

struct SmokeGlobals {
    slots: array<SmokeSlot, MAX_SLOTS>,
    holes: array<SmokeHole, MAX_HOLES>,
    lights: array<SmokeLight, MAX_LIGHTS>,
    // Indices into `slots` to bake this frame, four per vector.
    bake_slots: array<vec4<u32>, 8>,
    // xyz: voxels per slot, w: atlas layers
    grid: vec4<u32>,
    // x: active slots, y: lights, z: slots to bake, w: frame
    counts: vec4<u32>,
    // xyz: unit vector towards the sun, w: 1 if a sun exists
    sun_direction: vec4<f32>,
    // rgb: sun illuminance (lux) times colour
    sun_color: vec4<f32>,
    // rgb: ambient radiance
    ambient: vec4<f32>,
    // rgb: scattering albedo, w: extinction (1/m) at density 1
    medium: vec4<f32>,
    // x: noise frequency (1/m), y: erosion, z: detail erosion, w: time (s)
    noise: vec4<f32>,
    // xyz: noise drift (m/s), w: phase anisotropy
    wind: vec4<f32>,
    // x: multi-scattering, y: indoor ambient, z: ambient occlusion, w: voxel size
    lighting: vec4<f32>,
    // For `SmokeSettings::noise_within_rule`. NOISE_CAPPED, x: how much cloud
    // (m at full density) the noise may take from a line of sight.
    // NOISE_FILLED: from y to z metres of cloud the opacity the noise took is
    // filled back in.
    fairness: vec4<f32>,
}

struct SmokeView {
    world_from_clip: mat4x4<f32>,
    // xyz: camera position, w: near plane distance
    camera_position: vec4<f32>,
    // xy: viewport size in pixels, zw: smoke buffer size
    target_size: vec4<f32>,
    // xy: viewport origin in the render target
    viewport: vec4<f32>,
    // x: exposure, y: step size (m), z: step growth per metre, w: resolution divisor
    march: vec4<f32>,
    // smoke-buffer rectangle to march: min.xy, max.xy (exclusive)
    rect: vec4<u32>,
    // x: max steps, y: jitter frame, z: the slot a SINGLE_SMOKE march reads
    params: vec4<u32>,
    // Faceted. x: coarse divisor, y: max steps, z: shadow steps, w: flags
    // (one of them for the volumetric composite)
    facet: vec4<u32>,
    // Faceted. x: hit tolerance (pixels), y: angle of one pixel (radians),
    // z: shading tolerance
    facet_f: vec4<f32>,
    // Faceted coarse rectangle: min.xy, max.xy (exclusive), coarse pixels
    facet_rect: vec4<u32>,
    // Faceted rectangle: min.xy, max.xy (exclusive), viewport pixels
    facet_px: vec4<u32>,
}

const FLAG_TRACED_SHADOWS: u32 = 1u;
const FLAG_TRACED_OCCLUSION: u32 = 2u;
const FLAG_DEBUG_SILHOUETTE: u32 = 4u;
const FLAG_DEBUG_TRANSMITTANCE: u32 = 8u;

// smoothstep with a well-defined result for any edge order.
fn sstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn remap(v: f32, lo: f32, hi: f32, new_lo: f32, new_hi: f32) -> f32 {
    return new_lo + (v - lo) / max(hi - lo, 1e-5) * (new_hi - new_lo);
}

// Coarse density of a voxel with normalised fill distance `d` (1 = empty).
// Mirrors `sim::shape_density`.
fn shape_density(d: f32, growth: f32, dissolve: f32) -> f32 {
    if (d >= 0.999) {
        return 0.0;
    }
    let shell = 1.0 - sstep(0.55, 1.0, d);
    let front = 1.0 - sstep(growth - 0.25, growth, d);
    let k = 1.2 * (1.0 - dissolve);
    let keep = 1.0 - sstep(k - 0.3, k, d);
    return shell * front * keep * (1.0 - 0.35 * dissolve);
}

fn distance_to_segment(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>) -> f32 {
    let ab = b - a;
    let len2 = dot(ab, ab);
    var t = 0.0;
    if (len2 > 1e-12) {
        t = clamp(dot(p - a, ab) / len2, 0.0, 1.0);
    }
    return distance(p, a + ab * t);
}

// Henyey-Greenstein phase function.
fn hg_phase(cos_theta: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denom = max(1.0 + g2 - 2.0 * g * cos_theta, 1e-4);
    return (1.0 - g2) / (4.0 * PI * denom * sqrt(denom));
}

// Entry and exit distances of a ray through an AABB (exit < entry = miss).
fn ray_aabb(ro: vec3<f32>, inv_rd: vec3<f32>, bmin: vec3<f32>, bmax: vec3<f32>) -> vec2<f32> {
    let t0 = (bmin - ro) * inv_rd;
    let t1 = (bmax - ro) * inv_rd;
    let lo = min(t0, t1);
    let hi = max(t0, t1);
    return vec2<f32>(max(max(lo.x, lo.y), lo.z), min(min(hi.x, hi.y), hi.z));
}

// Jimenez's interleaved gradient noise, optionally animated per frame.
fn interleaved_gradient_noise(p: vec2<f32>, frame: u32) -> f32 {
    let q = p + 5.588238 * f32(frame % 64u);
    return fract(52.9829189 * fract(0.06711056 * q.x + 0.00583715 * q.y));
}
