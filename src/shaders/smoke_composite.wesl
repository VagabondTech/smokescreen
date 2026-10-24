// Depth-aware upsample of the reduced-resolution smoke buffer, blended over
// the scene by the fixed-function blender: scene * transmittance + light.
//
// Each smoke texel was marched against the depth of one representative
// full-resolution texel. For every full-resolution pixel we take the 2x2
// smoke texels around it and weight them by bilinear position and by how
// close their representative depth is to this pixel's depth, so smoke doesn't
// bleed across the silhouettes of objects in front of it. Where the four
// texels agree within `AGREE` anyway, any blend of them does too, so they are
// blended by position alone and the depth buffer is left unread. At full
// resolution a pixel's own texel is all it needs.

import package::shaders::smoke_common::{SmokeView, FLAG_DEBUG_TRANSMITTANCE};

// How far apart, in every channel, the four texels around a pixel may be for
// it to skip the depth weights: no weights could then move the pixel by more.
// Close up at the best dial step about nine pixels in ten skip them, which on
// an Apple M4 at 1080p saved 0.25 of the 0.33 ms the depth reads cost there.
// At 0.03 no pixel of the demo's sweep changed by more than 5 levels out of
// 255. At 0.01 none changed by more than 1, but at the two cheapest steps,
// whose texels lie 4 pixels apart, about a tenth as many pixels skipped.
const AGREE: f32 = 0.03;

@group(0) @binding(0) var<uniform> view: SmokeView;
@if(MULTISAMPLED)
@group(0) @binding(1) var depth_texture: texture_depth_multisampled_2d;
@else
@group(0) @binding(1) var depth_texture: texture_depth_2d;
@group(0) @binding(2) var smoke_texture: texture_2d<f32>;

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

// View-space distance for Bevy's reverse-Z infinite perspective projection.
fn linear_depth(raw: f32) -> f32 {
    return view.camera_position.w / max(raw, 1e-7);
}

@fragment
fn fragment(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let target_px = vec2<i32>(floor(in.position.xy));
    let local = vec2<f32>(target_px) - view.viewport.xy;
    let divisor = view.march.w;
    let viewport_origin = vec2<i32>(view.viewport.xy);
    let full_max = vec2<i32>(view.target_size.xy) - vec2<i32>(1);
    let low_max = vec2<i32>(view.target_size.zw) - vec2<i32>(1);

    let u = local / divisor;
    let base = vec2<i32>(floor(u));
    let f = u - vec2<f32>(base);

    // At full resolution every pixel has a smoke texel of its own, marched
    // against its own depth: the bilinear weights of its neighbours are all
    // but zero, so take it alone and leave the depth buffer unread.
    if (divisor == 1.0) {
        let smoke = textureLoad(smoke_texture, clamp(base, vec2<i32>(0), low_max), 0);
        if (smoke.a > 0.999) {
            discard;
        }
        return finish(smoke);
    }

    // Fetch the 2x2 smoke texels first: most pixels in the scissor rect see
    // no smoke at all and can leave before touching the depth buffer.
    var texels: array<vec4<f32>, 4>;
    var coords: array<vec2<i32>, 4>;
    var clear = true;
    for (var k = 0; k < 4; k++) {
        let c = clamp(base + vec2<i32>(k & 1, k >> 1), vec2<i32>(0), low_max);
        coords[k] = c;
        texels[k] = textureLoad(smoke_texture, c, 0);
        clear = clear && texels[k].a > 0.999;
    }
    if (clear) {
        discard;
    }
    // Four texels that agree: blend them by position, without the depth.
    let lo = min(min(texels[0], texels[1]), min(texels[2], texels[3]));
    let hi = max(max(texels[0], texels[1]), max(texels[2], texels[3]));
    if (all(hi - lo <= vec4<f32>(AGREE))) {
        let smoke = mix(mix(texels[0], texels[1], f.x), mix(texels[2], texels[3], f.x), f.y);
        if (smoke.a > 0.999) {
            discard;
        }
        return finish(smoke);
    }

    let z = linear_depth(textureLoad(depth_texture, target_px, 0));
    var sum = vec4<f32>(0.0);
    var weight_sum = 0.0;
    for (var k = 0; k < 4; k++) {
        let representative = viewport_origin + min(vec2<i32>(floor(vec2<f32>(coords[k]) * divisor)), full_max);
        let zs = linear_depth(textureLoad(depth_texture, representative, 0));
        let wx = select(1.0 - f.x, f.x, (k & 1) == 1);
        let wy = select(1.0 - f.y, f.y, (k >> 1) == 1);
        let relative = abs(zs - z) / max(z, 1e-3);
        // ~0.5 at a 3 % depth difference, ~0.01 at 30 %.
        let depth_weight = 1.0 / (1.0 + relative * relative * 1000.0);
        let w = max(wx * wy, 1e-3) * depth_weight;
        sum += texels[k] * w;
        weight_sum += w;
    }
    let smoke = sum / max(weight_sum, 1e-6);
    if (smoke.a > 0.999) {
        discard;
    }
    return finish(smoke);
}

// The premultiplied colour and coverage the blender applies.
fn finish(smoke: vec4<f32>) -> vec4<f32> {
    if ((view.facet.w & FLAG_DEBUG_TRANSMITTANCE) != 0u) {
        // The transmittance itself, opaque (`SmokeSettings::debug_transmittance`).
        return vec4<f32>(1.0, smoke.a, 1.0, 1.0);
    }
    var light = smoke.rgb;
    @if(LDR_TARGET) {
        // LDR cameras tonemap inside the main pass, so approximate that here.
        light = light / (1.0 + dot(light, vec3<f32>(0.2126, 0.7152, 0.0722)));
    }
    return vec4<f32>(light, 1.0 - smoke.a);
}
