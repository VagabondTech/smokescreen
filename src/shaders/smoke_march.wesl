// Reduced-resolution raymarch through all smokes in view.
//
// Per pixel: intersect the view ray with every smoke's bounds (cheap ALU),
// then march front to back only inside those intervals. Each step costs one
// fetch of the baked volume (density + lighting) and one or two fetches of
// the tiling detail noise. Rays stop at scene depth, at full opacity, or at
// the step budget, and skip empty voxels. The common case of a ray crossing
// a single smoke takes a fast path with that smoke's data hoisted out of the
// loop.

import package::shaders::smoke_common::{
    SmokeGlobals, SmokeSlot, SmokeView, MAX_SLOTS, PI, STYLE_FACETED,
    hg_phase, ray_aabb, interleaved_gradient_noise, remap
};

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

// Extinction and in-scattered light of one smoke at one point.
fn sample_smoke(slot: SmokeSlot, p: vec3<f32>, rd: vec3<f32>, phase_sun: f32, multi_weight: f32) -> StepSample {
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
    let d = detail_density(min(lit.r / max(open, 0.004), 1.0), p, slot);
    if (d <= 0.0) {
        return out;
    }
    let sigma = d * globals.medium.w * slot.params.y;
    var light = globals.sun_color.rgb * (lit.g * phase_sun + lit.b * multi_weight)
        + globals.ambient.rgb * lit.a;
    if (globals.counts.y > 0u) {
        light += point_lights(slot, p, rd);
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
    var t0s: array<f32, MAX_SLOTS>;
    var t1s: array<f32, MAX_SLOTS>;
    var ids: array<u32, MAX_SLOTS>;
    var count = 0u;
    var t_begin = 1e30;
    var t_end = 0.0;
    for (var i = 0u; i < globals.counts.x; i++) {
        let s = globals.slots[i];
        if (s.holes.z == STYLE_FACETED) {
            continue;
        }
        let hit = ray_aabb(ro, inv_rd, s.bounds_min.xyz, s.bounds_max.xyz);
        let a = max(hit.x, 0.0);
        let b = min(hit.y, t_scene);
        if (b > a) {
            t0s[count] = a;
            t1s[count] = b;
            ids[count] = i;
            count += 1u;
            t_begin = min(t_begin, a);
            t_end = max(t_end, b);
        }
    }
    if (count == 0u) {
        textureStore(smoke_out, px, vec4<f32>(0.0, 0.0, 0.0, 1.0));
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

    var t = t_begin + jitter * base_step;
    var transmittance = 1.0;
    var radiance = vec3<f32>(0.0);
    var steps = 0u;

    if (count == 1u) {
        // Fast path: one smoke along this ray.
        let slot = globals.slots[ids[0]];
        loop {
            if (t >= t_end || transmittance < 0.01 || steps >= max_steps) {
                break;
            }
            steps += 1u;
            let dt = base_step * (1.0 + t * growth);
            let smp = sample_smoke(slot, ro + rd * t, rd, phase_sun, multi_weight);
            if (smp.coarse <= 0.002) {
                // No smoke within a voxel of here: skip ahead.
                t += max(dt, voxel_size);
                continue;
            }
            if (smp.sigma > 0.0) {
                // Energy-conserving integration over the step (Hillaire 2015).
                let step_t = exp(-smp.sigma * dt);
                radiance += transmittance * smp.source * (1.0 - step_t) / smp.sigma;
                transmittance *= step_t;
            }
            t += dt;
        }
    } else {
        // Overlapping smokes: sum every smoke whose interval contains t.
        loop {
            if (t >= t_end || transmittance < 0.01 || steps >= max_steps) {
                break;
            }
            steps += 1u;
            let dt = base_step * (1.0 + t * growth);
            let p = ro + rd * t;
            var sigma_t = 0.0;
            var source = vec3<f32>(0.0);
            var inside = false;
            var coarse = 0.0;
            var next_start = 1e30;
            for (var k = 0u; k < count; k++) {
                if (t < t0s[k]) {
                    next_start = min(next_start, t0s[k]);
                    continue;
                }
                if (t > t1s[k]) {
                    continue;
                }
                inside = true;
                let smp = sample_smoke(globals.slots[ids[k]], p, rd, phase_sun, multi_weight);
                coarse = max(coarse, smp.coarse);
                sigma_t += smp.sigma;
                source += smp.source;
            }
            if (!inside) {
                // Between two smokes: jump straight to the next one.
                if (next_start >= 1e29) {
                    break;
                }
                t = next_start + jitter * base_step;
                continue;
            }
            if (coarse <= 0.002) {
                t += max(dt, voxel_size);
                continue;
            }
            if (sigma_t > 0.0) {
                let step_t = exp(-sigma_t * dt);
                radiance += transmittance * source * (1.0 - step_t) / sigma_t;
                transmittance *= step_t;
            }
            t += dt;
        }
    }

    // Snap nearly-opaque to opaque so nothing behind leaks through.
    if (transmittance < 0.01) {
        radiance /= 1.0 - transmittance;
        transmittance = 0.0;
    }
    textureStore(smoke_out, px, vec4<f32>(radiance * view.march.x, transmittance));
}
