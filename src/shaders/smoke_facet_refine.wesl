// Faceted smoke at reduced resolution, after the coarse pre-pass. Shared by
//   - the classify pass: every pixel of the smoke's rectangle; facet
//     interiors are shaded straight from the four surrounding coarse samples,
//     empty pixels cleared, and everything else queued;
//   - the retrace pass: queued pixels (silhouettes, creases, shadow edges)
//     traced and shaded at full resolution, reusing the coarse lighting where
//     the samples on the same surface agree.
// Queuing keeps the expensive pixels together instead of letting them hold up
// the cheap ones that would share their SIMD groups. Both write premultiplied
// smoke into `layer`, which is composited over the scene afterwards.

import package::shaders::smoke_common::FLAG_DEBUG_SILHOUETTE;
import package::shaders::smoke_facets::{
    view, globals, depth_texture, trace_all, pixel_ray, scene_distance, inside_slot, whiteout,
    Lighting, surface_normal, sun_visibility, ambient_visibility, radiance, oct_decode
};

@group(0) @binding(9) var coarse_in: texture_2d<u32>;
@group(0) @binding(10) var layer: texture_storage_2d<rgba16float, write>;

// Step budget of a retraced pixel. Queued pixels are the hard ones (rays
// grazing a silhouette on their way to a surface behind it) and few, so they
// get the top tier's budget: running out draws smoke, but shaded from the
// wrong surface.
const RETRACE_STEPS: u32 = 128u;

// Normals closer than this (cosine, about 11°) belong to the same facet or
// smoothly curved stretch of surface. Neighbouring faces of the polyhedra are
// 40° or more apart.
const SAME_SURFACE: f32 = 0.98;

struct Coarse {
    first: f32,
    // Hit distance, 1e30 for a miss.
    t: f32,
    // A miss's last near approach: it saw no surface past this.
    last: f32,
    n: vec3<f32>,
    // Sun and sky visibility.
    light: vec2<f32>,
    slot: u32,
}

// The 2×2 coarse samples around the current pixel, their bilinear weights,
// and the world distance between neighbouring samples.
var<private> around: array<Coarse, 4>;
var<private> around_coords: array<vec2<i32>, 4>;
var<private> around_w: vec4<f32>;
var<private> around_spacing: f32;

fn coarse_at(c: vec2<i32>, coarse_max: vec2<i32>) -> Coarse {
    let v = textureLoad(coarse_in, clamp(c, vec2<i32>(0), coarse_max), 0);
    var out: Coarse;
    out.first = bitcast<f32>(v.x);
    let t = bitcast<f32>(v.y);
    out.t = select(1e30, t, t >= 0.0);
    out.last = -t;
    out.n = oct_decode(unpack2x16snorm(v.z));
    out.light = vec2<f32>(f32(v.w & 255u), f32((v.w >> 8u) & 255u)) / 255.0;
    out.slot = v.w >> 16u;
    return out;
}

struct Neighbourhood {
    // Earliest near approach of the four (1e30: none came near smoke).
    first: f32,
    hit_min: f32,
    hit_max: f32,
    // All four hit, and none passed close to anything nearer first: the
    // surface this pixel sees lies within their depth spread plus a ridge's
    // worth of bulge (`margin`).
    regular: bool,
    margin: f32,
    // Where a full-resolution trace of this pixel may start.
    t_start: f32,
}

// Loads the coarse samples around a pixel (viewport coordinates).
fn load_around(local: vec2<f32>) -> Neighbourhood {
    let divisor = f32(view.facet.x);
    let coarse_max = vec2<i32>(ceil(view.target_size.xy / divisor)) - vec2<i32>(1);
    let g = local / divisor - 0.5;
    let base = vec2<i32>(floor(g));
    let f = g - floor(g);
    for (var k = 0; k < 4; k++) {
        around_coords[k] = clamp(base + vec2<i32>(k & 1, k >> 1), vec2<i32>(0), coarse_max);
        around[k] = coarse_at(around_coords[k], coarse_max);
    }
    around_w = vec4<f32>((1.0 - f.x) * (1.0 - f.y), f.x * (1.0 - f.y), (1.0 - f.x) * f.y, f.x * f.y);

    var nb: Neighbourhood;
    nb.first = min(min(around[0].first, around[1].first), min(around[2].first, around[3].first));
    nb.hit_min = min(min(around[0].t, around[1].t), min(around[2].t, around[3].t));
    nb.hit_max = max(max(around[0].t, around[1].t), max(around[2].t, around[3].t));
    let all_hit = nb.hit_max < 1e29;
    around_spacing = view.facet_f.y * divisor * select(nb.first, nb.hit_min, all_hit);
    // Any surface this pixel can show passes within the near threshold of its
    // own block's coarse ray, which notices it no later than 0.72 spacings
    // past it: nothing lies nearer than `first` minus that. By default (a
    // silhouette), start well before it.
    nb.t_start = max(nb.first - (0.1 + 0.03 * nb.first), 0.0);
    nb.regular = false;
    nb.margin = 0.0;
    if (all_hit) {
        nb.margin = (nb.hit_max - nb.hit_min) + 3.0 * around_spacing + 0.01;
        if (nb.first >= nb.hit_min - nb.margin) {
            nb.regular = true;
            nb.t_start = max(nb.hit_min - nb.margin - around_spacing, 0.0);
        }
    }
    return nb;
}

// Hit point of coarse sample k.
fn sample_point(k: i32, ro: vec3<f32>) -> vec3<f32> {
    let divisor = f32(view.facet.x);
    let local = min((vec2<f32>(around_coords[k]) + 0.5) * divisor, view.target_size.xy - 0.5);
    return ro + pixel_ray(local).rd * around[k].t;
}

// Where the samples sit on facets meeting at convex edges (the edges of one
// polyhedron, or a puff against the region clip or a tunnel wall), the
// surface along the ray is the farthest of their planes: the ray is outside
// the solid until it has entered every half-space. Returns the hit distance
// and the sample whose facet it is, or a negative distance if the corner
// isn't convex (a concave crease between puffs needs a trace).
fn convex_corner(ro: vec3<f32>, rd: vec3<f32>, nb: Neighbourhood) -> vec2<f32> {
    var points: array<vec3<f32>, 4>;
    for (var k = 0; k < 4; k++) {
        points[k] = sample_point(k, ro);
    }
    // Coarse hits stop up to one hit tolerance outside the surface.
    let tol = view.facet_f.x * around_spacing / f32(view.facet.x) + 0.002;
    var best_t = -1.0;
    var best_k = 0;
    for (var k = 0; k < 4; k++) {
        let n = around[k].n;
        let facing = dot(n, rd);
        if (facing > -0.05) {
            return vec2<f32>(-1.0, 0.0);
        }
        let tk = dot(n, points[k] - ro) / facing;
        if (tk > best_t) {
            best_t = tk;
            best_k = k;
        }
        for (var j = 0; j < 4; j++) {
            if (dot(n, points[j] - points[k]) > tol) {
                return vec2<f32>(-1.0, 0.0);
            }
        }
    }
    if (best_t < nb.hit_min - nb.margin || best_t > nb.hit_max + nb.margin) {
        return vec2<f32>(-1.0, 0.0);
    }
    return vec2<f32>(best_t, f32(best_k));
}

struct Gathered {
    light: Lighting,
    sun_ok: bool,
    ambient_ok: bool,
}

// Lighting of a surface point from the coarse samples on the same surface
// (same smoke, similar depth and normal). A term is usable only where those
// samples agree to within the shading tolerance.
fn gather_lighting(si: u32, t: f32, n: vec3<f32>) -> Gathered {
    let depth_tol = 4.0 * around_spacing + 0.02;
    var wsum = 0.0;
    var sum = vec2<f32>(0.0);
    var lo = vec2<f32>(1.0);
    var hi = vec2<f32>(0.0);
    for (var k = 0; k < 4; k++) {
        let c = around[k];
        if (c.t < 1e29 && c.slot == si && abs(c.t - t) <= depth_tol && dot(c.n, n) > SAME_SURFACE) {
            let w = around_w[k] + 1e-3;
            wsum += w;
            sum += c.light * w;
            lo = min(lo, c.light);
            hi = max(hi, c.light);
        }
    }
    let tol = view.facet_f.z;
    var out: Gathered;
    let mean = sum / max(wsum, 1e-6);
    out.light.sun = mean.x;
    out.light.ambient = mean.y;
    out.sun_ok = tol > 0.0 && wsum > 0.0 && hi.x - lo.x <= tol;
    out.ambient_ok = tol > 0.0 && wsum > 0.0 && hi.y - lo.y <= tol;
    return out;
}

// Radiance of a traced surface point (or near miss) at distance t: the
// normal per pixel, lighting from the coarse samples where they agree.
fn surface_color(si: u32, p: vec3<f32>, rd: vec3<f32>, t: f32) -> vec3<f32> {
    let s = globals.slots[si];
    let n = surface_normal(s, p, rd);
    var g = gather_lighting(si, t, n);
    if (!g.sun_ok) {
        g.light.sun = sun_visibility(si, s, p, n);
    }
    if (!g.ambient_ok) {
        g.light.ambient = ambient_visibility(s, p, n);
    }
    return radiance(si, p, n, g.light);
}

fn finish(light: vec3<f32>) -> vec3<f32> {
    if ((view.facet.w & FLAG_DEBUG_SILHOUETTE) != 0u) {
        return vec3<f32>(1.0, 0.0, 1.0);
    }
    return light;
}

fn store(px: vec2<u32>, v: vec4<f32>) {
    textureStore(layer, vec2<i32>(px), v);
}

fn pixel_scene_distance(px: vec2<u32>, local: vec2<f32>, rd: vec3<f32>) -> f32 {
    let raw = textureLoad(depth_texture, vec2<i32>(px) + vec2<i32>(view.viewport.xy), 0);
    return scene_distance(local, rd, raw);
}

// Classify pass: stores the pixel if it is empty or a facet interior, and
// returns true if it needs a full-resolution trace.
fn classify_pixel(px: vec2<u32>) -> bool {
    let local = vec2<f32>(px) + 0.5;
    let ray = pixel_ray(local);

    let inside = inside_slot(ray.ro);
    if (inside >= 0) {
        store(px, vec4<f32>(finish(whiteout(u32(inside))), 1.0));
        return false;
    }

    let nb = load_around(local);
    if (nb.first >= 1e29) {
        // None of the four came near any smoke.
        store(px, vec4<f32>(0.0));
        return false;
    }
    let t_scene = pixel_scene_distance(px, local, ray.rd);
    if (t_scene <= nb.t_start) {
        // The scene is in front of any smoke this pixel could show.
        store(px, vec4<f32>(0.0));
        return false;
    }

    // Inside the silhouette: one smoke, smooth depth, and the scene behind
    // all of it. A facet interior (one normal) or a convex edge between
    // facets is reconstructed from the samples and, if their lighting
    // agrees, shaded without tracing.
    let slot = around[0].slot;
    if (nb.regular && view.facet_f.z > 0.0 && t_scene > nb.hit_max + nb.margin
        && nb.hit_max - nb.hit_min <= 8.0 * around_spacing + 0.01
        && around[1].slot == slot && around[2].slot == slot && around[3].slot == slot) {
        var n = normalize(
            around[0].n * around_w.x + around[1].n * around_w.y + around[2].n * around_w.z
                + around[3].n * around_w.w,
        );
        let flat = min(
            min(dot(around[0].n, n), dot(around[1].n, n)),
            min(dot(around[2].n, n), dot(around[3].n, n)),
        ) > SAME_SURFACE;
        var t = -1.0;
        if (flat) {
            t = dot(around_w, vec4<f32>(around[0].t, around[1].t, around[2].t, around[3].t));
        } else {
            let corner = convex_corner(ray.ro, ray.rd, nb);
            t = corner.x;
            n = around[i32(corner.y)].n;
        }
        if (t > 0.0) {
            let g = gather_lighting(slot, t, n);
            if (g.sun_ok && g.ambient_ok) {
                let p = ray.ro + ray.rd * t;
                store(px, vec4<f32>(finish(radiance(slot, p, n, g.light)), 1.0));
                return false;
            }
        }
    }

    // Everything else: cleared now, traced by the retrace pass.
    store(px, vec4<f32>(0.0));
    return true;
}

// Retrace pass: trace and shade one queued pixel at full resolution.
fn retrace_pixel(px: vec2<u32>) {
    let local = vec2<f32>(px) + 0.5;
    let ray = pixel_ray(local);
    let nb = load_around(local);
    var t_scene = pixel_scene_distance(px, local, ray.rd);
    // If the coarse ray of this pixel's own block missed, any surface this
    // pixel could show would have come within its near threshold, so nothing
    // lies past its last near approach (plus the sampling slack).
    let divisor = f32(view.facet.x);
    let own = clamp(vec2<i32>(floor(local / divisor)), around_coords[0], around_coords[3]);
    let own_k = i32(own.x != around_coords[0].x) + 2 * i32(own.y != around_coords[0].y);
    let own_sample = around[own_k];
    if (own_sample.t >= 1e29) {
        let slack = 2.0 * view.facet_f.y * divisor * own_sample.last + 0.01;
        t_scene = min(t_scene, own_sample.last + slack);
    }
    let px_angle = view.facet_f.y;
    var hit = trace_all(ray.ro, ray.rd, nb.t_start, t_scene, px_angle, view.facet_f.x, 0.0, RETRACE_STEPS);
    if (hit.trace.hit && nb.t_start > 0.0 && hit.trace.t <= nb.t_start + 1e-3) {
        // Started inside a surface nearer than the coarse rays saw: trace
        // this pixel from the camera instead.
        hit = trace_all(ray.ro, ray.rd, 0.0, t_scene, px_angle, view.facet_f.x, 0.0, RETRACE_STEPS);
    }
    if (hit.trace.hit) {
        let t = hit.trace.t;
        store(px, vec4<f32>(finish(surface_color(hit.slot, ray.ro + ray.rd * t, ray.rd, t)), 1.0));
        return;
    }
    // Anti-aliasing: a miss that passed within half a pixel of the surface.
    let coverage = clamp(0.5 - hit.trace.near_ratio, 0.0, 1.0);
    if (coverage > 0.0) {
        let t = hit.trace.near_t;
        let color = finish(surface_color(hit.near_slot, ray.ro + ray.rd * t, ray.rd, t));
        store(px, vec4<f32>(color * coverage, coverage));
    }
}
