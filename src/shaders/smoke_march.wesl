// Reduced-resolution raymarch through all smokes in view.
//
// Per pixel: intersect the view ray with every smoke's bounds (cheap ALU),
// then march front to back only inside those intervals. Each step costs one
// fetch of the baked volume (density + lighting), one of the open mask and
// one or two fetches of the tiling detail noise (with NOISE_CAPPED, only
// until the ray's erosion budget is spent; below). Rays stop at scene depth,
// once nearly opaque (`OPAQUE_BELOW`), or at the step budget. The common
// case of a ray crossing a single smoke takes a fast path with that smoke's
// data hoisted out of the loop.
//
// Compile-time variants make the march cheaper still, without changing a
// sample:
// - SINGLE_SMOKE: the dispatch marches one smoke, the slot in
//   `view.params.z`, over its own screen rectangle. The renderer dispatches
//   it for a smoke whose rectangle no other smoke's comes near, so no ray
//   can meet two smokes' boxes and every ray takes the fast path. The kernel
//   then needs none of the per-ray interval arrays, and the slot is the same
//   for every invocation, so its data can stay out of the per-ray registers.
//   On an Apple M4 that made the march 27-32 % cheaper close up at the best
//   dial step.
// - RAY_SMOKES_8: room in the per-ray interval arrays for 8 smokes instead
//   of every slot, where the renderer has worked out that no ray meets more.
//   The overlapping path reads them at every sample: on the M4 the short
//   arrays made two overlapping smokes 32-39 % cheaper to draw close up at
//   the top four dial steps.
// - NO_POINT_LIGHTS: the point lights compiled out, for smokes no light
//   reaches (a light out of range adds exactly nothing). Unused, their code
//   still cost registers: 8 % close up once the arrays were gone, nothing
//   while the long ones were there, and 4-11 % with the short ones and two
//   overlapping smokes.
// The march is short of registers rather than of arithmetic: removing a
// third of the sample loop's instructions (constants folded per smoke,
// coordinates advanced along the ray) made it 6-7 % slower on the M4.
//
// The samples of a ray lie on a sequence fixed by the ray alone:
// t_k = u0·r^k − 1/g with u0 = jitter·b + 1/g and r = 1 + b·g (b the step
// near the camera, g its growth per metre), which is the recurrence
// dt = b·(1 + t·g) started at the camera instead of at a smoke's box. The
// first sample at or past any distance is then a log away, so a ray starts
// where it enters the box the smoke occupies at this tick, and no sample
// moves as that box changes tick by tick. Skipped samples count against
// the step budget as they did, so a tier reaches as far as it did.
//
// A ray stops between two samples: at the scene, or where it leaves the box
// its smoke occupies. Its last sample stands for the stretch up to half a
// step past that point, the same for every ray. Each sample used to stand
// for a whole step, so the last one reached anywhere from nothing to a whole
// step past a surface inside the smoke, depending on the ray's jitter, and
// the jitter's pattern showed on such surfaces in bands a smoke texel wide
// (4 pixels at a quarter resolution). Half a step is what that came to on
// average. Stopping exactly at the surface instead let a few pixels the rule
// blocks show through at the cheapest steps with `noise_within_rule` set: at
// a quarter resolution a texel's ray stops at one pixel's depth, and the
// overshoot covered the deeper surfaces of the pixels around it.
//
// Skipping the samples inside that box that read nothing (the 4³ bricks the
// rule skips) was tried twice on an Apple M4, dropping their fetches and
// jumping them outright, and cost 5–15 % close up and at mid-range both
// times: the lookups cost more than the iterations they save, and a lane
// that skips saves nothing while the rest of its SIMD group doesn't.
//
// The noise only ever thins the smoke, so it can open lines the rule blocks.
// `SmokeSettings::noise_within_rule` keeps it from doing so in one of two
// ways, each compiled in only when chosen: their bookkeeping cost 9–14 %
// close up at the top dial steps on an Apple M4 even where it changed
// nothing.
// - NOISE_CAPPED: the noise may thin a ray only by so much, the cloud the
//   rule leaves spare: `sight_threshold` less the ln(100) / extinction the
//   march needs to draw a line opaque (0.8 − 0.66 = 0.14 m by default),
//   shared front to back by every smoke the ray crosses. Once a ray has spent
//   it, its samples are drawn as the rule counts them, without the noise. No
//   march that goes front to back can keep more: the noise never adds smoke,
//   so once more than the budget is gone, a line that turns out to hold the
//   threshold's worth of cloud can't be drawn opaque any more.
// - NOISE_FILLED: the noise runs freely, and the march also adds up the cloud
//   the rule counts along the ray, with the light it would scatter. At the end,
//   from ln(20) / extinction of that cloud, where the march draws a line
//   opaque, to `sight_threshold`, the opacity the noise took is filled back in
//   behind what was drawn.
// Either way, what an empty-space jump stepped over (at most a ramp from
// nothing up to the sample it lands on) counts as cloud the noise took.

import package::shaders::smoke_common::{
    SmokeGlobals, SmokeSlot, SmokeView, MAX_SLOTS, PI, STYLE_FACETED,
    hg_phase, ray_aabb, interleaved_gradient_noise, remap, sstep
};

// A ray ends once less than this share of the scene shows through it, and is
// drawn opaque: the light it would still have gathered is taken to be like
// what it has gathered so far. It stops at 5 %, so a line through
// ln(20) / extinction of the cloud as drawn (0.43 m by default) is opaque:
// that only adds smoke, changes a pixel by at most 6 levels out of 255, and
// made the best dial step 8–9 % cheaper close up on an Apple M4 while a cloud
// is grown or blooming. NOISE_FILLED fills in from there (`render.rs`), which
// also closed its leak at the two cheapest dial steps. NOISE_CAPPED goes on
// to 1 %, as its budget is worked out from ln(100) / extinction: at 5 % the
// larger budget kept rays reading the noise for longer, 15 % slower close up,
// and a few blocked pixels showed through at the cheapest steps. Skipping the
// noise once a ray is nearly opaque instead cost 6–8 % more: a SIMD group
// keeps the pace of its rays through thin smoke, which never get there, and
// the test in every sample costs on its own.
@if(NOISE_CAPPED)
const OPAQUE_BELOW: f32 = 0.01;
@else
const OPAQUE_BELOW: f32 = 0.05;

// Room for the smokes one ray meets, in the march that adds them up.
@if(RAY_SMOKES_8)
const RAY_SMOKES: u32 = 8u;
@else
const RAY_SMOKES: u32 = MAX_SLOTS;

@group(0) @binding(0) var<uniform> globals: SmokeGlobals;
@group(0) @binding(1) var<uniform> view: SmokeView;
@if(MULTISAMPLED)
@group(0) @binding(2) var depth_texture: texture_depth_multisampled_2d;
@else
@group(0) @binding(2) var depth_texture: texture_depth_2d;
@group(0) @binding(3) var lit_atlas: texture_3d<f32>;
@group(0) @binding(4) var linear_clamp: sampler;
@group(0) @binding(5) var noise_texture: texture_3d<f32>;
@group(0) @binding(6) var linear_repeat: sampler;
@group(0) @binding(7) var smoke_out: texture_storage_2d<rgba16float, write>;
// 1 where the level is open, 0 where it is solid, per voxel.
@group(0) @binding(8) var open_atlas: texture_3d<f32>;
// Where each ray last saw smoke through more than a fifth of the scene, on
// frames the camera measures (`view.params.w`): `smoke_depth.wesl` counts it
// for the resolution the march runs at.
@group(0) @binding(9) var depth_out: texture_storage_2d<r32float, write>;

// A ray's sample sequence (see the top of the file). Without growth it is
// arithmetic: t_k = t_0 + k·b. Between jumps a ray steps by the recurrence
// t += b·(1 + t·g), which is the same sequence to within rounding; a jump
// lands on the closed form.
struct Seq {
    u0: f32,
    ln_r: f32,
    inv_g: f32,
    b: f32,
    geometric: bool,
}

fn seq_new(jitter: f32, b: f32, g: f32) -> Seq {
    var s: Seq;
    s.b = b;
    s.geometric = g > 1e-4;
    if (s.geometric) {
        s.inv_g = 1.0 / g;
        s.u0 = jitter * b + s.inv_g;
        s.ln_r = log(1.0 + b * g);
    } else {
        s.inv_g = 0.0;
        s.u0 = jitter * b;
        s.ln_r = 0.0;
    }
    return s;
}

// Where sample k lies.
fn seq_t(s: Seq, k: u32) -> f32 {
    if (s.geometric) {
        return s.u0 * exp(f32(k) * s.ln_r) - s.inv_g;
    }
    return s.u0 + f32(k) * s.b;
}

// The first sample at or past t (the first of all for anything before it).
fn seq_first(s: Seq, t: f32) -> u32 {
    var k: f32;
    if (s.geometric) {
        k = ceil(log(max(t + s.inv_g, s.u0) / s.u0) / s.ln_r);
    } else {
        k = ceil((t - s.u0) / s.b);
    }
    return u32(max(k, 0.0));
}

// Atlas coordinates of a world position, kept inside the slot's layer.
fn atlas_uvw(slot: SmokeSlot, p: vec3<f32>) -> vec3<f32> {
    let dims = vec3<f32>(globals.grid.xyz);
    var uvw = (p - slot.grid_min.xyz) / (slot.grid_min.w * dims);
    let half_texel = 0.5 / dims.z;
    uvw.z = (clamp(uvw.z, half_texel, 1.0 - half_texel) + slot.bounds_min.w) / f32(globals.grid.w);
    return uvw;
}

// Baked (density, sun single, sun multi, ambient) at a world position.
fn sample_lit(slot: SmokeSlot, p: vec3<f32>) -> vec4<f32> {
    return textureSampleLevel(lit_atlas, linear_clamp, atlas_uvw(slot, p), 0.0);
}

// Filtered open mask: the share of the voxels a sample reads that aren't
// solid. Solid voxels hold no smoke, so dividing filtered density by this
// averages the open voxels only, as the core does: smoke pressed against a
// wall or floor stays as dense as the voxels in front of it.
fn sample_open(slot: SmokeSlot, p: vec3<f32>) -> f32 {
    return textureSampleLevel(open_atlas, linear_clamp, atlas_uvw(slot, p), 0.0).r;
}

// Carve the coarse voxel density into billows and wisps.
fn detail_density(base: f32, p: vec3<f32>, slot: SmokeSlot) -> f32 {
    let time = globals.noise.w;
    let freq = globals.noise.x;
    let seed = vec3<f32>(slot.params.w, slot.params.w * 0.37, slot.params.w * 0.71);
    let q = (p - globals.wind.xyz * time) * freq + seed;
    let n = textureSampleLevel(noise_texture, linear_repeat, q, 0.0);

    // Billows: the thinner the coarse density, the deeper the noise cuts.
    let erosion = clamp(globals.noise.y + slot.params.z, 0.0, 0.95);
    let d = clamp(remap(base, (1.0 - n.r) * erosion, 1.0, 0.0, 1.0), 0.0, 1.0);
    if (d <= 0.0 || d >= 0.98) {
        return d;
    }
    // Fine wisps on the thin parts only, scrolled differently and swirled by
    // the low-frequency channel so the edges churn.
    let swirl = (n.a - 0.5) * 0.8;
    let q2 = q * 2.7 + vec3<f32>(swirl, -time * freq * 0.45, -swirl);
    let fine = textureSampleLevel(noise_texture, linear_repeat, q2, 0.0);
    let fray = (fine.g * 0.6 + fine.b * 0.4) * globals.noise.z * (1.0 - d);
    return clamp(remap(d, fray, 1.0, 0.0, 1.0), 0.0, 1.0);
}

// Point lights (muzzle flashes, explosions, lamps). Shadowing by the smoke
// itself is estimated from one coarse-density sample halfway to the light,
// so a flash inside the cloud lights a glowing pocket rather than all of it.
fn point_lights(slot: SmokeSlot, p: vec3<f32>, rd: vec3<f32>) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    let g = globals.wind.w;
    let extinction = globals.medium.w * slot.params.y;
    for (var i = 0u; i < globals.counts.y; i++) {
        let light = globals.lights[i];
        let to_light = light.position_range.xyz - p;
        let d2 = dot(to_light, to_light);
        let r2 = light.position_range.w * light.position_range.w;
        if (d2 >= r2) {
            continue;
        }
        // Bevy's windowed inverse-square falloff.
        let f = d2 / r2;
        let window = clamp(1.0 - f * f, 0.0, 1.0);
        let attenuation = window * window / max(d2, 0.01);
        let dist = sqrt(d2);
        let between = sample_lit(slot, p + to_light * 0.5).r;
        let shadow = exp(-extinction * between * dist * 0.75);
        let cos_theta = dot(rd, to_light / max(dist, 1e-4));
        let phase = mix(hg_phase(cos_theta, g), 1.0 / (4.0 * PI), 0.5);
        sum += light.color.rgb * attenuation * shadow * phase;
    }
    return sum;
}

struct StepSample {
    sigma: f32,
    source: vec3<f32>,
    coarse: f32,
}

// Extinction and in-scattered light of one smoke at one point, over a step
// of `dt`. `skipped` is how far before here a jump stepped over samples
// without reading them. With NOISE_CAPPED, `budget` is how much more cloud
// the noise may take from this ray; with NOISE_FILLED, `rule` adds up the
// cloud the rule counts and the light it would scatter (see the top of the
// file). Otherwise they're unused.
fn sample_smoke(
    slot: SmokeSlot,
    p: vec3<f32>,
    dt: f32,
    skipped: f32,
    budget: ptr<function, f32>,
    rule: ptr<function, vec4<f32>>,
    rd: vec3<f32>,
    phase_sun: f32,
    multi_weight: f32,
) -> StepSample {
    var out: StepSample;
    let lit = sample_lit(slot, p);
    out.coarse = lit.r;
    if (lit.r <= 0.002) {
        return out;
    }
    // Solid voxels hold no smoke: average the open ones only. Smoke against
    // a wall is then as dense as the smoke in front of it, so the noise
    // doesn't fray a see-through layer along the wall either.
    let open = sample_open(slot, p);
    let base = min(lit.r / max(open, 0.004), 1.0);
    @if(NOISE_FILLED) {
        let seen = globals.sun_color.rgb * (lit.g * phase_sun + lit.b * multi_weight)
            + globals.ambient.rgb * lit.a;
        *rule += vec4<f32>(globals.medium.rgb * slot.tint.rgb * seen, 1.0) * (base * (dt + skipped));
    }
    var d = base;
    @if(NOISE_CAPPED) {
        // A jump that lands in smoke may have stepped over the start of the
        // cloud's edge: count what it skipped (at most a ramp from nothing
        // to here) as taken by the noise, which leaves it undrawn too.
        *budget = max(*budget - base * skipped, 0.0);
        // The noise only ever thins the smoke. It takes what it would from
        // this step while the ray's budget lasts, and the last of the budget
        // once it would take more; after that the smoke is drawn as the rule
        // counts it.
        if (*budget > 0.0) {
            let noisy = detail_density(base, p, slot);
            let taken = (base - noisy) * dt;
            if (taken < *budget) {
                d = noisy;
                *budget -= taken;
            } else {
                d = base - *budget / dt;
                *budget = 0.0;
            }
        }
    }
    @else {
        d = detail_density(base, p, slot);
    }
    if (d <= 0.0) {
        return out;
    }
    let sigma = d * globals.medium.w * slot.params.y;
    var light = globals.sun_color.rgb * (lit.g * phase_sun + lit.b * multi_weight)
        + globals.ambient.rgb * lit.a;
    @if(!NO_POINT_LIGHTS) {
        if (globals.counts.y > 0u) {
            light += point_lights(slot, p, rd);
        }
    }
    out.sigma = sigma;
    out.source = sigma * globals.medium.rgb * slot.tint.rgb * light;
    return out;
}

@compute @workgroup_size(8, 8, 1)
fn march(@builtin(global_invocation_id) gid: vec3<u32>) {
    let px = view.rect.xy + gid.xy;
    if (px.x >= view.rect.z || px.y >= view.rect.w) {
        return;
    }
    let divisor = view.march.w;
    let full_size = vec2<u32>(view.target_size.xy);
    // The full-resolution texel this smoke texel stands for. The composite
    // uses the same texel's depth for its depth-aware upsample.
    let full_px = min(vec2<u32>(floor(vec2<f32>(px) * divisor)), full_size - vec2<u32>(1u));
    let target_px = vec2<i32>(full_px) + vec2<i32>(view.viewport.xy);
    let raw_depth = textureLoad(depth_texture, target_px, 0);

    let uv = (vec2<f32>(full_px) + 0.5) / vec2<f32>(full_size);
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let ro = view.camera_position.xyz;
    let near_point = view.world_from_clip * vec4<f32>(ndc, 1.0, 1.0);
    let rd = normalize(near_point.xyz / near_point.w - ro);
    var t_scene = 1e30;
    if (raw_depth > 0.0) {
        let scene = view.world_from_clip * vec4<f32>(ndc, raw_depth, 1.0);
        t_scene = dot(scene.xyz / scene.w - ro, rd);
    }

    // Which smokes does this ray pass through, and where?
    // Avoid infinities for axis-aligned rays in the slab tests.
    let inv_rd = 1.0 / select(rd, vec3<f32>(1e-6), abs(rd) < vec3<f32>(1e-6));
    @if(!SINGLE_SMOKE) var t0s: array<f32, RAY_SMOKES>;
    @if(!SINGLE_SMOKE) var t1s: array<f32, RAY_SMOKES>;
    @if(!SINGLE_SMOKE) var ids: array<u32, RAY_SMOKES>;
    var count = 0u;
    var t_begin = 1e30;
    var t_end = 0.0;
    @if(SINGLE_SMOKE) {
        // This dispatch's smoke, the only one any of its rays can meet.
        let s = globals.slots[view.params.z];
        let hit = ray_aabb(ro, inv_rd, s.bounds_min.xyz, s.bounds_max.xyz);
        let a = max(hit.x, 0.0);
        let b = min(hit.y, t_scene);
        if (b > a) {
            count = 1u;
            t_begin = a;
            t_end = b;
        }
    }
    @else {
        for (var i = 0u; i < globals.counts.x; i++) {
            let s = globals.slots[i];
            if (s.holes.z == STYLE_FACETED) {
                continue;
            }
            let hit = ray_aabb(ro, inv_rd, s.bounds_min.xyz, s.bounds_max.xyz);
            let a = max(hit.x, 0.0);
            let b = min(hit.y, t_scene);
            // The renderer keeps every ray within RAY_SMOKES smokes.
            if (b > a && count < RAY_SMOKES) {
                t0s[count] = a;
                t1s[count] = b;
                ids[count] = i;
                count += 1u;
                t_begin = min(t_begin, a);
                t_end = max(t_end, b);
            }
        }
    }
    if (count == 0u) {
        textureStore(smoke_out, px, vec4<f32>(0.0, 0.0, 0.0, 1.0));
        if (view.params.w != 0u) {
            textureStore(depth_out, px, vec4<f32>(0.0));
        }
        return;
    }

    let cos_sun = dot(rd, globals.sun_direction.xyz);
    // Forward lobe plus a weak back lobe for the single-scattered sun.
    let phase_sun = mix(hg_phase(cos_sun, globals.wind.w), hg_phase(cos_sun, -0.3), 0.2);
    // Multiple scattering is close to isotropic.
    let multi_weight = globals.lighting.x * 2.0 / (4.0 * PI);
    let voxel_size = globals.lighting.w;

    let jitter = interleaved_gradient_noise(vec2<f32>(px), view.params.y);
    let base_step = view.march.y;
    let growth = view.march.z;
    let max_steps = view.params.x;
    let seq = seq_new(jitter, base_step, growth);

    var transmittance = 1.0;
    var radiance = vec3<f32>(0.0);
    // Samples consumed, jumped ones included.
    var steps = 0u;
    // How much more cloud (metres of it at full density) the noise may take
    // from this ray, front to back, every smoke along it together (see the
    // top of the file).
    var budget = globals.fairness.x;
    // The cloud the rule counts along this ray (w) and the light it would
    // scatter (rgb, times the cloud).
    var rule = vec4<f32>(0.0);
    // Half the stretch the last jump stepped over, for the sample it lands on.
    var skipped = 0.0;
    // The end of the last step that held smoke while more than a fifth of the
    // scene still showed through. Carrying it cost nothing measurable on an
    // Apple M4; it is written out only on frames the camera measures.
    var seen = 0.0;

    // With SINGLE_SMOKE every ray that gets here meets exactly one smoke.
    @if(SINGLE_SMOKE) let one = true;
    @else let one = count == 1u;
    if (one) {
        // Fast path: one smoke along this ray, from where it enters the box
        // the smoke occupies at this tick to where it leaves it: a sample
        // outside reads nothing.
        @if(SINGLE_SMOKE) let slot = globals.slots[view.params.z];
        @else let slot = globals.slots[ids[0]];
        let drawn = ray_aabb(ro, inv_rd, slot.drawn_min.xyz, slot.drawn_max.xyz);
        let t_stop = min(drawn.y, t_end);
        var k = seq_first(seq, max(drawn.x, t_begin));
        var t = seq_t(seq, k);
        if (t_stop <= t) {
            textureStore(smoke_out, px, vec4<f32>(0.0, 0.0, 0.0, 1.0));
            if (view.params.w != 0u) {
                textureStore(depth_out, px, vec4<f32>(0.0));
            }
            return;
        }
        loop {
            if (t >= t_stop || transmittance < OPAQUE_BELOW || steps >= max_steps) {
                break;
            }
            let dt = base_step * (1.0 + t * growth);
            // The stretch this sample stands for: a step, or for the last one
            // up to half a step past where the ray stops (see the top).
            let span = select(dt, t_stop + 0.5 * dt - t, t + dt >= t_stop);
            let smp = sample_smoke(slot, ro + rd * t, span, skipped, &budget, &rule, rd, phase_sun, multi_weight);
            skipped = 0.0;
            if (smp.coarse <= 0.002) {
                // Next to nothing here: on to the first sample at least a
                // voxel on, one step as before. A sample can read this
                // little within a voxel of smoke, so with the noise kept
                // within the rule, what the jump steps over counts as cloud
                // the noise took if it lands in smoke.
                steps += 1u;
                @if(NOISE_CAPPED || NOISE_FILLED) let left = t;
                let until = t + max(dt, voxel_size);
                loop {
                    k += 1u;
                    t += base_step * (1.0 + t * growth);
                    if (t >= until) {
                        break;
                    }
                }
                @if(NOISE_CAPPED || NOISE_FILLED) {
                    skipped = select(0.0, 0.5 * (t - left), t - left > dt * 1.5);
                }
                continue;
            }
            if (smp.sigma > 0.0) {
                seen = select(seen, t + dt, transmittance > 0.2);
                // Energy-conserving integration over the step (Hillaire 2015).
                let step_t = exp(-smp.sigma * span);
                radiance += transmittance * smp.source * (1.0 - step_t) / smp.sigma;
                transmittance *= step_t;
            }
            k += 1u;
            steps += 1u;
            t += dt;
        }
    } else {
        // Overlapping smokes: sum every smoke whose interval contains t
        // (never with SINGLE_SMOKE).
        @if(!SINGLE_SMOKE) var k = seq_first(seq, t_begin);
        @if(!SINGLE_SMOKE) var t = seq_t(seq, k);
        @if(!SINGLE_SMOKE) loop {
            if (t >= t_end || transmittance < OPAQUE_BELOW || steps >= max_steps) {
                break;
            }
            let p = ro + rd * t;
            let dt = base_step * (1.0 + t * growth);
            let span = select(dt, t_end + 0.5 * dt - t, t + dt >= t_end);
            var sigma_t = 0.0;
            var source = vec3<f32>(0.0);
            var inside = false;
            var fetched = false;
            var coarse = 0.0;
            // The next smoke's start past t, and how far every smoke here
            // reads nothing.
            var next_start = 1e30;
            var empty_until = 1e30;
            for (var i = 0u; i < count; i++) {
                if (t < t0s[i]) {
                    next_start = min(next_start, t0s[i]);
                    continue;
                }
                if (t > t1s[i]) {
                    continue;
                }
                inside = true;
                let s = globals.slots[ids[i]];
                // Outside the box the smoke occupies at this tick, or in a
                // brick past the cutoff, a sample reads nothing.
                let d = ray_aabb(ro, inv_rd, s.drawn_min.xyz, s.drawn_max.xyz);
                if (t < d.x) {
                    empty_until = min(empty_until, d.x);
                    continue;
                }
                if (t >= d.y) {
                    empty_until = min(empty_until, t1s[i]);
                    continue;
                }
                fetched = true;
                let smp = sample_smoke(s, p, span, skipped, &budget, &rule, rd, phase_sun, multi_weight);
                coarse = max(coarse, smp.coarse);
                sigma_t += smp.sigma;
                source += smp.source;
            }
            skipped = 0.0;
            if (!inside) {
                // Between two smokes: on to the next one, one step as before.
                if (next_start >= 1e29) {
                    break;
                }
                steps += 1u;
                k = max(seq_first(seq, next_start), k + 1u);
                t = seq_t(seq, k);
                continue;
            }
            if (!fetched) {
                // Nothing to read here until a smoke starts or stops being
                // empty: on to the first sample there, charged as the empty
                // steps it would have taken.
                let until = min(empty_until, next_start);
                steps += u32(ceil((until - t) / max(dt, voxel_size)));
                k = max(seq_first(seq, until), k + 1u);
                t = seq_t(seq, k);
                continue;
            }
            if (coarse <= 0.002) {
                steps += 1u;
                @if(NOISE_CAPPED || NOISE_FILLED) let left = t;
                let until = t + max(dt, voxel_size);
                loop {
                    k += 1u;
                    t += base_step * (1.0 + t * growth);
                    if (t >= until) {
                        break;
                    }
                }
                @if(NOISE_CAPPED || NOISE_FILLED) {
                    skipped = select(0.0, 0.5 * (t - left), t - left > dt * 1.5);
                }
                continue;
            }
            if (sigma_t > 0.0) {
                seen = select(seen, t + dt, transmittance > 0.2);
                let step_t = exp(-sigma_t * span);
                radiance += transmittance * source * (1.0 - step_t) / sigma_t;
                transmittance *= step_t;
            }
            k += 1u;
            steps += 1u;
            t += dt;
        }
    }

    @if(NOISE_FILLED) {
        // Fill back in the opacity the noise took, behind what was drawn,
        // from where the coarse picture turns opaque to the rule's threshold.
        if (rule.w > globals.fairness.y && transmittance >= OPAQUE_BELOW) {
            let filled = transmittance * sstep(globals.fairness.y, globals.fairness.z, rule.w);
            radiance += filled * rule.rgb / rule.w;
            transmittance -= filled;
        }
    }
    // Snap nearly-opaque to opaque so nothing behind leaks through.
    if (transmittance < OPAQUE_BELOW) {
        radiance /= 1.0 - transmittance;
        transmittance = 0.0;
    }
    textureStore(smoke_out, px, vec4<f32>(radiance * view.march.x, transmittance));
    if (view.params.w != 0u) {
        textureStore(depth_out, px, vec4<f32>(seen));
    }
}
