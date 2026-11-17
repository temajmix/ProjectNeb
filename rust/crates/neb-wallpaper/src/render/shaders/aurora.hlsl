// ProjectNeb living wallpaper: the aurora scene.
//
// One fullscreen triangle, no vertex buffer, no textures: the vertex stage invents the triangle from
// SV_VertexID and the pixel stage evaluates three drifting colour blobs, a slow noise field and a
// vignette. Everything the scene needs arrives in the 64-byte `b0` block that
// `render/scene.rs::Frame` writes, so the animation's timing is testable in Rust.

cbuffer Frame : register(b0)
{
    float4 g_params;    // x = seconds, y = width, z = height, w = intensity
    float4 g_colour_a;  // linear rgb
    float4 g_colour_b;
    float4 g_colour_c;
};

struct VsOut
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

// The three blob centres, in the same parameterisation as `scene::blob_centre`, so a unit test can
// reason about where they are. Kept in one place because the shader cannot be unit tested.
float2 blob_centre(uint index, float time)
{
    const float loop_seconds = 64.0;
    const float tau = 6.28318530718;
    float angle = tau * frac(time / loop_seconds) + tau * (float)index / 3.0;
    float s, c;
    sincos(angle, s, c);

    if (index == 0)
    {
        return float2(0.5 + 0.30 * c, 0.34 + 0.16 * sin(2.0 * angle));
    }

    if (index == 1)
    {
        return float2(0.5 + 0.34 * sin(angle + 2.0), 0.62 + 0.14 * cos(3.0 * angle));
    }

    return float2(0.5 + 0.26 * cos(2.0 * angle + 1.0), 0.5 + 0.22 * s);
}

float hash21(float2 p)
{
    p = frac(p * float2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return frac(p.x * p.y);
}

// Bilinear value noise: cheap, and at this scale the aurora reads as haze rather than as a pattern.
float value_noise(float2 p)
{
    float2 cell = floor(p);
    float2 local = frac(p);
    float2 smooth = local * local * (3.0 - 2.0 * local);

    float a = hash21(cell);
    float b = hash21(cell + float2(1.0, 0.0));
    float c = hash21(cell + float2(0.0, 1.0));
    float d = hash21(cell + float2(1.0, 1.0));

    return lerp(lerp(a, b, smooth.x), lerp(c, d, smooth.x), smooth.y);
}

float fbm(float2 p)
{
    float total = 0.0;
    float amplitude = 0.5;

    for (int octave = 0; octave < 4; ++octave)
    {
        total += value_noise(p) * amplitude;
        p *= 2.03;
        amplitude *= 0.5;
    }

    return total;
}

// One blob added to `colour`. Written out per blob rather than looped over an array: the shader
// compiler unrolls this anyway, and a literal body has no dynamic indexing to get wrong.
float3 add_blob(float3 colour, float2 uv, float aspect, uint index, float3 tint, float time)
{
    float2 centre = blob_centre(index, time);
    float2 delta = (float2(uv.x * aspect, uv.y) - centre * float2(aspect, 1.0)) * float2(1.0, 1.35);
    float weight = exp(-dot(delta, delta) * 7.5);

    // A drifting sine band inside each blob gives the aurora its curtains.
    float curtain = 0.55 + 0.45 * sin(uv.y * 11.0 + time * 0.9 + (float)index * 2.1);

    return colour + tint * weight * (0.65 + 0.35 * curtain);
}

VsOut vs_main(uint vertex_id : SV_VertexID)
{
    // (0,0) (2,0) (0,2) in uv space is one triangle that covers the whole viewport.
    float2 uv = float2((float)((vertex_id << 1) & 2), (float)(vertex_id & 2));

    VsOut output;
    output.position = float4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    output.uv = uv;
    return output;
}

float4 ps_main(VsOut input) : SV_Target
{
    float time = g_params.x;
    float2 size = max(g_params.yz, float2(1.0, 1.0));
    float intensity = max(g_params.w, 0.0);

    // Aspect-corrected coordinates: the blobs are round on a 21:9 monitor as well as on a 16:9 one.
    float aspect = size.x / size.y;
    float2 uv = input.uv;

    // The haze the blobs sit in: a slow noise field that keeps the flat background from looking like
    // a solid colour behind the icons.
    float haze = fbm(float2(uv.x * aspect * 2.4 + time * 0.05, uv.y * 3.7 - time * 0.03));
    float3 colour = lerp(g_colour_a.rgb * 0.10, g_colour_b.rgb * 0.14, haze) * (0.4 + 0.6 * haze);

    colour = add_blob(colour, uv, aspect, 0, g_colour_a.rgb, time);
    colour = add_blob(colour, uv, aspect, 1, g_colour_b.rgb, time);
    colour = add_blob(colour, uv, aspect, 2, g_colour_c.rgb, time);

    // A dark floor at the bottom and a soft vignette at the edges keep the taskbar and the icons
    // readable no matter how bright the scene gets.
    float2 centred = uv - 0.5;
    float vignette = 1.0 - 0.30 * dot(centred, centred) * 2.0;
    float floor_fade = lerp(0.6, 1.0, smoothstep(1.0, 0.35, uv.y));

    colour *= vignette * floor_fade * intensity;

    // A touch of dither in 8-bit steps, so the dark gradients do not band on a large monitor.
    float dither = (hash21(uv * size + time) - 0.5) / 255.0;

    return float4(saturate(colour + dither), 1.0);
}
