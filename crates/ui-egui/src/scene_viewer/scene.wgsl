struct View {
    projection: mat4x4<f32>,
    material: vec4<f32>,
    accent: vec4<f32>,
    selected: vec4<u32>,
}
@group(0) @binding(0) var<uniform> view: View;
struct Vertex {
    @location(0) position: vec3<f32>,
    @location(1) a: vec4<f32>,
    @location(2) b: vec4<f32>,
    @location(3) c: vec4<f32>,
    @location(4) d: vec4<f32>,
    @location(5) id: u32,
    @location(6) visible: u32,
    @location(7) color: u32,
}
struct Fragment {
    @builtin(position) clip: vec4<f32>,
    @location(0) position: vec3<f32>,
    @location(1) @interpolate(flat) id: u32,
    @location(2) @interpolate(flat) color: u32,
}
@vertex fn vertex(v: Vertex) -> Fragment {
    var out: Fragment;
    let world = mat4x4<f32>(v.a,v.b,v.c,v.d) * vec4<f32>(v.position,1.0);
    out.position = world.xyz;
    out.clip = view.projection * world;
    if v.visible == 0u { out.clip = vec4<f32>(2.0,2.0,2.0,1.0); }
    out.id = v.id;
    out.color = v.color;
    return out;
}
@fragment fn fragment(v: Fragment) -> @location(0) vec4<f32> {
    let raw = cross(dpdx(v.position),dpdy(v.position));
    let n = raw / max(length(raw),0.0000001);
    let light = 0.35 + 0.65 * abs(dot(n, normalize(vec3<f32>(0.4,-0.6,0.8))));
    var color = view.material;
    if (v.color >> 24u) != 0u { color=vec4<f32>(f32(v.color & 255u),f32((v.color>>8u)&255u),f32((v.color>>16u)&255u),255.0)/255.0; }
    if v.id == view.selected.x { color = view.accent; }
    return vec4<f32>(color.rgb * light,1.0);
}
