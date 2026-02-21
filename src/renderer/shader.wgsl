struct Globals {
    screen_size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0)
var atlas_tex: texture_2d<f32>;
@group(0) @binding(1)
var atlas_sampler: sampler;
@group(0) @binding(2)
var<uniform> globals: Globals;

struct VsIn {
    @location(0) quad_pos: vec2<f32>,
    @location(1) quad_uv: vec2<f32>,
    @location(2) inst_pos: vec2<f32>,
    @location(3) inst_size: vec2<f32>,
    @location(4) uv_min: vec2<f32>,
    @location(5) uv_max: vec2<f32>,
    @location(6) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    let pixel_pos = input.inst_pos + input.quad_pos * input.inst_size;
    let ndc = vec2<f32>(
        (pixel_pos.x / globals.screen_size.x) * 2.0 - 1.0,
        1.0 - (pixel_pos.y / globals.screen_size.y) * 2.0,
    );

    out.clip_pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = input.uv_min + input.quad_uv * (input.uv_max - input.uv_min);
    out.color = input.color;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    let sample_alpha = textureSample(atlas_tex, atlas_sampler, input.uv).r;
    return vec4<f32>(input.color.rgb, input.color.a * sample_alpha);
}
