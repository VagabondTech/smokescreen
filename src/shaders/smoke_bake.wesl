// Bake passes. Run only for slots that changed this frame.
//
// 1. bake_density: per voxel, apply the growth front, dissolve and holes to
//    the flood-fill distance. This is the only place holes are evaluated, so
//    their cost is per voxel instead of per pixel per raymarch step.
// 2. bake_lighting: per voxel, march towards the sun through the smoke and
//    upwards for ambient occlusion, and store the results. The raymarch then
//    reads lighting with a single texture fetch per step. A volumetric
//    smoke's voxels that no sample holding smoke can read (those around the
//    box it occupies at this tick) are cleared instead.

import package::shaders::smoke_common::{
    SmokeGlobals, SmokeSlot, STYLE_FACETED, shape_density, distance_to_segment, sstep
};

@group(0) @binding(0) var<uniform> globals: SmokeGlobals;
@group(0) @binding(1) var source_atlas: texture_3d<f32>;
@group(0) @binding(2) var density_out: texture_storage_3d<rgba8unorm, write>;
@group(0) @binding(3) var density_in: texture_3d<f32>;
@group(0) @binding(4) var linear_clamp: sampler;
@group(0) @binding(5) var lit_out: texture_storage_3d<rgba8unorm, write>;
// 1 where the level is open, 0 where it is solid, per voxel.
@group(0) @binding(6) var open_in: texture_3d<f32>;

struct BakeTexel {
    slot: SmokeSlot,
    local: vec3<u32>,
    texel: vec3<i32>,
    layer: u32,
    valid: bool,
}

// Map a dispatch thread to (slot, voxel). The z dimension of the dispatch is
// `dims.z` threads per slot in the bake list.
fn bake_texel(gid: vec3<u32>) -> BakeTexel {
    var out: BakeTexel;
    let dims = globals.grid.xyz;
    let entry = gid.z / dims.z;
    out.valid = entry < globals.counts.z && gid.x < dims.x && gid.y < dims.y;
    if (!out.valid) {
        return out;
    }
    let packed = globals.bake_slots[entry / 4u];
    out.slot = globals.slots[packed[entry % 4u]];
    out.local = vec3<u32>(gid.x, gid.y, gid.z % dims.z);
    out.layer = u32(out.slot.bounds_min.w + 0.5);
    out.texel = vec3<i32>(out.local + vec3<u32>(0u, 0u, out.layer * dims.z));
    return out;
}

@compute @workgroup_size(4, 4, 4)
fn bake_density(@builtin(global_invocation_id) gid: vec3<u32>) {
    let b = bake_texel(gid);
    if (!b.valid) {
        return;
    }
    let slot = b.slot;
    let src = textureLoad(source_atlas, b.texel, 0);
    var density = shape_density(src.r, slot.params.x, slot.bounds_max.w);
    if (density > 0.0 && slot.holes.y > 0u) {
        let p = slot.grid_min.xyz + (vec3<f32>(b.local) + 0.5) * slot.grid_min.w;
        for (var i = 0u; i < slot.holes.y; i++) {
            let h = globals.holes[slot.holes.x + i];
            let d = distance_to_segment(p, h.a.xyz, h.b.xyz);
            if (d < h.a.w) {
                let core = 1.0 - sstep(h.a.w * 0.45, h.a.w, d);
                density *= 1.0 - h.b.w * core;
            }
        }
    }
    // g, b: the level's shadowing of sun and sky, carried through unchanged.
    textureStore(density_out, b.texel, vec4<f32>(clamp(density, 0.0, 1.0), src.g, src.b, 1.0));
}

// Filtered density at a position in voxel units, kept inside the slot's
// layer. Like the march, it averages the open voxels only, so smoke next to
// walls shades itself as much as smoke away from them.
fn density_at(voxel_pos: vec3<f32>, layer: u32) -> f32 {
    let dims = vec3<f32>(globals.grid.xyz);
    var uvw = voxel_pos / dims;
    let half_texel = 0.5 / dims.z;
    uvw.z = (clamp(uvw.z, half_texel, 1.0 - half_texel) + f32(layer)) / f32(globals.grid.w);
    let density = textureSampleLevel(density_in, linear_clamp, uvw, 0.0).r;
    if (density <= 0.0) {
        return 0.0;
    }
    let open = textureSampleLevel(open_in, linear_clamp, uvw, 0.0).r;
    return min(density / max(open, 0.004), 1.0);
}

// Optical depth (density x metres) from a voxel along `dir`, with steps that
// grow as they get further away. Stops at the grid edge.
fn optical_depth(start: vec3<f32>, dir: vec3<f32>, layer: u32, steps: u32, voxel_size: f32) -> f32 {
    let dims = vec3<f32>(globals.grid.xyz);
    var od = 0.0;
    var t = 0.35;
    var stride = 0.75;
    for (var i = 0u; i < steps; i++) {
        let p = start + dir * (t + stride * 0.5);
        if (any(p < vec3<f32>(0.0)) || any(p > dims)) {
            break;
        }
        od += density_at(p, layer) * stride * voxel_size;
        t += stride;
        stride *= 1.4;
    }
    return od;
}

@compute @workgroup_size(4, 4, 4)
fn bake_lighting(@builtin(global_invocation_id) gid: vec3<u32>) {
    let b = bake_texel(gid);
    if (!b.valid) {
        return;
    }
    let slot = b.slot;
    let voxel_size = slot.grid_min.w;
    // A voxel of a volumetric smoke whose 26 neighbours all lie outside the
    // box the smoke occupies at this tick: none of them holds smoke, so no
    // sample that reads this voxel does either, and its lighting is never
    // seen. Its density is 0, and the march reads that from here: store it
    // and skip the marches. (Faceted smokes read lighting at their puffs'
    // surfaces, which can reach past the box, and are baked in full.)
    if (slot.holes.z != STYLE_FACETED) {
        let centre = slot.grid_min.xyz + (vec3<f32>(b.local) + 0.5) * voxel_size;
        if (any(centre + voxel_size < slot.drawn_min.xyz) || any(centre - voxel_size > slot.drawn_max.xyz)) {
            textureStore(lit_out, b.texel, vec4<f32>(0.0));
            return;
        }
    }
    let here = textureLoad(density_in, b.texel, 0);
    let start = vec3<f32>(b.local) + 0.5;
    let sigma = globals.medium.w * slot.params.y;

    // Sun: single scattering, plus Wrenninge-style octaves that stand in for
    // multiple scattering. The octaves see a thinner medium, so light reaches
    // deeper into the cloud instead of the core going black. Stored
    // normalised (1 at the surface); the march applies the strength.
    var single = 0.0;
    var multi = 0.0;
    if (globals.sun_direction.w > 0.5) {
        let od = optical_depth(start, globals.sun_direction.xyz, b.layer, 10u, voxel_size) * sigma;
        single = exp(-od);
        multi = (exp(-od * 0.5) + 0.7 * exp(-od * 0.25) + 0.5 * exp(-od * 0.1)) / 2.2;
    }
    // Precomputed on the CPU: how much the level shades this voxel from the
    // sun (g) and from the sky (b).
    let sun_visibility = here.g;
    let sky_visibility = here.b;

    // Ambient: how much sky the level lets through (never below the indoor
    // floor, standing in for bounce light), times how buried the voxel is.
    let od_up = optical_depth(start, vec3<f32>(0.0, 1.0, 0.0), b.layer, 6u, voxel_size) * sigma;
    let sky = mix(globals.lighting.y, 1.0, sky_visibility);
    let ambient = sky * mix(1.0, exp(-od_up * 0.25), globals.lighting.z);

    textureStore(
        lit_out,
        b.texel,
        vec4<f32>(here.r, single * sun_visibility, multi * sun_visibility, ambient),
    );
}
