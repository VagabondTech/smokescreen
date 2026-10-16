// Faceted smoke at full resolution: trace and shade every pixel of the
// smoke's rectangle, and blend over the scene. Opaque, except for a thin
// anti-aliasing fringe outside the silhouette, which only ever adds smoke.
// (Reduced resolutions use the coarse, classify, retrace and composite passes
// instead.)

import package::shaders::smoke_common::FLAG_DEBUG_SILHOUETTE;
import package::shaders::smoke_facets::{
    view, depth_texture, trace_all, pixel_ray, scene_distance, shade, inside_slot, whiteout
};

struct FullscreenOutput {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> FullscreenOutput {
    // One triangle covering the screen.
    let uv = vec2<f32>(f32(index >> 1u), f32(index & 1u)) * 2.0;
    var out: FullscreenOutput;
    out.position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

fn finish(light: vec3<f32>) -> vec3<f32> {
    if ((view.facet.w & FLAG_DEBUG_SILHOUETTE) != 0u) {
        return vec3<f32>(1.0, 0.0, 1.0);
    }
    var c = light;
    @if(LDR_TARGET) {
        // LDR cameras tonemap inside the main pass, so approximate that here.
        c = c / (1.0 + dot(c, vec3<f32>(0.2126, 0.7152, 0.0722)));
    }
    return c;
}

@fragment
fn fragment(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let target_px = vec2<i32>(floor(in.position.xy));
    let local = vec2<f32>(target_px) - view.viewport.xy + 0.5;
    let ray = pixel_ray(local);

    let inside = inside_slot(ray.ro);
    if (inside >= 0) {
        return vec4<f32>(finish(whiteout(u32(inside))), 1.0);
    }

    let raw_depth = textureLoad(depth_texture, target_px, 0);
    let t_scene = scene_distance(local, ray.rd, raw_depth);
    let hit = trace_all(ray.ro, ray.rd, 0.0, t_scene, view.facet_f.y, view.facet_f.x, 0.0, view.facet.y);
    if (hit.trace.hit) {
        return vec4<f32>(finish(shade(hit.slot, ray.ro + ray.rd * hit.trace.t, ray.rd)), 1.0);
    }
    // Anti-aliasing: a miss that passed within half a pixel of the surface.
    let coverage = clamp(0.5 - hit.trace.near_ratio, 0.0, 1.0);
    if (coverage <= 0.0) {
        discard;
    }
    let color = finish(shade(hit.near_slot, ray.ro + ray.rd * hit.trace.near_t, ray.rd));
    return vec4<f32>(color * coverage, coverage);
}
