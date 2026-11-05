// Rotational motion blur of the moving layer, then the sharp layer over it:
// output = top over blur(moving), one invocation per output pixel.
//
// Vello writes straight alpha, sRGB-encoded, and blends in that encoded space. The blur
// averages in linear light, since it stands for the light gathered during the exposure;
// the composite blends encoded values as Vello does, so pixels the blur leaves alone
// come out as a plain Vello render of both layers would.

struct Params {
    center: vec2<f32>, // centre of rotation, pixels
    angle: f32,        // signed arc swept during the exposure, radians
    radius: f32,       // pixels farther than this from the centre do not move
}

@group(0) @binding(0) var moving: texture_2d<f32>;
@group(0) @binding(1) var top: texture_2d<f32>;
@group(0) @binding(2) var linear_clamp: sampler;
@group(0) @binding(3) var output: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(4) var<uniform> params: Params;

// Bounds the cost: past it, rim samples of fast spins drift apart and band slightly.
const MAX_SAMPLES: f32 = 64.0;

// The exact sRGB transfer functions (IEC 61966-2-1), not a 2.2 gamma.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3(2.4)), c / 12.92, c <= vec3(0.04045));
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3(0.0031308));
}

// Rotates v by the angle whose cosine and sine are cs.x and cs.y.
fn rotate(v: vec2<f32>, cs: vec2<f32>) -> vec2<f32> {
    return vec2(v.x * cs.x - v.y * cs.y, v.x * cs.y + v.y * cs.x);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(output)) {
        return; // the dispatch is rounded up to whole workgroups
    }
    let pixel = vec2<i32>(id.xy);
    let offset = vec2<f32>(id.xy) + 0.5 - params.center;
    let r = length(offset);
    // About one sample per pixel of arc at this radius.
    let n = u32(clamp(ceil(abs(params.angle) * r), 1.0, MAX_SAMPLES));

    var color: vec3<f32>;
    if r > params.radius || n == 1u {
        // A single centred sample lands on the pixel itself: load it, no filtering.
        color = textureLoad(moving, pixel, 0).rgb;
    } else {
        // A centred shutter: the midpoints of n equal steps across [-angle/2, +angle/2].
        // Each sample is the previous one turned by one step, which saves a sin and a
        // cos per sample. Every 8th sample is recomputed from its absolute angle, so the
        // platform's sin/cos error cannot compound over more than 7 steps.
        let delta = params.angle / f32(n);
        let turn = vec2(cos(delta), sin(delta));
        let start = 0.5 * (delta - params.angle);
        var v = offset;
        let to_uv = 1.0 / vec2<f32>(textureDimensions(moving));
        var sum = vec3(0.0);
        for (var i = 0u; i < n; i++) {
            if i % 8u == 0u {
                let a = start + f32(i) * delta;
                v = rotate(offset, vec2(cos(a), sin(a)));
            }
            // The moving layer is opaque, so straight alpha filters like premultiplied.
            let filtered = textureSampleLevel(moving, linear_clamp, (params.center + v) * to_uv, 0.0);
            sum += srgb_to_linear(filtered.rgb);
            v = rotate(v, turn);
        }
        color = linear_to_srgb(sum / f32(n));
    }

    let over = textureLoad(top, pixel, 0);
    textureStore(output, pixel, vec4(over.rgb * over.a + color * (1.0 - over.a), 1.0));
}
