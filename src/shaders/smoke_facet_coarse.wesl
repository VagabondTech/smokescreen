// Faceted smoke, coarse pre-pass: one ray per divisor×divisor block of
// pixels. Each ray records the first distance at which it came within a few
// coarse pixels of the surface, where it hit (1e30 for never), and at the hit
// the surface normal and the expensive part of the lighting (shadow ray,
// occlusion). The full-resolution pass then
//   - skips pixels whose four surrounding coarse rays saw nothing,
//   - shades facet interiors straight from the four samples, without tracing,
//   - traces everything else (silhouettes, creases) again, starting just in
//     front of the surface, and reuses the samples' lighting where they agree.
//
// The near threshold is wide enough that any surface a full-resolution pixel
// could show (hit or anti-aliasing fringe) passes within it of the coarse ray
// of the pixel's own block, so skipping never drops smoke that a
// full-resolution trace would draw.
//
// Texel: first near approach and hit distance (f32 bits; a miss stores minus
// its last near approach instead), octahedral normal (2×snorm16), sun and sky
// visibility (2×unorm8) and slot (16 bits).

import package::shaders::smoke_facets::{
    view, globals, depth_texture, trace_all, pixel_ray, scene_distance, surface_normal, lighting,
    oct_encode, pack_lighting
};

@group(0) @binding(9) var coarse_out: texture_storage_2d<rgba32uint, write>;

// Near threshold in coarse pixels. A full-resolution ray is at most 0.53
// coarse pixels (divisor 4) from its block's ray and draws smoke within one
// pixel (0.25 coarse) of the surface; sphere tracing samples a pass at most
// ~1.5× its closest approach. 2 leaves margin on top.
const NEAR_PX: f32 = 2.0;

@compute @workgroup_size(8, 8, 1)
fn coarse(@builtin(global_invocation_id) gid: vec3<u32>) {
    let c = view.facet_rect.xy + gid.xy;
    if (c.x >= view.facet_rect.z || c.y >= view.facet_rect.w) {
        return;
    }
    let divisor = view.facet.x;
    let size = vec2<u32>(view.target_size.xy);
    let origin = vec2<i32>(view.viewport.xy);

    // Trace to the farthest scene depth in the block (reverse-Z: smallest
    // value), so a block straddling a wall's silhouette still sees the smoke
    // behind it.
    let block = c * divisor;
    var raw_min = 1.0;
    for (var y = 0u; y < divisor; y++) {
        for (var x = 0u; x < divisor; x++) {
            let px = min(block + vec2<u32>(x, y), size - vec2<u32>(1u));
            raw_min = min(raw_min, textureLoad(depth_texture, vec2<i32>(px) + origin, 0));
        }
    }

    let local = min((vec2<f32>(c) + 0.5) * f32(divisor), view.target_size.xy - 0.5);
    let ray = pixel_ray(local);
    let t_scene = scene_distance(local, ray.rd, raw_min);
    let px_angle = view.facet_f.y * f32(divisor);
    // Hits to the full-resolution tolerance (in coarse pixels): a looser hit
    // would let blocks just outside a silhouette count as inside it.
    let tolerance = view.facet_f.x / f32(divisor);
    let hit = trace_all(ray.ro, ray.rd, 0.0, t_scene, px_angle, tolerance, NEAR_PX, view.facet.y);
    // A miss: minus the last near approach (negative even if there was none).
    var hit_t = -max(hit.trace.last_near, 1e-3);
    var normal_bits = 0u;
    var light_bits = 0u;
    if (hit.trace.hit) {
        hit_t = hit.trace.t;
        let s = globals.slots[hit.slot];
        let p = ray.ro + ray.rd * hit_t;
        let n = surface_normal(s, p, ray.rd);
        normal_bits = pack2x16snorm(oct_encode(n));
        light_bits = pack_lighting(lighting(hit.slot, s, p, n), hit.slot);
    }
    textureStore(
        coarse_out,
        vec2<i32>(c),
        vec4<u32>(bitcast<u32>(hit.trace.first_near), bitcast<u32>(hit_t), normal_bits, light_bits),
    );
}
