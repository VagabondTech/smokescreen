// Faceted smoke at reduced resolution: trace the pixels the classify pass
// queued, one per invocation (see `smoke_facet_refine.wesl`).

import package::shaders::smoke_facet_refine::retrace_pixel;

struct Work {
    groups: u32,
    groups_y: u32,
    groups_z: u32,
    count: u32,
    pixels: array<u32>,
}

// Read-only here: the same buffer holds this pass's indirect dispatch.
@group(0) @binding(11) var<storage, read> work: Work;

@compute @workgroup_size(64, 1, 1)
fn retrace(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    let i = (group.y * groups.x + group.x) * 64u + local;
    if (i >= min(work.count, arrayLength(&work.pixels))) {
        return;
    }
    let packed = work.pixels[i];
    retrace_pixel(vec2<u32>(packed & 0xffffu, packed >> 16u));
}
