// Faceted smoke at reduced resolution: classify every pixel of the smoke's
// rectangle (see `smoke_facet_refine.wesl`) and queue the ones that need a
// full-resolution trace, with the retrace pass's dispatch size.

import package::shaders::smoke_facets::view;
import package::shaders::smoke_facet_refine::classify_pixel;

struct Work {
    // Indirect dispatch of the retrace pass: workgroups x, y, z.
    groups: atomic<u32>,
    groups_y: atomic<u32>,
    groups_z: atomic<u32>,
    count: atomic<u32>,
    // Queued pixels, x | y << 16.
    pixels: array<u32>,
}

@group(0) @binding(11) var<storage, read_write> work: Work;

// Must match the retrace pass's workgroup size.
const RETRACE_GROUP: u32 = 64u;
// Workgroups per dispatch dimension (WebGPU's minimum limit); more queued
// pixels spill into y.
const MAX_GROUPS_X: u32 = 65535u;

var<workgroup> group_count: atomic<u32>;
var<workgroup> group_base: u32;

@compute @workgroup_size(8, 8, 1)
fn classify(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if (li == 0u) {
        atomicStore(&group_count, 0u);
    }
    workgroupBarrier();

    let px = view.facet_px.xy + gid.xy;
    var queued = false;
    if (all(px < view.facet_px.zw)) {
        queued = classify_pixel(px);
    }

    // One global reservation per workgroup.
    var index = 0u;
    if (queued) {
        index = atomicAdd(&group_count, 1u);
    }
    workgroupBarrier();
    if (li == 0u) {
        let n = atomicLoad(&group_count);
        if (n > 0u) {
            let base = atomicAdd(&work.count, n);
            group_base = base;
            // The largest count any workgroup saw wins, so the maxima of
            // these (both monotonic in the count) are the final dispatch.
            let groups = (base + n + RETRACE_GROUP - 1u) / RETRACE_GROUP;
            atomicMax(&work.groups, min(groups, MAX_GROUPS_X));
            atomicMax(&work.groups_y, (groups + MAX_GROUPS_X - 1u) / MAX_GROUPS_X);
            atomicStore(&work.groups_z, 1u);
        }
    }
    workgroupBarrier();
    if (queued && group_base + index < arrayLength(&work.pixels)) {
        work.pixels[group_base + index] = px.x | (px.y << 16u);
    }
}
