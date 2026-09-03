//! Material variables: the values a `.vmt` sets, and the flags it raises.
//!
//! Replaces `materialsystem/cmaterialvar.cpp` (2,105 lines) and the flag half
//! of `materialsystem/cmaterial.cpp`. Almost all of that file is machinery this
//! port does not have: a custom page allocator for the vars themselves, a
//! render-call queue that shadows every write into a temp var
//! (`s_pTempMaterialVar`), tool-framework change recording, and a
//! `CUtlSymbol` table so that a name could be compared as a `uint16`. What is
//! left, and what is here, is a tagged union plus the *coercion rules* between
//! its arms.
//!
//! # Coercion is the whole point
//!
//! `CMaterialVar` stores every representation at once: `SetIntValue` also
//! writes `m_VecVal[0..4]`, `SetStringValue` also runs `atoi`/`atof`, and every
//! getter reads its own field regardless of the declared type
//! (`cmaterialvar.cpp:916-1120`). That is not sloppiness — it is what lets a
//! `.vmt` write `"$color" "1"` where a shader reads a vector, which shipped
//! content does. So [`MaterialVar`] is a plain enum and the coercions live in
//! the accessors, computed on demand instead of eagerly. Same answers, one
//! source of truth.
//!
//! # The type of a value is guessed, not declared
//!
//! Nothing in a `.vmt` says whether `2` is an int, a float or a string. Valve
//! decides in two places that have to be read together: `KeyValues`' text
//! loader sniffs int-vs-float-vs-string with `strtol`/`strtod` end pointers
//! (`tier1/KeyValues.cpp:2620`), and then `CreateMaterialVarFromKeyValue`
//! (`cmaterial.cpp:1085`) takes the strings and looks for matrices and vectors.
//! [`MaterialVar::parse`] is those two layers as one function, which is the
//! only way to see the rule whole.

use std::fmt;

/// A value read from a `.vmt`, or set by code.
///
/// `MaterialVarType_t` (`public/materialsystem/imaterialvar_declarations.h:9`)
/// minus three arms this port does not reproduce:
///
/// | Dropped | Why |
/// |---|---|
/// | `MATERIAL_VAR_TYPE_TEXTURE` | a resolved texture is not a *value*; [`Material`] keeps textures in their own map, so a var never holds a GPU object and this module never touches `wgpu` |
/// | `MATERIAL_VAR_TYPE_MATERIAL` | only `$fallbackmaterial` and material proxies used it, and neither is ported |
/// | `MATERIAL_VAR_TYPE_FOURCC` | an escape hatch for passing app-defined structs between a proxy and a shader (`imaterialvar.h:97`); with proxies unported it has no producer and no consumer |
///
/// `MATERIAL_VAR_TYPE_UNDEFINED` has no arm either: a var that is not defined
/// is simply absent from the material's map, which is what `IsDefined()` asked.
///
/// [`Material`]: super::material::Material
#[derive(Debug, Clone, PartialEq)]
pub enum MaterialVar {
    Float(f32),
    Int(i32),
    /// A `[x y z w]` or `{r g b a}` vector, and how many components the file
    /// actually wrote. Absent components are zero, as `SetVecValue` leaves
    /// them (`cmaterialvar.cpp:1526`); the count is kept because a shader that
    /// asks for three components from a two-component var must see the
    /// difference.
    Vec(Vec4, u8),
    /// Row-major, `m[row][col]`, translation in column 3 — `VMatrix`'s layout,
    /// which is fixed by [`MaterialVar::parse`]'s `center/scale/rotate` forms
    /// having been written against it.
    Matrix(Matrix),
    Str(String),
}

/// Four floats. Vectors in a `.vmt` are always at most four wide.
pub type Vec4 = [f32; 4];

/// A 4x4 matrix, `m[row][col]`. See [`MaterialVar::Matrix`].
pub type Matrix = [[f32; 4]; 4];

/// The identity matrix, and the value an undefined matrix param takes —
/// `InitShaderParameters` (`materialsystem/shadersystem.cpp:906`).
pub const IDENTITY: Matrix = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

impl MaterialVar {
    /// Reads the value written in a `.vmt`.
    ///
    /// The order matters and is Valve's, spread across two files:
    ///
    /// 1. **Empty** — no var at all (`cmaterial.cpp:1102` returns null, and the
    ///    caller drops the key). Reported here as `None`.
    /// 2. **Float**, if `strtod` consumed the whole token *and* got further
    ///    than `strtol` would.
    /// 3. **Int**, if `strtol` consumed the whole token.
    /// 4. **Matrix**, in any of its three spellings — see [`parse_matrix`].
    /// 5. **Vector**, if the first non-blank character is `[` or `{`.
    /// 6. **String**, otherwise.
    ///
    /// Two consequences worth knowing, both of them Valve's:
    ///
    /// - **Whitespace is not trimmed first.** `strtol("1 ")` stops at the
    ///   space, so `" 1 "` is neither an int nor a float and falls through to
    ///   *string*. A shader reading it as a number still gets 1, because
    ///   [`as_f32`](Self::as_f32) runs `atof` — which is exactly why the
    ///   coercions exist.
    /// - **`0x…` is a string.** `strtod` accepts hex under POSIX and
    ///   `KeyValues` explicitly undoes that (`KeyValues.cpp:2628`) so that
    ///   content behaves the same on every platform.
    pub fn parse(text: &str) -> Option<MaterialVar> {
        if text.is_empty() {
            return None;
        }

        let int_end = strtol_end(text);
        let float_end = strtod_end(text);
        let end = text.len();

        if float_end > int_end && float_end == end {
            return Some(MaterialVar::Float(c_atof(text)));
        }
        if int_end == end {
            // `strtol` saturates and sets ERANGE on overflow, and KeyValues
            // treats that as a string rather than a clamped number.
            return match text.trim_start().parse::<i64>() {
                Ok(value) => Some(MaterialVar::Int(value as i32)),
                Err(_) => Some(MaterialVar::Str(text.to_owned())),
            };
        }

        if let Some(matrix) = parse_matrix(text) {
            return Some(MaterialVar::Matrix(matrix));
        }

        if is_vector(text) {
            // A malformed vector is *no var*, not a string: `ParseVectorFrom-
            // KeyValueString` returns 0 and `CreateVectorMaterialVarFromKeyValue`
            // returns null (`cmaterial.cpp:999`).
            let (value, comps) = parse_vector(text)?;
            return Some(MaterialVar::Vec(value, comps));
        }

        Some(MaterialVar::Str(text.to_owned()))
    }

    /// `GetFloatValue`. Reads `m_VecVal[0]`, whatever the arm.
    pub fn as_f32(&self) -> f32 {
        match self {
            MaterialVar::Float(value) => *value,
            MaterialVar::Int(value) => *value as f32,
            MaterialVar::Vec(value, _) => value[0],
            // `SetMatrixValue` zeroes the vector (`cmaterialvar.cpp:1666`).
            MaterialVar::Matrix(_) => 0.0,
            MaterialVar::Str(text) => c_atof(text),
        }
    }

    /// `GetIntValue`. Reads `m_intVal`, which every setter also writes.
    pub fn as_i32(&self) -> i32 {
        match self {
            // C's float-to-int conversion truncates toward zero, and so does
            // Rust's `as` (which additionally saturates instead of being
            // undefined at the extremes).
            MaterialVar::Float(value) => *value as i32,
            MaterialVar::Int(value) => *value,
            MaterialVar::Vec(value, _) => value[0] as i32,
            MaterialVar::Matrix(_) => 0,
            MaterialVar::Str(text) => c_atoi(text),
        }
    }

    /// How shaders test a boolean param: `params[X]->GetIntValue() != 0`.
    ///
    /// There is no boolean arm. `SHADER_PARAM_TYPE_BOOL` exists in the C++
    /// param table but is stored as an int and read with `GetIntValue`, so
    /// `$phong 1`, `$phong 1.0` and `$phong "1"` all mean the same thing.
    pub fn as_bool(&self) -> bool {
        self.as_i32() != 0
    }

    /// `GetVecValue`. A scalar broadcasts to every component, as
    /// `SetFloatValue`/`SetIntValue` do when they fill `m_VecVal`.
    pub fn as_vec4(&self) -> Vec4 {
        match self {
            MaterialVar::Float(value) => [*value; 4],
            MaterialVar::Int(value) => [*value as f32; 4],
            MaterialVar::Vec(value, _) => *value,
            MaterialVar::Matrix(_) => [0.0; 4],
            MaterialVar::Str(text) => [c_atof(text); 4],
        }
    }

    /// `GetMatrixValue`. Anything that is not a matrix reads as the identity,
    /// which is what `SetVertexShaderTextureTransform` substitutes when the var
    /// is the wrong type (`stdshaders/BaseVSShader.cpp:286`).
    pub fn as_matrix(&self) -> Matrix {
        match self {
            MaterialVar::Matrix(matrix) => *matrix,
            _ => IDENTITY,
        }
    }

    /// The string arm, and only the string arm.
    ///
    /// Deliberately narrower than `GetStringValue`, which formats numbers,
    /// vectors and matrices into a shared static buffer. The only callers that
    /// wanted that were debug output and the `.vmt` writer, neither of which is
    /// ported; the callers that matter — texture names, shader names — are
    /// asking whether the var *is* a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            MaterialVar::Str(text) => Some(text),
            _ => None,
        }
    }
}

impl fmt::Display for MaterialVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MaterialVar::Float(value) => write!(f, "{value}"),
            MaterialVar::Int(value) => write!(f, "{value}"),
            MaterialVar::Vec(value, comps) => {
                write!(f, "[")?;
                for component in &value[..*comps as usize] {
                    write!(f, " {component}")?;
                }
                write!(f, " ]")
            }
            MaterialVar::Matrix(_) => write!(f, "<matrix>"),
            MaterialVar::Str(text) => write!(f, "{text}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Value syntax
// ---------------------------------------------------------------------------

/// `IsWhitespace` (`cmaterialvar.cpp:1901`) — space and tab only. A newline
/// ends a value rather than being skipped over.
fn is_blank(c: u8) -> bool {
    c == b' ' || c == b'\t'
}

/// `IsVector` (`cmaterialvar.cpp:1911`): does the value open a vector?
fn is_vector(text: &str) -> bool {
    match text.as_bytes().iter().find(|c| !is_blank(**c)) {
        Some(b'[') | Some(b'{') => true,
        _ => false,
    }
}

/// `ParseVectorFromKeyValueString` (`cmaterial.cpp:932`).
///
/// Returns the components and how many were written. `{}` braces mean the
/// numbers are 0-255 and get divided by 255 — the spelling shipped content uses
/// for colours picked in an image editor.
///
/// `None` only for a component that is not a number at all; a *missing* `]` is
/// a warning in the original and shorter vector here, because content has both.
fn parse_vector(text: &str) -> Option<(Vec4, u8)> {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() && is_blank(bytes[at]) {
        at += 1;
    }

    let divide_by_255 = bytes.get(at) == Some(&b'{');
    at += 1; // the '[' or '{'

    let mut value = [0.0f32; 4];
    let mut comps = 4;
    for component in 0..4 {
        while at < bytes.len() && is_blank(bytes[at]) {
            at += 1;
        }
        // `IsEndline` counts the terminating NUL, which for us is the end of
        // the string.
        match bytes.get(at) {
            None | Some(b'\n') | Some(b']') | Some(b'}') => {
                comps = component;
                break;
            }
            _ => {}
        }

        let len = strtod_end(&text[at..]);
        if len == 0 {
            return None;
        }
        value[component] = c_atof(&text[at..at + len]);
        at += len;
    }

    if comps == 0 {
        // `[]`. The original returns a dimension of 0 and the caller turns that
        // into no var at all, same as a parse failure.
        return None;
    }

    if divide_by_255 {
        for component in &mut value {
            *component *= 1.0 / 255.0;
        }
    }
    Some((value, comps as u8))
}

/// `CreateMatrixMaterialVarFromKeyValue` (`cmaterial.cpp:1014`).
///
/// Three spellings, tried in this order:
///
/// ```text
/// [ m00 m01 m02 m03  m10 ... m33 ]                    16 numbers, row-major
/// scale sx sy translate tx ty rotate a                pre-rotation form
/// center cx cy scale sx sy rotate a translate tx ty   the common one
/// ```
///
/// The third is what `$basetexturetransform` is authored as almost everywhere,
/// usually as the no-op `center .5 .5 scale 1 1 rotate 0 translate 0 0`.
///
/// The two composed forms are transliterated rather than simplified: their
/// multiplication *order* differs between them (`MatrixMultiply(mat, temp, mat)`
/// against `MatrixMultiply(temp, mat, mat)`), and the pre-rotation form's
/// half-texel offset is rotated backwards by the same angle. Neither reads like
/// something anybody would derive twice the same way.
pub fn parse_matrix(text: &str) -> Option<Matrix> {
    if let Some(numbers) = scan_numbers(text, "[", 16) {
        let mut matrix = IDENTITY;
        for (index, value) in numbers.iter().enumerate() {
            matrix[index / 4][index % 4] = *value;
        }
        return Some(matrix);
    }

    if let Some(n) = scan_keyed(text, &["scale", "translate", "rotate"], &[2, 2, 1]) {
        let (scale, translate, angle) = ([n[0], n[1]], [n[2], n[3]], n[4]);

        let mut matrix = translation(translate[0] - 0.5, translate[1] - 0.5);
        matrix = multiply(&matrix, &scaling(scale[0], scale[1]));
        matrix = multiply(&matrix, &rotation_z(angle));

        // Half a texel, in the *scaled* texture's units, rotated back out of
        // the rotation applied above.
        let offset = [
            0.5 / if scale[0] != 0.0 { scale[0] } else { 1.0 },
            0.5 / if scale[1] != 0.0 { scale[1] } else { 1.0 },
        ];
        let offset = rotate_2d(offset, -angle);
        matrix = multiply(&matrix, &translation(offset[0], offset[1]));
        return Some(matrix);
    }

    if let Some(n) = scan_keyed(
        text,
        &["center", "scale", "rotate", "translate"],
        &[2, 2, 1, 2],
    ) {
        let (center, scale, angle, translate) = ([n[0], n[1]], [n[2], n[3]], n[4], [n[5], n[6]]);

        let mut matrix = translation(-center[0], -center[1]);
        matrix = multiply(&scaling(scale[0], scale[1]), &matrix);
        matrix = multiply(&rotation_z(angle), &matrix);
        matrix = multiply(
            &translation(center[0] + translate[0], center[1] + translate[1]),
            &matrix,
        );
        return Some(matrix);
    }

    None
}

/// Reads `prefix` then exactly `count` numbers, and nothing else that matters.
///
/// Stands in for `sscanf( " [ %f %f ... ]" )`: leading blanks are skipped, the
/// literal is matched, and every number must parse. `sscanf` ignores whatever
/// follows the last conversion — including the missing `]` — so this does too.
fn scan_numbers(text: &str, prefix: &str, count: usize) -> Option<Vec<f32>> {
    let mut rest = text.trim_start_matches([' ', '\t']);
    rest = rest.strip_prefix(prefix)?;

    let mut numbers = Vec::with_capacity(count);
    for _ in 0..count {
        rest = rest.trim_start_matches([' ', '\t']);
        let len = strtod_end(rest);
        if len == 0 {
            return None;
        }
        numbers.push(c_atof(&rest[..len]));
        rest = &rest[len..];
    }
    Some(numbers)
}

/// Reads `keyword n n keyword n ...`, the transform spellings' grammar.
///
/// Keywords are matched case-sensitively, as `sscanf`'s literals are.
fn scan_keyed(text: &str, keywords: &[&str], counts: &[usize]) -> Option<Vec<f32>> {
    let mut rest = text;
    let mut numbers = Vec::new();
    for (keyword, count) in keywords.iter().zip(counts) {
        rest = rest.trim_start_matches([' ', '\t']);
        rest = rest.strip_prefix(keyword)?;
        for _ in 0..*count {
            rest = rest.trim_start_matches([' ', '\t']);
            let len = strtod_end(rest);
            if len == 0 {
                return None;
            }
            numbers.push(c_atof(&rest[..len]));
            rest = &rest[len..];
        }
    }
    Some(numbers)
}

// The four `VMatrix` operations the transform spellings need, and no more.
// This is not the start of a math library: `mathlib` becomes `glam` when the
// matrix stack lands in stage 4 (`portdocs/MATERIALSYSTEM.md` §9), and these
// four go with it. Until then a `.vmt` transform is the only matrix the port
// builds, and a dependency for it would be premature.

/// `MatrixBuildTranslation` (`mathlib/vmatrix.cpp:956`) — column 3.
fn translation(x: f32, y: f32) -> Matrix {
    let mut matrix = IDENTITY;
    matrix[0][3] = x;
    matrix[1][3] = y;
    matrix
}

/// `MatrixBuildScale` (`mathlib/vmatrix.cpp:1063`), z left at 1.
fn scaling(x: f32, y: f32) -> Matrix {
    let mut matrix = IDENTITY;
    matrix[0][0] = x;
    matrix[1][1] = y;
    matrix
}

/// `MatrixBuildRotateZ` (`mathlib/vmatrix.cpp:1049`), degrees.
fn rotation_z(degrees: f32) -> Matrix {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let mut matrix = IDENTITY;
    matrix[0][0] = cos;
    matrix[0][1] = -sin;
    matrix[1][0] = sin;
    matrix[1][1] = cos;
    matrix
}

/// `Vector2DRotate` (`public/mathlib/vector2d.h:457`), degrees.
fn rotate_2d(v: [f32; 2], degrees: f32) -> [f32; 2] {
    let (sin, cos) = degrees.to_radians().sin_cos();
    [v[0] * cos - v[1] * sin, v[0] * sin + v[1] * cos]
}

/// `MatrixMultiply( a, b, dst )` (`mathlib/vmatrix.cpp:711`) — `dst = a * b`.
fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [[0.0f32; 4]; 4];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (col, cell) in out_row.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[row][k] * b[k][col]).sum();
        }
    }
    out
}

// ---------------------------------------------------------------------------
// C number parsing
// ---------------------------------------------------------------------------
//
// Rust's `str::parse` is stricter than C's converters in exactly the ways that
// matter here: it rejects trailing text, and it has no notion of "how far did
// you get", which is the *entire* signal Valve's type sniffing runs on. These
// four functions are `strtol`, `strtod`, `atoi` and `atof` in the only respects
// the material system uses them.

/// How far C's `strtol( s, &end, 10 )` advances; 0 if it converts nothing.
///
/// Grammar: blanks, an optional sign, then decimal digits.
fn strtol_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    if matches!(bytes.get(at), Some(b'+') | Some(b'-')) {
        at += 1;
    }
    let digits = at;
    while at < bytes.len() && bytes[at].is_ascii_digit() {
        at += 1;
    }
    if at == digits {
        return 0;
    }
    at
}

/// How far C's `strtod( s, &end )` advances; 0 if it converts nothing.
///
/// Grammar: blanks, an optional sign, digits with an optional point, then an
/// optional `e±digits` exponent — which is taken only if it has digits, since
/// `strtod` backs up to the longest valid prefix and leaves `1e` as just `1`.
///
/// **Hex (`0x1p4`), `inf` and `nan` are deliberately not accepted**, even
/// though C99 `strtod` takes all three. `KeyValues` disables hex by hand on
/// POSIX so that content types the same everywhere (`KeyValues.cpp:2628`), and
/// the same reasoning covers the other two: a `.vmt` saying `infinite` should
/// be the string it looks like.
fn strtod_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    if matches!(bytes.get(at), Some(b'+') | Some(b'-')) {
        at += 1;
    }

    let mut digits = 0;
    while at < bytes.len() && bytes[at].is_ascii_digit() {
        at += 1;
        digits += 1;
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return 0;
    }

    if matches!(bytes.get(at), Some(b'e') | Some(b'E')) {
        let mut exponent = at + 1;
        if matches!(bytes.get(exponent), Some(b'+') | Some(b'-')) {
            exponent += 1;
        }
        let start = exponent;
        while exponent < bytes.len() && bytes[exponent].is_ascii_digit() {
            exponent += 1;
        }
        if exponent > start {
            at = exponent;
        }
    }
    at
}

/// C's `atoi`: the leading integer, or 0.
fn c_atoi(text: &str) -> i32 {
    let end = strtol_end(text);
    // Saturating, like `strtol`'s LONG_MAX/LONG_MIN clamp.
    text[..end].trim_start().parse::<i64>().unwrap_or(0) as i32
}

/// C's `atof`: the leading float, or 0.
fn c_atof(text: &str) -> f32 {
    let end = strtod_end(text);
    text[..end].trim_start().parse::<f32>().unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

/// The `$flags` bit set a `.vmt` can raise.
///
/// `MaterialVarFlags_t` (`public/materialsystem/imaterial_declarations.h:12`)
/// paired with the *names* content writes, which live somewhere else entirely:
/// `s_pShaderStateString` (`materialsystem/shadersystem.cpp:544`), an array
/// whose index is the bit number. The two are kept in sync by a comment in each
/// file asking the reader to remember the other. Here there is one table, and
/// the bit is derived from its position.
///
/// A plain newtype for the same reason as
/// [`TextureFlags`](super::vtf::TextureFlags): the set is fixed by shipped
/// content and never grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct MaterialFlags(pub u32);

/// Flag names, indexed by bit. `s_pShaderStateString`, verbatim — including
/// the two that name nothing (`$xxxxxxunusedxxxxx` at bit 18) and the one that
/// documents itself as not-for-content
/// (`$alphamodifiedbyproxy_DO_NOT_SET_IN_VMT` at bit 30). Keeping the holes is
/// what makes the index *be* the bit.
const FLAG_NAMES: [&str; 32] = [
    "$debug",
    // Bit 1 is `MATERIAL_VAR_NO_DEBUG_OVERRIDE`. The name content writes for
    // it does not match, and this is the table content is matched against.
    "$no_fullbright",
    "$no_draw",
    "$use_in_fillrate_mode",
    "$vertexcolor",
    "$vertexalpha",
    "$selfillum",
    "$additive",
    "$alphatest",
    // Bit 9 has a name here but no enumerator in `MaterialVarFlags_t`, where
    // the slot is `MATERIAL_VAR_NOALPHAWRITES` under `#ifdef _PS3` at bit 30.
    "$pseudotranslucent",
    "$znearer",
    "$model",
    "$flat",
    "$nocull",
    "$nofog",
    "$ignorez",
    "$decal",
    "$envmapsphere",
    "$xxxxxxunusedxxxxx",
    "$envmapcameraspace",
    "$basealphaenvmapmask",
    "$translucent",
    "$normalmapalphaenvmapmask",
    "$softwareskin",
    "$opaquetexture",
    "$multiply",
    "$nodecal",
    "$halflambert",
    "$wireframe",
    "$allowalphatocoverage",
    "$alphamodifiedbyproxy_DO_NOT_SET_IN_VMT",
    "$vertexfog",
];

#[allow(dead_code)]
impl MaterialFlags {
    pub const NONE: Self = Self(0);

    pub const DEBUG: Self = Self(1 << 0);
    pub const NO_DEBUG_OVERRIDE: Self = Self(1 << 1);
    pub const NO_DRAW: Self = Self(1 << 2);
    pub const USE_IN_FILLRATE_MODE: Self = Self(1 << 3);
    pub const VERTEXCOLOR: Self = Self(1 << 4);
    pub const VERTEXALPHA: Self = Self(1 << 5);
    pub const SELFILLUM: Self = Self(1 << 6);
    pub const ADDITIVE: Self = Self(1 << 7);
    pub const ALPHATEST: Self = Self(1 << 8);
    pub const PSEUDOTRANSLUCENT: Self = Self(1 << 9);
    pub const ZNEARER: Self = Self(1 << 10);
    pub const MODEL: Self = Self(1 << 11);
    pub const FLAT: Self = Self(1 << 12);
    pub const NOCULL: Self = Self(1 << 13);
    pub const NOFOG: Self = Self(1 << 14);
    pub const IGNOREZ: Self = Self(1 << 15);
    pub const DECAL: Self = Self(1 << 16);
    pub const ENVMAPSPHERE: Self = Self(1 << 17);
    pub const ENVMAPCAMERASPACE: Self = Self(1 << 19);
    pub const BASEALPHAENVMAPMASK: Self = Self(1 << 20);
    pub const TRANSLUCENT: Self = Self(1 << 21);
    pub const NORMALMAPALPHAENVMAPMASK: Self = Self(1 << 22);
    pub const NEEDS_SOFTWARE_SKINNING: Self = Self(1 << 23);
    pub const OPAQUETEXTURE: Self = Self(1 << 24);
    pub const MULTIPLY: Self = Self(1 << 25);
    pub const SUPPRESS_DECALS: Self = Self(1 << 26);
    pub const HALFLAMBERT: Self = Self(1 << 27);
    pub const WIREFRAME: Self = Self(1 << 28);
    pub const ALLOWALPHATOCOVERAGE: Self = Self(1 << 29);
    pub const ALPHA_MODIFIED_BY_PROXY: Self = Self(1 << 30);
    pub const VERTEXFOG: Self = Self(1 << 31);

    /// Whether every bit of `other` is set here.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    pub fn set(&mut self, other: Self, on: bool) {
        if on {
            self.insert(other);
        } else {
            self.remove(other);
        }
    }

    /// The flag a `.vmt` key names, if it names one.
    ///
    /// `CMaterial::FindMaterialVarFlag` (`cmaterial.cpp:1123`) compares with
    /// `Q_stristr` anchored at the first non-blank character and then insists
    /// the rest is blank — which is case-insensitive equality with the
    /// surrounding whitespace ignored, written the long way because it was
    /// walking a C string. `$modelfoo` does not match `$model`.
    pub fn find(name: &str) -> Option<MaterialFlags> {
        let name = name.trim_matches([' ', '\t']);
        let bit = FLAG_NAMES
            .iter()
            .position(|flag| flag.eq_ignore_ascii_case(name))?;
        Some(MaterialFlags(1 << bit))
    }
}

impl fmt::Display for MaterialFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (bit, name) in FLAG_NAMES.iter().enumerate() {
            if self.0 & (1 << bit) != 0 {
                if !first {
                    write!(f, "|")?;
                }
                write!(f, "{name}")?;
                first = false;
            }
        }
        if first {
            write!(f, "none")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything a shipped `.vmt` can put on the right of a key, and which arm
    /// it lands in. This is the part with no second chance: a value that sniffs
    /// as the wrong type does not fail, it silently means something else.
    #[test]
    fn values_sniff_to_the_type_valve_would_pick() {
        assert_eq!(MaterialVar::parse(""), None);
        assert_eq!(MaterialVar::parse("1"), Some(MaterialVar::Int(1)));
        assert_eq!(MaterialVar::parse("-3"), Some(MaterialVar::Int(-3)));
        assert_eq!(MaterialVar::parse("1.0"), Some(MaterialVar::Float(1.0)));
        assert_eq!(MaterialVar::parse(".5"), Some(MaterialVar::Float(0.5)));
        assert_eq!(MaterialVar::parse("1e3"), Some(MaterialVar::Float(1000.0)));
        assert_eq!(
            MaterialVar::parse("metal/wall01"),
            Some(MaterialVar::Str("metal/wall01".into()))
        );

        // `1e` is a float followed by junk, so neither converter reaches the
        // end and it is a string.
        assert_eq!(
            MaterialVar::parse("1e"),
            Some(MaterialVar::Str("1e".into()))
        );
        // Hex is a string, on purpose.
        assert_eq!(
            MaterialVar::parse("0x10"),
            Some(MaterialVar::Str("0x10".into()))
        );
        // So is a number with a space stuck to it.
        assert_eq!(
            MaterialVar::parse("1 "),
            Some(MaterialVar::Str("1 ".into()))
        );
    }

    #[test]
    fn vectors_come_in_two_spellings() {
        assert_eq!(
            MaterialVar::parse("[1 .5 0]"),
            Some(MaterialVar::Vec([1.0, 0.5, 0.0, 0.0], 3))
        );
        // Braces mean 0-255. Compared with a tolerance because the division is
        // Valve's `* (1.0f / 255.0f)`, which is not exact for most inputs and
        // is reproduced rather than improved.
        let Some(MaterialVar::Vec(value, 3)) = MaterialVar::parse("{255 0 51}") else {
            panic!("a braced vector is a three-component vector");
        };
        assert!((value[0] - 1.0).abs() < 1e-6, "{value:?}");
        assert!((value[1] - 0.0).abs() < 1e-6, "{value:?}");
        assert!((value[2] - 0.2).abs() < 1e-6, "{value:?}");
        // Fewer than four components is legal and the count is kept.
        assert_eq!(
            MaterialVar::parse("[2 4]"),
            Some(MaterialVar::Vec([2.0, 4.0, 0.0, 0.0], 2))
        );
        assert_eq!(
            MaterialVar::parse("[1 2 3 4]"),
            Some(MaterialVar::Vec([1.0, 2.0, 3.0, 4.0], 4))
        );
        // A missing bracket is content Valve warns about and still reads.
        assert_eq!(
            MaterialVar::parse("[1 1 1"),
            Some(MaterialVar::Vec([1.0, 1.0, 1.0, 0.0], 3))
        );
        // A vector of nothing is no var at all, not an empty vector.
        assert_eq!(MaterialVar::parse("[x]"), None);
    }

    #[test]
    fn the_common_texture_transform_is_the_identity() {
        // What `$basetexturetransform` says in almost every shipped material.
        let var = MaterialVar::parse("center .5 .5 scale 1 1 rotate 0 translate 0 0").unwrap();
        let matrix = var.as_matrix();
        for row in 0..4 {
            for col in 0..4 {
                assert!(
                    (matrix[row][col] - IDENTITY[row][col]).abs() < 1e-6,
                    "m[{row}][{col}] = {}",
                    matrix[row][col]
                );
            }
        }
    }

    #[test]
    fn texture_transforms_scale_about_the_centre() {
        // Scale 2x about (.5,.5): the middle of the texture stays put and the
        // corners move out. Applied as `m * (u, v, 0, 1)`.
        let var = MaterialVar::parse("center .5 .5 scale 2 2 rotate 0 translate 0 0").unwrap();
        let m = var.as_matrix();
        let apply = |u: f32, v: f32| {
            [
                m[0][0] * u + m[0][1] * v + m[0][3],
                m[1][0] * u + m[1][1] * v + m[1][3],
            ]
        };

        let centre = apply(0.5, 0.5);
        assert!((centre[0] - 0.5).abs() < 1e-6 && (centre[1] - 0.5).abs() < 1e-6);
        let corner = apply(1.0, 1.0);
        assert!((corner[0] - 1.5).abs() < 1e-6 && (corner[1] - 1.5).abs() < 1e-6);
    }

    #[test]
    fn a_translating_transform_moves_the_coordinates() {
        let var = MaterialVar::parse("center 0 0 scale 1 1 rotate 0 translate .25 .5").unwrap();
        let m = var.as_matrix();
        assert!((m[0][3] - 0.25).abs() < 1e-6, "u offset");
        assert!((m[1][3] - 0.5).abs() < 1e-6, "v offset");
    }

    #[test]
    fn an_explicit_sixteen_number_matrix_is_read_row_major() {
        let var = MaterialVar::parse("[ 1 2 3 4  5 6 7 8  9 10 11 12  13 14 15 16 ]").unwrap();
        let m = var.as_matrix();
        assert_eq!(m[0], [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(m[3], [13.0, 14.0, 15.0, 16.0]);
    }

    #[test]
    fn a_scalar_read_as_a_vector_broadcasts() {
        // `"$color" "1"` is content, and it means white.
        assert_eq!(MaterialVar::Int(1).as_vec4(), [1.0; 4]);
        assert_eq!(MaterialVar::Float(0.5).as_vec4(), [0.5; 4]);
    }

    #[test]
    fn a_string_read_as_a_number_runs_atof() {
        let var = MaterialVar::Str(" 2.5 rest".into());
        assert_eq!(var.as_f32(), 2.5);
        assert_eq!(var.as_i32(), 2);
        assert_eq!(var.as_vec4(), [2.5; 4]);

        // Not a number at all reads as zero, never as an error.
        let var = MaterialVar::Str("nope".into());
        assert_eq!(var.as_f32(), 0.0);
        assert_eq!(var.as_i32(), 0);
    }

    #[test]
    fn floats_truncate_toward_zero_when_read_as_ints() {
        assert_eq!(MaterialVar::Float(1.9).as_i32(), 1);
        assert_eq!(MaterialVar::Float(-1.9).as_i32(), -1);
    }

    #[test]
    fn a_matrix_reads_as_zero_everywhere_else() {
        let var = MaterialVar::parse("center 0 0 scale 2 2 rotate 0 translate 0 0").unwrap();
        assert_eq!(var.as_f32(), 0.0);
        assert_eq!(var.as_i32(), 0);
        assert_eq!(var.as_vec4(), [0.0; 4]);
        assert_eq!(var.as_str(), None);
    }

    #[test]
    fn flag_names_map_to_their_bit() {
        assert_eq!(
            MaterialFlags::find("$translucent"),
            Some(MaterialFlags::TRANSLUCENT)
        );
        assert_eq!(
            MaterialFlags::find("$TRANSLUCENT"),
            Some(MaterialFlags::TRANSLUCENT)
        );
        assert_eq!(
            MaterialFlags::find("  $nocull "),
            Some(MaterialFlags::NOCULL)
        );
        assert_eq!(
            MaterialFlags::find("$vertexfog"),
            Some(MaterialFlags::VERTEXFOG)
        );
        assert_eq!(MaterialFlags::find("$model"), Some(MaterialFlags::MODEL));

        // Not flags.
        assert_eq!(MaterialFlags::find("$basetexture"), None);
        assert_eq!(MaterialFlags::find("$models"), None);
        assert_eq!(MaterialFlags::find(""), None);
    }

    #[test]
    fn the_name_table_and_the_bit_constants_agree() {
        // The two halves live in different files in the original and are kept
        // in sync by a comment. Here the pairing is checkable, so check it.
        for (name, flag) in [
            ("$debug", MaterialFlags::DEBUG),
            ("$no_fullbright", MaterialFlags::NO_DEBUG_OVERRIDE),
            ("$alphatest", MaterialFlags::ALPHATEST),
            ("$znearer", MaterialFlags::ZNEARER),
            ("$decal", MaterialFlags::DECAL),
            ("$nodecal", MaterialFlags::SUPPRESS_DECALS),
            ("$softwareskin", MaterialFlags::NEEDS_SOFTWARE_SKINNING),
            ("$allowalphatocoverage", MaterialFlags::ALLOWALPHATOCOVERAGE),
            ("$vertexfog", MaterialFlags::VERTEXFOG),
        ] {
            assert_eq!(MaterialFlags::find(name), Some(flag), "{name}");
        }
    }
}
