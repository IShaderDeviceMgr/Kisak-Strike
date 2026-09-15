// Group 3 for the shaders whose `ContextBinding` is `ModelLighting`: the
// ambient cube, four local lights, and the two helpers that both of them
// evaluate the same way.
//
// `common_vertexlitgeneric_dx9.h` and the lighting half of `common_vs_fxc.h`.
// Concatenated between `shaders/prelude.wgsl` and the shader body by
// `ShaderKind::wgsl`, for `VertexLitGeneric` and `Phong` only.
//
// **Why this is a third file and not part of the prelude.** Group 3's *layout*
// is per shader (`shader::ContextBinding`), so a `@group(3)` declaration
// cannot live in something every shader includes: `LightmappedGeneric` binds
// an atlas page at group 3 binding 0 and `Refract` binds a copy of the frame
// there. What can be shared is the declaration for the shaders that agree, and
// two of them now do — which is what this file is, and is the first time the
// concatenation mechanism has had a piece narrower than "everything".
//
// **Why the diffuse term is not here.** `VertexLitGeneric`'s unbumped path
// lights per vertex with `CosineTermInternal` and `Phong` lights per pixel
// with `DiffuseTerm`, and those are two different functions in the original —
// they disagree about saturation, about squaring the half-Lambert term, and
// about whether a light warp replaces the scalar. Each shader keeps its own.

// Mirrors `uniforms::ModelLighting`. Not part of the material and not part of
// the draw: it is per model *instance*, which is what
// `R_StudioSetupLighting` computes once and every mesh of that model then
// shares. `Pass::set_model_lighting` is the setter.

struct Light {
    // rgb, and w = 1 for a directional light.
    color: vec4<f32>,
    // xyz, and w = 1 for a spot light.
    direction: vec4<f32>,
    position: vec4<f32>,
    // falloff, thetaDot, phiDot, 1/(thetaDot - phiDot).
    spot: vec4<f32>,
    // constant, linear, quadratic.
    attenuation: vec4<f32>,
}

struct ModelLighting {
    // +x, -x, +y, -y, +z, -z, in linear space.
    ambient_cube: array<vec4<f32>, 6>,
    lights: array<Light, 4>,
    count: u32,
    static_light: u32,
    ambient_light: u32,
    pad0: u32,
}

@group(3) @binding(0) var<uniform> lighting: ModelLighting;

// `PixelShaderAmbientLight` (`common_vertexlitgeneric_dx9.h:38`).
//
// Valve has two spellings of this — a vertex one that indexes the cube array
// dynamically and a pixel one that does not — and they compute the same thing.
// The pixel form is used for both here because WGSL cannot dynamically index a
// value array, and because "the same thing" is not an approximation: the six
// products are the same six products.
//
// The cube is stored `+x, -x, +y, -y, +z, -z`, so `is_negative` picks the odd
// slot. Swapping a pair lights a model from the wrong side, which reads as a
// level built wrong rather than as a shader bug.
fn ambient_light(world_normal: vec3<f32>) -> vec3<f32> {
    if lighting.ambient_light == 0u {
        return vec3<f32>(0.0);
    }
    let n_squared = world_normal * world_normal;
    let is_negative = vec3<f32>(world_normal < vec3<f32>(0.0)) * n_squared;
    let is_positive = n_squared - is_negative;

    return is_positive.x * lighting.ambient_cube[0].rgb
        + is_negative.x * lighting.ambient_cube[1].rgb
        + is_positive.y * lighting.ambient_cube[2].rgb
        + is_negative.y * lighting.ambient_cube[3].rgb
        + is_positive.z * lighting.ambient_cube[4].rgb
        + is_negative.z * lighting.ambient_cube[5].rgb;
}

// `VertexAttenInternal` (`common_vs_fxc.h:733`).
//
// Three terms folded together with two `mix`es rather than branches, which is
// Valve's shape and worth keeping: the light *type* is not a uniform here
// either, it is the `w` of two of the light's own vectors, so a branch would
// be per light rather than per draw.
//
//   distance: 1 / (a0 + a1*d + a2*d²)   -- `dst()` builds (1, d, d²)
//   spot:     saturate( pow( max( 1e-4, (cos - phiDot) * ooDot ), falloff ) )
//   select:   mix( dist, dist * spot, dir.w ) then mix( that, 1, color.w )
//
// The second `mix` is what makes a directional light unattenuated.
fn light_attenuation(light: Light, world_pos: vec3<f32>) -> f32 {
    var light_dir = light.position.xyz - world_pos;
    let dist_squared = dot(light_dir, light_dir);
    let one_over_dist = inverseSqrt(max(dist_squared, 1e-12));
    light_dir = light_dir * one_over_dist;

    // `dst( distSquared, ooDist ).xyz` is (1, d, d²).
    let dist = vec3<f32>(1.0, dist_squared * one_over_dist, dist_squared);
    let distance_atten = 1.0 / max(dot(light.attenuation.xyz, dist), 1e-6);

    let cos_theta = dot(light.direction.xyz, -light_dir);
    var spot_atten = (cos_theta - light.spot.z) * light.spot.w;
    spot_atten = pow(max(1e-4, spot_atten), light.spot.x);
    spot_atten = saturate(spot_atten);

    let atten = mix(distance_atten, distance_atten * spot_atten, light.direction.w);
    return mix(atten, 1.0, light.color.w);
}

// The direction light `i` arrives from, as a unit vector.
//
// `normalize( cLightInfo[i].pos.xyz - worldPos )` for a point or spot light,
// and the light's own `-direction` for a directional one, selected by
// `color.w` — the negation is Valve's, because `cLightInfo.dir` points the way
// the light shines.
//
// Two things about it that are not obvious:
//
// **The `normalize` is guarded.** The reference writes a plain `normalize`,
// which is a NaN when a light sits exactly on the point being lit — and `mix`
// propagates it even on the branch that discards the result, so one degenerate
// vertex turns a whole surface into garbage rather than a black spot. Valve
// never hits it by construction, twice over: a real light has a real position,
// and `CompilePixelShaderLocalLights` (`shaderapidx8.cpp:8434`) even converts
// a *directional* light into a point light 10,000 units away so that the
// expression stays well-defined.
//
// **That 10,000-unit conversion is why the pixel path needs the `mix` at
// all.** `PixelShaderGetLightVector` (`common_vertexlitgeneric_dx9.h:127`)
// only ever subtracts a position, because `PixelShaderLightInfo` carries no
// direction — the conversion above is what makes that correct for a
// directional light. This port's `uniforms::Light` keeps the direction and
// leaves a directional light's position at the origin, so selecting on
// `color.w` here is the same answer without the sentinel distance.
fn light_direction(light: Light, world_pos: vec3<f32>) -> vec3<f32> {
    let to_light = light.position.xyz - world_pos;
    let point_dir = to_light * inverseSqrt(max(dot(to_light, to_light), 1e-12));
    return mix(point_dir, -light.direction.xyz, light.color.w);
}
