// Faceted smoke at reduced resolution: blend the premultiplied smoke layer
// written by the classify and retrace passes over the scene.

import package::shaders::smoke_common::{SmokeView, FLAG_DEBUG_SILHOUETTE};

@group(0) @binding(0) var<uniform> view: SmokeView;
@group(0) @binding(1) var layer: texture_2d<f32>;

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

@fragment
fn fragment(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let px = vec2<i32>(floor(in.position.xy)) - vec2<i32>(view.viewport.xy);
    var v = textureLoad(layer, px, 0);
    if (v.a <= 0.0) {
        discard;
    }
    @if(LDR_TARGET) {
        if ((view.facet.w & FLAG_DEBUG_SILHOUETTE) == 0u) {
            // LDR cameras tonemap inside the main pass, so approximate that
            // here (on the colour before coverage).
            let c = v.rgb / v.a;
            v = vec4<f32>(c / (1.0 + dot(c, vec3<f32>(0.2126, 0.7152, 0.0722))) * v.a, v.a);
        }
    }
    return v;
}
