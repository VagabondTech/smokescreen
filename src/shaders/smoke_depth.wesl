// How far a camera's rays see volumetric smoke, for the resolution the
// march runs at (`quality::update_smoke_quality`).
//
// The march wrote, for each ray, where it last saw smoke through more than a
// fifth of the scene (0 where it saw none). One workgroup per 8×8 tile of a
// march's rectangle takes the tile's farthest, and counts it into the
// camera's histogram of DEPTH_BIN-metre bins. The histogram is read back a
// frame or two later; the controller takes its 97th percentile.

import package::shaders::smoke_common::SmokeView;

const DEPTH_BINS: u32 = 256u;
const DEPTH_BIN: f32 = 0.25;

@group(0) @binding(0) var<uniform> view: SmokeView;
@group(0) @binding(1) var depth_in: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> histogram: array<atomic<u32>, DEPTH_BINS>;

// The tile's farthest depth: positive floats order as their bits do.
var<workgroup> farthest: atomic<u32>;

@compute @workgroup_size(8, 8, 1)
fn reduce(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) index: u32) {
    if (index == 0u) {
        atomicStore(&farthest, 0u);
    }
    workgroupBarrier();
    let px = view.rect.xy + gid.xy;
    if (px.x < view.rect.z && px.y < view.rect.w) {
        let depth = textureLoad(depth_in, px, 0).r;
        if (depth > 0.0) {
            atomicMax(&farthest, bitcast<u32>(depth));
        }
    }
    workgroupBarrier();
    if (index == 0u) {
        let bits = atomicLoad(&farthest);
        if (bits != 0u) {
            atomicAdd(&histogram[min(u32(bitcast<f32>(bits) / DEPTH_BIN), DEPTH_BINS - 1u)], 1u);
        }
    }
}
