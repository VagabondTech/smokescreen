// Faceted smoke: the signed distance field of tumbling polyhedral puffs,
// merged with a smooth minimum, clipped to the flood fill's region and cut by
// bullet tunnels and explosion cavities. Mirrors `facets.rs`, whose CPU copy
// answers gameplay queries.
//
// Tracing walks a uniform grid of bins; each bin lists the puffs that can
// reach it. Far from the surface a step only tests bounding spheres; polyhedra
// are evaluated near the surface, and holes and the region clip only right at
// it. Shared by the coarse pre-pass and the full-resolution trace.

import package::shaders::smoke_common::{
    SmokeGlobals, SmokeSlot, SmokeView, PI, STYLE_FACETED,
    FLAG_TRACED_SHADOWS, FLAG_TRACED_OCCLUSION, ray_aabb, distance_to_segment
};

struct FacetPuff {
    // xyz: centre, w: inradius now (0 = not there yet / any more)
    center_radius: vec4<f32>,
    // Rows of the world-to-local rotation. w: shape id, corner ratio, unused.
    rot0: vec4<f32>,
    rot1: vec4<f32>,
    rot2: vec4<f32>,
}

@group(0) @binding(0) var<uniform> globals: SmokeGlobals;
@group(0) @binding(1) var<uniform> view: SmokeView;
@if(MULTISAMPLED)
@group(0) @binding(2) var depth_texture: texture_depth_multisampled_2d;
@else
@group(0) @binding(2) var depth_texture: texture_depth_2d;
@group(0) @binding(3) var source_atlas: texture_3d<f32>;
@group(0) @binding(4) var lit_atlas: texture_3d<f32>;
@group(0) @binding(5) var linear_clamp: sampler;
@group(0) @binding(6) var<storage, read> facet_puffs: array<FacetPuff>;
@group(0) @binding(7) var<storage, read> facet_cells: array<vec2<u32>>;
@group(0) @binding(8) var<storage, read> facet_indices: array<u32>;

const FAR: f32 = 1e3;
// Bounding-sphere distance below which real polyhedra are evaluated.
const NEAR_SURFACE: f32 = 0.2;
// Distance below which the region clip and holes are applied. Both can only
// make the field larger, so skipping them further out stays conservative.
const CLIP_THRESHOLD: f32 = 0.25;
const REGION_RANGE: f32 = 1.6;

const INV_SQRT3: f32 = 0.57735027;
// Dodecahedron and icosahedron face normals (0, b, c) and their cyclic
// permutations.
const DB: f32 = 0.52573111;
const DC: f32 = 0.85065081;
const IB: f32 = 0.35682209;
const IC: f32 = 0.93417236;
const ICOSA_RATIO: f32 = 1.258409;
const HEX_RATIO: f32 = 1.1547005;

fn is_faceted(s: SmokeSlot) -> bool {
    return s.holes.z == STYLE_FACETED;
}

// Polynomial smooth minimum; exactly min() when a and b differ by k or more.
fn smin(a: f32, b: f32, k: f32) -> f32 {
    if (k <= 0.0) {
        return min(a, b);
    }
    let h = max(k - abs(a - b), 0.0) / k;
    return min(a, b) - h * h * k * 0.25;
}

// Largest |n·q| over a shape's face normals (its "gauge"): 1 on the surface
// of the unit-inradius polyhedron. Ids: 0 cube, 1 octahedron,
// 2 dodecahedron, 3 icosahedron.
fn gauge(q: vec3<f32>, shape: u32) -> f32 {
    let a = abs(q);
    switch shape {
        case 0u: {
            return max(a.x, max(a.y, a.z));
        }
        case 1u: {
            return (a.x + a.y + a.z) * INV_SQRT3;
        }
        case 2u: {
            return max(DB * a.y + DC * a.z, max(DB * a.x + DC * a.y, DC * a.x + DB * a.z));
        }
        default: {
            let ico = max(IB * a.y + IC * a.z, max(IB * a.x + IC * a.y, IC * a.x + IB * a.z));
            return max((a.x + a.y + a.z) * INV_SQRT3, ico);
        }
    }
}

// The best of the three cyclic face pairs: (face normal, gauge).
fn best_of_three(a: vec3<f32>, s: vec3<f32>, b: f32, c: f32) -> vec4<f32> {
    let g0 = b * a.y + c * a.z;
    let g1 = b * a.x + c * a.y;
    let g2 = c * a.x + b * a.z;
    if (g0 >= g1 && g0 >= g2) {
        return vec4<f32>(0.0, b * s.y, c * s.z, g0);
    }
    if (g1 >= g2) {
        return vec4<f32>(b * s.x, c * s.y, 0.0, g1);
    }
    return vec4<f32>(c * s.x, 0.0, b * s.z, g2);
}

// Gauge plus the normal of the face that achieves it (local space).
fn gauge_grad(q: vec3<f32>, shape: u32) -> vec4<f32> {
    let a = abs(q);
    let s = select(vec3<f32>(-1.0), vec3<f32>(1.0), q >= vec3<f32>(0.0));
    switch shape {
        case 0u: {
            if (a.x >= a.y && a.x >= a.z) {
                return vec4<f32>(s.x, 0.0, 0.0, a.x);
            }
            if (a.y >= a.z) {
                return vec4<f32>(0.0, s.y, 0.0, a.y);
            }
            return vec4<f32>(0.0, 0.0, s.z, a.z);
        }
        case 1u: {
            return vec4<f32>(s * INV_SQRT3, (a.x + a.y + a.z) * INV_SQRT3);
        }
        case 2u: {
            return best_of_three(a, s, DB, DC);
        }
        default: {
            let ico = best_of_three(a, s, IB, IC);
            let oct = (a.x + a.y + a.z) * INV_SQRT3;
            if (oct > ico.w) {
                return vec4<f32>(s * INV_SQRT3, oct);
            }
            return ico;
        }
    }
}

// Duff et al. 2017 orthonormal basis, identical to glam's
// `any_orthonormal_pair` so tunnels match the CPU exactly.
fn ortho_basis(n: vec3<f32>) -> mat2x3<f32> {
    let sgn = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (sgn + n.z);
    let b = n.x * n.y * a;
    return mat2x3<f32>(
        vec3<f32>(1.0 + sgn * n.x * n.x * a, sgn * b, -sgn * n.x),
        vec3<f32>(b, sgn + n.y * n.y * a, -n.y),
    );
}

// Hexagonal tunnel around a..b: (gradient, distance).
fn tunnel(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>, r: f32) -> vec4<f32> {
    let ab = b - a;
    let len = max(length(ab), 1e-6);
    let u = ab / len;
    let q = p - a;
    let s = dot(q, u);
    let radial = q - u * s;
    let e = ortho_basis(u);
    let x = dot(radial, e[0]);
    let y = dot(radial, e[1]);
    let h1 = 0.5 * x + 0.8660254 * y;
    let h2 = -0.5 * x + 0.8660254 * y;
    var hex = abs(x);
    var n2 = vec2<f32>(sign(x), 0.0);
    if (abs(h1) > hex) {
        hex = abs(h1);
        n2 = sign(h1) * vec2<f32>(0.5, 0.8660254);
    }
    if (abs(h2) > hex) {
        hex = abs(h2);
        n2 = sign(h2) * vec2<f32>(-0.5, 0.8660254);
    }
    var value = hex - r;
    var grad = e[0] * n2.x + e[1] * n2.y;
    let cap = max(-s, s - len);
    if (cap > value) {
        value = cap;
        grad = select(u, -u, -s > s - len);
    }
    return vec4<f32>(grad, value);
}

// Texture coordinates of a world point in this slot's layer of an atlas.
fn atlas_uvw(s: SmokeSlot, p: vec3<f32>) -> vec3<f32> {
    let dims = vec3<f32>(globals.grid.xyz);
    var uvw = (p - s.grid_min.xyz) / (s.grid_min.w * dims);
    let half_texel = 0.5 / dims.z;
    uvw.z = (clamp(uvw.z, half_texel, 1.0 - half_texel) + s.bounds_min.w) / f32(globals.grid.w);
    return uvw;
}

// Signed distance to the fill plus its halo (puffs never go past it).
fn region_distance(s: SmokeSlot, p: vec3<f32>) -> f32 {
    let v = textureSampleLevel(source_atlas, linear_clamp, atlas_uvw(s, p), 0.0).a;
    return (v * 2.0 - 1.0) * REGION_RANGE - s.facet_shape.y;
}

fn region_gradient(s: SmokeSlot, p: vec3<f32>) -> vec3<f32> {
    let e = s.grid_min.w * 0.5;
    return vec3<f32>(
        region_distance(s, p + vec3<f32>(e, 0.0, 0.0)) - region_distance(s, p - vec3<f32>(e, 0.0, 0.0)),
        region_distance(s, p + vec3<f32>(0.0, e, 0.0)) - region_distance(s, p - vec3<f32>(0.0, e, 0.0)),
        region_distance(s, p + vec3<f32>(0.0, 0.0, e)) - region_distance(s, p - vec3<f32>(0.0, 0.0, e)),
    );
}

fn bins_max(s: SmokeSlot) -> vec3<f32> {
    return s.facet_grid.xyz + vec3<f32>(s.facet_dims.xyz) * s.facet_grid.w;
}

fn bin_of(s: SmokeSlot, p: vec3<f32>) -> vec3<i32> {
    return vec3<i32>(floor((p - s.facet_grid.xyz) / s.facet_grid.w));
}

fn bin_inside(s: SmokeSlot, c: vec3<i32>) -> bool {
    return all(c >= vec3<i32>(0)) && all(c < vec3<i32>(s.facet_dims.xyz));
}

// (first index, count) of a bin's puff list.
fn bin_range(s: SmokeSlot, c: vec3<i32>) -> vec2<u32> {
    let d = s.facet_dims.xyz;
    return facet_cells[s.facet_dims.w + u32(c.x) + d.x * (u32(c.y) + d.y * u32(c.z))];
}

// Distance along the ray to leave bin c.
fn bin_exit(s: SmokeSlot, c: vec3<i32>, ro: vec3<f32>, inv_rd: vec3<f32>) -> f32 {
    let cmin = s.facet_grid.xyz + vec3<f32>(c) * s.facet_grid.w;
    let e0 = (cmin - ro) * inv_rd;
    let e1 = (cmin + s.facet_grid.w - ro) * inv_rd;
    let far = max(e0, e1);
    return min(far.x, min(far.y, far.z));
}

// Lower bound on the distance to any listed puff, from bounding spheres.
fn puffs_bound(s: SmokeSlot, p: vec3<f32>, range: vec2<u32>) -> f32 {
    var b = FAR;
    let base = s.facet_ofs.x + range.x;
    for (var j = 0u; j < range.y; j++) {
        let puff = facet_puffs[s.facet_ofs.y + facet_indices[base + j]];
        let r = puff.center_radius.w;
        if (r > 0.0) {
            let ratio = puff.rot1.w;
            b = min(b, (length(p - puff.center_radius.xyz) - r * ratio) / ratio);
        }
    }
    return b;
}

// Smooth union of the listed puffs.
fn puffs_distance(s: SmokeSlot, p: vec3<f32>, range: vec2<u32>) -> f32 {
    var d = FAR;
    let k = s.facet_shape.x;
    let base = s.facet_ofs.x + range.x;
    for (var j = 0u; j < range.y; j++) {
        let puff = facet_puffs[s.facet_ofs.y + facet_indices[base + j]];
        let r = puff.center_radius.w;
        if (r <= 0.0) {
            continue;
        }
        let rel = p - puff.center_radius.xyz;
        let ratio = puff.rot1.w;
        // A puff whose bounding sphere is k past the current surface can't
        // change the smooth union.
        if ((length(rel) - r * ratio) / ratio >= d + k) {
            continue;
        }
        let q = vec3<f32>(dot(puff.rot0.xyz, rel), dot(puff.rot1.xyz, rel), dot(puff.rot2.xyz, rel));
        d = smin(d, gauge(q, u32(puff.rot0.w)) - r, k);
    }
    return d;
}

fn apply_holes(s: SmokeSlot, p: vec3<f32>, d_in: f32) -> f32 {
    var d = d_in;
    for (var i = 0u; i < s.holes.y; i++) {
        let h = globals.holes[s.holes.x + i];
        let strength = h.b.w;
        if (strength <= 0.0) {
            continue;
        }
        let r = h.a.w * min(strength * 1.5, 1.0);
        if (all(h.a.xyz == h.b.xyz)) {
            // Explosion: an icosahedral cavity of inradius 0.8 r.
            let rel = p - h.a.xyz;
            if ((length(rel) - r * 0.8 * ICOSA_RATIO) / ICOSA_RATIO >= -d) {
                continue;
            }
            d = max(d, r * 0.8 - gauge(rel, 3u));
        } else {
            if ((distance_to_segment(p, h.a.xyz, h.b.xyz) - r * HEX_RATIO) / HEX_RATIO >= -d) {
                continue;
            }
            d = max(d, -tunnel(p, h.a.xyz, h.b.xyz, r).w);
        }
    }
    return d;
}

// The full field at p, given p's bin list.
fn field(s: SmokeSlot, p: vec3<f32>, range: vec2<u32>) -> f32 {
    var d = puffs_distance(s, p, range);
    if (d < CLIP_THRESHOLD) {
        d = max(d, region_distance(s, p));
        if (s.holes.y > 0u) {
            d = apply_holes(s, p, d);
        }
    }
    return d;
}

fn field_at(s: SmokeSlot, p: vec3<f32>) -> f32 {
    let c = bin_of(s, p);
    if (!bin_inside(s, c)) {
        return FAR;
    }
    let range = bin_range(s, c);
    if (range.y == 0u) {
        return FAR;
    }
    return field(s, p, range);
}

// The full field and its analytic gradient: flat facets get exactly flat
// normals, with no finite-difference smearing at creases.
fn field_gradient(s: SmokeSlot, p: vec3<f32>) -> vec4<f32> {
    let c = clamp(bin_of(s, p), vec3<i32>(0), vec3<i32>(s.facet_dims.xyz) - vec3<i32>(1));
    let range = bin_range(s, c);
    let k = s.facet_shape.x;
    var d = FAR;
    var g = vec3<f32>(0.0, 1.0, 0.0);
    let base = s.facet_ofs.x + range.x;
    for (var j = 0u; j < range.y; j++) {
        let puff = facet_puffs[s.facet_ofs.y + facet_indices[base + j]];
        let r = puff.center_radius.w;
        if (r <= 0.0) {
            continue;
        }
        let rel = p - puff.center_radius.xyz;
        let ratio = puff.rot1.w;
        if ((length(rel) - r * ratio) / ratio >= d + k) {
            continue;
        }
        let q = vec3<f32>(dot(puff.rot0.xyz, rel), dot(puff.rot1.xyz, rel), dot(puff.rot2.xyz, rel));
        let gg = gauge_grad(q, u32(puff.rot0.w));
        let b = gg.w - r;
        let nb = puff.rot0.xyz * gg.x + puff.rot1.xyz * gg.y + puff.rot2.xyz * gg.z;
        if (k <= 0.0) {
            if (b < d) {
                d = b;
                g = nb;
            }
        } else {
            // d smin(a, b) = (1 - h/2) d a + (h/2) d b, with a the smaller.
            let h = max(k - abs(d - b), 0.0) / k;
            if (d <= b) {
                g = g * (1.0 - 0.5 * h) + nb * (0.5 * h);
                d = d - h * h * k * 0.25;
            } else {
                g = nb * (1.0 - 0.5 * h) + g * (0.5 * h);
                d = b - h * h * k * 0.25;
            }
        }
    }
    let reg = region_distance(s, p);
    if (reg > d) {
        d = reg;
        g = region_gradient(s, p);
    }
    for (var i = 0u; i < s.holes.y; i++) {
        let h = globals.holes[s.holes.x + i];
        let strength = h.b.w;
        if (strength <= 0.0) {
            continue;
        }
        let r = h.a.w * min(strength * 1.5, 1.0);
        var hole: vec4<f32>;
        if (all(h.a.xyz == h.b.xyz)) {
            let gg = gauge_grad(p - h.a.xyz, 3u);
            hole = vec4<f32>(gg.xyz, gg.w - r * 0.8);
        } else {
            hole = tunnel(p, h.a.xyz, h.b.xyz, r);
        }
        if (-hole.w > d) {
            d = -hole.w;
            g = -hole.xyz;
        }
    }
    return vec4<f32>(g, d);
}

struct Trace {
    // Hit distance (or the end of the traced interval).
    t: f32,
    hit: bool,
    // Closest approach of a miss, in pixels, and where it happened.
    near_ratio: f32,
    near_t: f32,
    // First distance at which the ray came within `near_px` pixels of the
    // surface (or hit it): where a refining ray may safely start.
    first_near: f32,
    // Last such distance (0 if none): past it, a missing ray saw nothing.
    last_near: f32,
}

// Sphere-trace one faceted smoke between t0 and t1.
fn trace_slot(
    si: u32,
    ro: vec3<f32>,
    rd: vec3<f32>,
    inv_rd: vec3<f32>,
    t0: f32,
    t1: f32,
    px_angle: f32,
    tol_px: f32,
    near_px: f32,
    max_steps: u32,
) -> Trace {
    let s = globals.slots[si];
    var out: Trace;
    out.t = t1;
    out.hit = false;
    out.near_ratio = 1e9;
    out.near_t = t0;
    out.first_near = 1e30;
    out.last_near = 0.0;
    let k = s.facet_shape.x;
    var t = t0;
    var steps = 0u;
    // Near the surface the bounding spheres can't help; skip them.
    var near = false;
    loop {
        if (t >= t1) {
            break;
        }
        if (steps >= max_steps) {
            // Out of steps inside the smoke's box: draw smoke here rather than
            // risk showing what's behind it ("fail closed").
            out.hit = true;
            out.t = t;
            out.first_near = min(out.first_near, t);
            out.last_near = t;
            return out;
        }
        steps += 1u;
        let p = ro + rd * t;
        let footprint = max(t * px_angle, 1e-7);
        let eps = max(footprint * tol_px, 5e-4);
        let c = bin_of(s, p);
        if (!bin_inside(s, c)) {
            // On the box edge (rounding): nudge in.
            t += eps;
            continue;
        }
        let to_exit = bin_exit(s, c, ro, inv_rd) - t + 1e-3;
        let range = bin_range(s, c);
        if (range.y == 0u) {
            t += max(to_exit, eps);
            continue;
        }
        // Far from every listed puff: step on bounding spheres alone.
        if (!near) {
            let bound = puffs_bound(s, p, range) - k * 0.25;
            if (bound > max(NEAR_SURFACE, near_px * footprint)) {
                t += max(min(bound * 0.95, to_exit), eps);
                continue;
            }
        }
        let d = field(s, p, range);
        near = d < NEAR_SURFACE;
        if (d < eps) {
            out.hit = true;
            out.t = t;
            out.first_near = min(out.first_near, t);
            out.last_near = t;
            return out;
        }
        let ratio = d / footprint;
        if (ratio < out.near_ratio) {
            out.near_ratio = ratio;
            out.near_t = t;
        }
        if (ratio < near_px) {
            out.first_near = min(out.first_near, t);
            out.last_near = t;
        }
        t += max(min(d * 0.9, to_exit), eps);
    }
    return out;
}

struct Hit {
    trace: Trace,
    // Slot of the hit, and of the closest miss.
    slot: u32,
    near_slot: u32,
}

fn safe_inverse(v: vec3<f32>) -> vec3<f32> {
    return 1.0 / select(v, vec3<f32>(1e-6), abs(v) < vec3<f32>(1e-6));
}

// Trace every faceted smoke along a ray; nearest hit wins.
fn trace_all(
    ro: vec3<f32>,
    rd: vec3<f32>,
    t_start: f32,
    t_scene: f32,
    px_angle: f32,
    tol_px: f32,
    near_px: f32,
    max_steps: u32,
) -> Hit {
    var best: Hit;
    best.trace.t = t_scene;
    best.trace.hit = false;
    best.trace.near_ratio = 1e9;
    best.trace.near_t = 0.0;
    best.trace.first_near = 1e30;
    best.trace.last_near = 0.0;
    best.slot = 0u;
    best.near_slot = 0u;
    let inv_rd = safe_inverse(rd);
    for (var i = 0u; i < globals.counts.x; i++) {
        let s = globals.slots[i];
        if (!is_faceted(s)) {
            continue;
        }
        let span = ray_aabb(ro, inv_rd, s.facet_grid.xyz, bins_max(s));
        let a = max(span.x, t_start);
        let b = min(span.y, best.trace.t);
        if (b <= a) {
            continue;
        }
        let r = trace_slot(i, ro, rd, inv_rd, a, b, px_angle, tol_px, near_px, max_steps);
        if (r.hit && r.t < best.trace.t) {
            best.trace.hit = true;
            best.trace.t = r.t;
            best.slot = i;
        }
        if (r.near_ratio < best.trace.near_ratio) {
            best.trace.near_ratio = r.near_ratio;
            best.trace.near_t = r.near_t;
            best.near_slot = i;
        }
        best.trace.first_near = min(best.trace.first_near, r.first_near);
        best.trace.last_near = max(best.trace.last_near, r.last_near);
    }
    return best;
}

// The slot whose smoke contains p, or -1.
fn inside_slot(p: vec3<f32>) -> i32 {
    for (var i = 0u; i < globals.counts.x; i++) {
        let s = globals.slots[i];
        if (is_faceted(s) && field_at(s, p) < 0.0) {
            return i32(i);
        }
    }
    return -1;
}

struct Ray {
    ro: vec3<f32>,
    rd: vec3<f32>,
}

// Ray through a point of the viewport (in pixels).
fn pixel_ray(local: vec2<f32>) -> Ray {
    let uv = local / view.target_size.xy;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let near_point = view.world_from_clip * vec4<f32>(ndc, 1.0, 1.0);
    var r: Ray;
    r.ro = view.camera_position.xyz;
    r.rd = normalize(near_point.xyz / near_point.w - r.ro);
    return r;
}

// Distance along the ray through `local` to a depth-buffer value.
fn scene_distance(local: vec2<f32>, rd: vec3<f32>, raw_depth: f32) -> f32 {
    if (raw_depth <= 0.0) {
        return 1e30;
    }
    let uv = local / view.target_size.xy;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let scene = view.world_from_clip * vec4<f32>(ndc, raw_depth, 1.0);
    return dot(scene.xyz / scene.w - view.camera_position.xyz, rd);
}

// Soft-edged hard shadow towards the sun through one smoke's puffs.
fn shadow_ray(si: u32, p0: vec3<f32>, l: vec3<f32>, max_steps: u32) -> f32 {
    let s = globals.slots[si];
    let inv = safe_inverse(l);
    let t_end = ray_aabb(p0, inv, s.facet_grid.xyz, bins_max(s)).y;
    let k = s.facet_shape.x;
    var t = 0.0;
    var res = 1.0;
    var steps = 0u;
    var near = false;
    loop {
        if (t >= t_end || steps >= max_steps) {
            break;
        }
        steps += 1u;
        let p = p0 + l * t;
        let c = bin_of(s, p);
        if (!bin_inside(s, c)) {
            break;
        }
        let to_exit = bin_exit(s, c, p0, inv) - t + 1e-3;
        let range = bin_range(s, c);
        if (range.y == 0u) {
            t += max(to_exit, 1e-3);
            continue;
        }
        if (!near) {
            let bound = puffs_bound(s, p, range) - k * 0.25;
            if (bound > NEAR_SURFACE) {
                t += max(min(bound * 0.95, to_exit), 1e-3);
                continue;
            }
        }
        let d = field(s, p, range);
        near = d < NEAR_SURFACE;
        if (d < 1e-3) {
            return 0.0;
        }
        res = min(res, 16.0 * d / max(t, 0.02));
        t += max(min(d * 0.9, to_exit), 1e-3);
    }
    return clamp(res, 0.0, 1.0);
}

// Distance-field ambient occlusion: three taps along the normal.
fn occlusion(s: SmokeSlot, p: vec3<f32>, n: vec3<f32>) -> f32 {
    let d0 = field_at(s, p + n * 0.06);
    let d1 = field_at(s, p + n * 0.16);
    let d2 = field_at(s, p + n * 0.34);
    let occ = clamp((0.06 - d0) / 0.06, 0.0, 1.0) * 0.5
        + clamp((0.16 - d1) / 0.16, 0.0, 1.0) * 0.3
        + clamp((0.34 - d2) / 0.34, 0.0, 1.0) * 0.2;
    return 1.0 - occ * 0.55;
}

// Diffuse light from point lights (muzzle flashes, explosions), unshadowed.
fn point_lights(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    for (var i = 0u; i < globals.counts.y; i++) {
        let light = globals.lights[i];
        let to_light = light.position_range.xyz - p;
        let d2 = dot(to_light, to_light);
        let r2 = light.position_range.w * light.position_range.w;
        if (d2 >= r2) {
            continue;
        }
        let f = d2 / r2;
        let window = clamp(1.0 - f * f, 0.0, 1.0);
        let ndl = max(dot(n, to_light * inverseSqrt(max(d2, 1e-8))), 0.0);
        sum += light.color.rgb * (window * window / max(d2, 0.01)) * ndl / PI;
    }
    return sum;
}

// Sun and sky visibility of a surface point, 0..1: the expensive part of
// shading (shadow rays, occlusion taps), which the full-resolution pass can
// reuse from the coarse pre-pass.
struct Lighting {
    sun: f32,
    ambient: f32,
}

// Unit normal of the smoke surface at (or near) p.
fn surface_normal(s: SmokeSlot, p: vec3<f32>, rd: vec3<f32>) -> vec3<f32> {
    let gd = field_gradient(s, p);
    let glen = length(gd.xyz);
    return select(-rd, gd.xyz / max(glen, 1e-8), glen > 1e-6);
}

// Wrapped diffuse factor of the sun; 0 without a sun or facing away from it.
fn sun_wrap(n: vec3<f32>) -> f32 {
    if (globals.sun_direction.w <= 0.5) {
        return 0.0;
    }
    return max((dot(n, globals.sun_direction.xyz) + 0.12) / 1.12, 0.0);
}

fn source_at(s: SmokeSlot, p: vec3<f32>) -> vec4<f32> {
    return textureSampleLevel(source_atlas, linear_clamp, atlas_uvw(s, p), 0.0);
}

// Baked lighting is read a little outside the surface, where the volumetric
// density doesn't shadow the facet itself.
fn baked_at(s: SmokeSlot, p: vec3<f32>, n: vec3<f32>) -> vec4<f32> {
    return textureSampleLevel(lit_atlas, linear_clamp, atlas_uvw(s, p + n * 0.15), 0.0);
}

// Sun visibility: level shadows from the fill times traced self-shadows, or
// the baked volumetric estimate. 0 where the facet faces away from the sun,
// where it doesn't matter.
fn sun_visibility(si: u32, s: SmokeSlot, p: vec3<f32>, n: vec3<f32>) -> f32 {
    if (sun_wrap(n) <= 0.0) {
        return 0.0;
    }
    if ((view.facet.w & FLAG_TRACED_SHADOWS) != 0u) {
        return source_at(s, p).g * shadow_ray(si, p + n * 0.02, globals.sun_direction.xyz, view.facet.z);
    }
    return baked_at(s, p, n).g;
}

// Sky visibility: open-air factor from the fill times traced occlusion, or
// the baked estimate.
fn ambient_visibility(s: SmokeSlot, p: vec3<f32>, n: vec3<f32>) -> f32 {
    if ((view.facet.w & FLAG_TRACED_OCCLUSION) != 0u) {
        return mix(globals.lighting.y, 1.0, source_at(s, p).b) * occlusion(s, p, n);
    }
    return baked_at(s, p, n).a;
}

// Both terms, sharing texture fetches.
fn lighting(si: u32, s: SmokeSlot, p: vec3<f32>, n: vec3<f32>) -> Lighting {
    let flags = view.facet.w;
    let src = source_at(s, p);
    let baked = baked_at(s, p, n);
    var out: Lighting;
    out.sun = 0.0;
    if (sun_wrap(n) > 0.0) {
        out.sun = baked.g;
        if ((flags & FLAG_TRACED_SHADOWS) != 0u) {
            out.sun = src.g * shadow_ray(si, p + n * 0.02, globals.sun_direction.xyz, view.facet.z);
        }
    }
    out.ambient = baked.a;
    if ((flags & FLAG_TRACED_OCCLUSION) != 0u) {
        out.ambient = mix(globals.lighting.y, 1.0, src.b) * occlusion(s, p, n);
    }
    return out;
}

// Radiance (exposed) of the smoke surface at p with normal n.
fn radiance(si: u32, p: vec3<f32>, n: vec3<f32>, light: Lighting) -> vec3<f32> {
    let s = globals.slots[si];
    var l = globals.sun_color.rgb * (sun_wrap(n) * light.sun / PI)
        + globals.ambient.rgb * light.ambient * (0.75 + 0.25 * n.y);
    if (globals.counts.y > 0u) {
        l += point_lights(p, n);
    }
    return globals.medium.rgb * s.tint.rgb * l * view.march.x;
}

// Radiance of the smoke surface at p seen along rd, everything per pixel.
fn shade(si: u32, p: vec3<f32>, rd: vec3<f32>) -> vec3<f32> {
    let s = globals.slots[si];
    let n = surface_normal(s, p, rd);
    return radiance(si, p, n, lighting(si, s, p, n));
}

// Octahedral normal encoding, for the coarse texture.
fn oct_encode(n: vec3<f32>) -> vec2<f32> {
    let p = n.xy / (abs(n.x) + abs(n.y) + abs(n.z));
    if (n.z >= 0.0) {
        return p;
    }
    return (1.0 - abs(p.yx)) * select(vec2<f32>(-1.0), vec2<f32>(1.0), p >= vec2<f32>(0.0));
}

fn oct_decode(e: vec2<f32>) -> vec3<f32> {
    var n = vec3<f32>(e, 1.0 - abs(e.x) - abs(e.y));
    if (n.z < 0.0) {
        n = vec3<f32>((1.0 - abs(n.yx)) * select(vec2<f32>(-1.0), vec2<f32>(1.0), n.xy >= vec2<f32>(0.0)), n.z);
    }
    return normalize(n);
}

// Lighting (8 bits each) and slot (16 bits) in one word.
fn pack_lighting(l: Lighting, slot: u32) -> u32 {
    let q = vec2<u32>(round(clamp(vec2<f32>(l.sun, l.ambient), vec2<f32>(0.0), vec2<f32>(1.0)) * 255.0));
    return q.x | (q.y << 8u) | (slot << 16u);
}

// What a camera inside a smoke sees: flat, softly lit smoke.
fn whiteout(si: u32) -> vec3<f32> {
    let s = globals.slots[si];
    let light = globals.ambient.rgb * 0.75 + globals.sun_color.rgb * (0.12 / PI);
    return globals.medium.rgb * s.tint.rgb * light * view.march.x;
}
