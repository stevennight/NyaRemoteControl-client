// Client presentation: YUV -> RGB with a CPU-supplied matrix, drawn as a
// 4-vertex strip.

cbuffer Params : register(b0) {
    float4 dst;    // NDC: x0, y_top, x1, y_bottom
    float4 m0;     // RGB = M * ((yuv - off) * scale)
    float4 m1;
    float4 m2;
    float4 yoff;
    float4 yscale;
};

Texture2D t0 : register(t0);
Texture2D t1 : register(t1);
Texture2D t2 : register(t2);
SamplerState samp : register(s0);

struct VSOut {
    float4 pos : SV_Position;
    float2 uv : TEXCOORD0;
};

VSOut vs_quad(uint id : SV_VertexID) {
    float2 t = float2(id & 1, (id >> 1) & 1);
    VSOut o;
    o.pos = float4(lerp(dst.x, dst.z, t.x), lerp(dst.y, dst.w, t.y), 0.0, 1.0);
    o.uv = t;
    return o;
}

float4 to_rgb(float3 yuv) {
    float3 v = (yuv - yoff.xyz) * yscale.xyz;
    return float4(saturate(float3(dot(m0.xyz, v), dot(m1.xyz, v), dot(m2.xyz, v))), 1.0);
}

float4 ps_nv12(VSOut i) : SV_Target {
    return to_rgb(float3(t0.Sample(samp, i.uv).r, t1.Sample(samp, i.uv).rg));
}

float4 ps_ayuv(VSOut i) : SV_Target {
    float4 c = t0.Sample(samp, i.uv); // R=V G=U B=Y
    return to_rgb(float3(c.b, c.g, c.r));
}

float4 ps_planar(VSOut i) : SV_Target {
    return to_rgb(float3(t0.Sample(samp, i.uv).r, t1.Sample(samp, i.uv).r, t2.Sample(samp, i.uv).r));
}
