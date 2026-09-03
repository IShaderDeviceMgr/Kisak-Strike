//! Reading a `.vmt`: patch expansion, conditional keys, flags and vars.
//!
//! Replaces the parsing half of `materialsystem/cmaterial.cpp` —
//! `LoadVMTFile` + `ExpandPatchFile` + `AccumulateRecursiveVmtPatches` +
//! `ApplyPatchKeyValues` + `ParseMaterialVars` + `ShouldSkipVar` +
//! `FindBuiltinFallbackBlock`, about 700 lines of it. What comes out is a
//! shader name, a flag word and a set of named values; what does *not* come out
//! is anything about the GPU, so this module is testable without one.
//!
//! The `.vmt` grammar is fixed: Valve authors the content and we do not own the
//! producer (`PORTING.md`, "Format is fixed"). So the rules here are
//! transliterated closely even where they look strange, and the strange ones
//! are labelled.
//!
//! # The four ways a key can fail to become a var
//!
//! Reading a `.vmt` is mostly *rejection*, and each rule below silently changes
//! what a material means if it is missed:
//!
//! 1. **`cond?$var`** — a conditional key. The condition is evaluated against
//!    fixed answers (see [`should_skip`]) and the key is dropped or kept whole.
//! 2. **`%tooltexture`** — an editor-only key, dropped outside Hammer.
//! 3. **`$translucent`** and its 31 siblings — *flags*, not vars. They set bits
//!    in [`MaterialFlags`] and never appear in the var map.
//! 4. **An empty or malformed value** — no var, rather than an empty one.
//!
//! # Patch materials
//!
//! A `.vmt` whose outermost key is `patch` is not a material: it names another
//! `.vmt` with `include` and edits it with `insert` and `replace` blocks. Portal
//! 2 content uses this heavily — it is how one base material is re-skinned
//! dozens of times. [`expand_patch`] resolves the chain.

use crate::filesystem::keyvalues::{self, Block, Value};
use crate::filesystem::Vfs;

use super::error::VmtError;
use super::var::{MaterialFlags, MaterialVar};

/// Where `.vmt` files live, relative to the search paths.
/// `CMaterialSystem::FindMaterial` builds `"materials/" + name`
/// (`cmaterialsystem.cpp:3078`).
pub const MATERIAL_DIR: &str = "materials/";
pub const MATERIAL_EXT: &str = ".vmt";

/// How many `patch` files may chain before the loop is called a cycle.
/// `AccumulateRecursiveVmtPatches` (`cmaterial.cpp:3510`) counts to ten.
const MAX_PATCH_DEPTH: u32 = 10;

/// A `.vmt`, read and resolved: shader, flags and values.
///
/// This is the whole of what the material system needs from the file. The
/// `KeyValues` document it came from is not kept — Valve held onto it for
/// `$fallbackmaterial` re-parsing and for the material editor, neither of which
/// is ported.
#[derive(Debug, Clone)]
pub struct Vmt {
    /// The outermost key: the shader this material asks for, in the file's own
    /// spelling. Resolving it to something drawable is [`ShaderKind`]'s job.
    ///
    /// [`ShaderKind`]: super::shader::ShaderKind
    pub shader: String,

    /// Every surviving `$key value` pair, keyed by the lowercased name with its
    /// `$` kept — `"$basetexture"`, not `"basetexture"`. The `$` is part of how
    /// content spells these and dropping it would only invite the question of
    /// what to do with the handful that start with `%`.
    pub vars: Vec<(String, MaterialVar)>,

    /// `$flags`: the bits raised by flag-named keys.
    pub flags: MaterialFlags,

    /// `$flags_defined`: which bits the file *mentioned*, however it set them.
    ///
    /// The distinction matters because "not set" and "set to 0" differ for a
    /// handful of shader decisions, and because a fallback block that turns a
    /// flag off has to be distinguishable from one that never spoke about it.
    pub flags_defined: MaterialFlags,
}

impl Vmt {
    /// Reads `materials/<name>.vmt`, following any patch chain.
    ///
    /// `name` is expected already normalized — lowercased, forward slashes, no
    /// extension. [`MaterialCache`](super::material::MaterialCache) does that.
    pub fn load(vfs: &Vfs, name: &str) -> Result<Vmt, VmtError> {
        let path = format!("{MATERIAL_DIR}{name}{MATERIAL_EXT}");
        let mut document = read_document(vfs, &path)?;
        expand_patch(vfs, &path, &mut document)?;
        Vmt::from_keyvalues(&path, &document)
    }

    /// Builds a material description from an already-parsed document.
    ///
    /// This is the entry point for materials that were never files:
    /// `CMaterialSystem::CreateMaterial( name, pVMTKeyValues )`
    /// (`cmaterialsystem.cpp:2981`) builds the error material, the flat
    /// material and the buffer-clear materials this way. No patch expansion
    /// happens here — a document handed over in memory is already final.
    pub fn from_keyvalues(name: &str, document: &Block) -> Result<Vmt, VmtError> {
        let shader = document
            .first_block_key()
            .ok_or_else(|| VmtError::NoShader {
                name: name.to_owned(),
            })?
            .to_owned();
        let root = document.first_block().expect("a key implies its block");

        let mut vmt = Vmt {
            shader,
            vars: Vec::new(),
            flags: MaterialFlags::NONE,
            flags_defined: MaterialFlags::NONE,
        };

        // `ParseMaterialVars` walks the override section first and then the
        // base, with one shared "already seen" set, so an override wins and
        // does so silently. See `read_block`.
        let mut seen = SeenKeys::default();
        if let Some(section) = builtin_fallback_block(&vmt.shader, root) {
            vmt.read_block(name, section, &mut seen, true);
        }
        vmt.read_block(name, root, &mut seen, false);

        Ok(vmt)
    }

    /// The value of a var, by name. Case-insensitive, `$` included.
    pub fn var(&self, name: &str) -> Option<&MaterialVar> {
        self.vars
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }

    /// One pass of `ParseMaterialVars` (`cmaterial.cpp:1335`) over one block.
    fn read_block(&mut self, name: &str, block: &Block, seen: &mut SeenKeys, overriding: bool) {
        for entry in block.entries() {
            // Only leaf keys are vars. A nested block is a fallback section, a
            // proxy list, or something else this loop is not about.
            let Value::String(text) = &entry.value else {
                continue;
            };

            let conditional = is_conditional(&entry.key);
            if conditional && should_skip(&entry.key, name) {
                continue;
            }
            let key = var_name(&entry.key);

            // `%`-prefixed keys are Hammer's (`%tooltexture`, `%keywords`).
            // The original keeps them only when the material system is running
            // without graphics or inside the editor, neither of which happens
            // here (`cmaterial.cpp:1372`).
            if key.starts_with('%') {
                continue;
            }

            if let Some(flag) = MaterialFlags::find(key) {
                self.set_flag(name, &entry.key, flag, text, seen, overriding);
                continue;
            }

            // Multiply defined. A conditional key is exempt on purpose: writing
            // `$foo 1` and then `hdr?$foo 2` is how content says "and this
            // instead, in HDR", so the later one has to replace the earlier.
            let lowercase = key.to_ascii_lowercase();
            if let Some(index) = seen.vars.iter().position(|k| *k == lowercase) {
                if !conditional {
                    // Silent when an override shadows the base: that is what an
                    // override *is*. Loud otherwise, because the file has two
                    // definitions of one thing and one of them does nothing.
                    if !seen.overridden[index] {
                        eprintln!(
                            "source-engine: materials: {name}: \"{}\" is defined more than once",
                            entry.key
                        );
                    }
                    continue;
                }
            }

            let Some(var) = MaterialVar::parse(text) else {
                continue;
            };
            match seen.vars.iter().position(|k| *k == lowercase) {
                Some(index) => {
                    self.vars[index].1 = var;
                    seen.overridden[index] |= overriding;
                }
                None => {
                    self.vars.push((lowercase.clone(), var));
                    seen.vars.push(lowercase);
                    seen.overridden.push(overriding);
                }
            }
        }
    }

    /// `CMaterial::ParseMaterialFlag` (`cmaterial.cpp:1181`).
    fn set_flag(
        &mut self,
        name: &str,
        key: &str,
        flag: MaterialFlags,
        text: &str,
        seen: &mut SeenKeys,
        overriding: bool,
    ) {
        let already = if overriding {
            seen.override_flags
        } else {
            seen.flags
        };
        if already.contains(flag) {
            eprintln!("source-engine: materials: {name}: flag \"{key}\" is defined more than once");
            return;
        }
        // Overrides win, and the base's copy is dropped without a word.
        if seen.override_flags.contains(flag) {
            return;
        }
        if overriding {
            seen.override_flags.insert(flag);
        } else {
            seen.flags.insert(flag);
        }

        let on = MaterialVar::parse(text)
            .map(|var| var.as_bool())
            .unwrap_or(false);
        self.flags.set(flag, on);
        self.flags_defined.insert(flag);
    }
}

/// What `ParseMaterialVars` tracked in `pOverride[]`, `flagMask` and
/// `overrideMask` — which keys have been claimed, and by which pass.
#[derive(Default)]
struct SeenKeys {
    /// Lowercased var names, parallel to [`Vmt::vars`].
    vars: Vec<String>,
    /// Whether the var at the same index came from the override section.
    overridden: Vec<bool>,
    flags: MaterialFlags,
    override_flags: MaterialFlags,
}

// ---------------------------------------------------------------------------
// Conditional keys
// ---------------------------------------------------------------------------

/// Whether a key carries a `cond?` prefix.
///
/// A `?` in the first position does not count — `CMaterial::ShouldSkipVar`
/// requires `pQuestion != pVarName` (`cmaterial.cpp:1234`).
fn is_conditional(key: &str) -> bool {
    matches!(key.find('?'), Some(at) if at > 0)
}

/// The key with any `cond?` prefix removed. `GetVarName` (`cmaterial.cpp:880`).
fn var_name(key: &str) -> &str {
    match key.find('?') {
        Some(at) => &key[at + 1..],
        None => key,
    }
}

/// Whether a `cond?$var` key is dropped. `CMaterial::ShouldSkipVar`.
///
/// Every condition in the original asks the hardware config, a convar or the
/// platform. This port has one fixed capability tier
/// (`portdocs/MATERIALSYSTEM.md` §4.6), so every answer is a constant — and
/// writing them down as constants is the point, because each one is a decision
/// somebody can reverse:
///
/// | Condition | Here | Because |
/// |---|---|---|
/// | `hdr` | **skipped** | `GetHDRType() == HDR_TYPE_NONE`: the swap chain is SDR, `portdocs/MATERIALSYSTEM.md` §10 |
/// | `ldr` | kept | the other half of the same answer |
/// | `srgb`, `srgb_pc` | kept | `UsesSRGBCorrectBlending()` was DX10-class-hardware-only; `wgpu` blends in linear space on an sRGB target unconditionally |
/// | `srgb_gameconsole` | skipped | no console |
/// | `360`, `SonyPS3`, `gameconsole` | skipped | `PORTING.md`, "Supported platforms" |
/// | `GPU>=1`, `GPU>=2`, `GPU>=3` | kept | `gpu_level` defaults to 3 (`cmaterialsystem.cpp:52`) and there is no video-options menu to lower it |
/// | `GPU<1`, `GPU<2`, `GPU<3` | skipped | same answer |
/// | `lowfill` | skipped | `mat_reduceparticles` defaults to 0 |
/// | `LowQualityCSM`, `HighQualityCSM` | **both skipped** | cascaded shadow maps are not ported; a material asking for either quality gets neither |
/// | anything else | skipped, with a warning | the original's fall-through, which leaves `bShouldSkip` at its initial `true` |
///
/// A leading `!` inverts whatever the table says, which is why the answers are
/// stated as "does this key survive" rather than as booleans about hardware.
fn should_skip(key: &str, material: &str) -> bool {
    let at = key.find('?').expect("callers check is_conditional first");
    let mut condition = &key[..at];

    let negated = condition.starts_with('!');
    if negated {
        condition = &condition[1..];
    }

    let skip = match condition.to_ascii_lowercase().as_str() {
        "ldr" | "srgb" | "srgb_pc" | "gpu>=1" | "gpu>=2" | "gpu>=3" => false,
        "hdr" | "srgb_gameconsole" | "gpu<1" | "gpu<2" | "gpu<3" | "360" | "sonyps3"
        | "gameconsole" | "lowfill" | "lowqualitycsm" | "highqualitycsm" => true,
        _ => {
            eprintln!(
                "source-engine: materials: {material}: unrecognized conditional test \"{key}\""
            );
            true
        }
    };
    skip ^ negated
}

// ---------------------------------------------------------------------------
// Built-in fallback blocks
// ---------------------------------------------------------------------------

/// The `<suffix>` blocks a `.vmt` may carry to override itself, in the order
/// the original tries them, reduced to the ones our capability tier reaches.
///
/// `FindBuiltinFallbackBlock` (`cmaterial.cpp:1470`) tests thirteen conditions
/// against `gpu_level` and `GetDXSupportLevel()`. With `gpu_level` 3 and DX
/// level 95 — the single tier of `portdocs/MATERIALSYSTEM.md` §4.6 — the
/// `GPU<n`, `<DX90`, `<DX95`, `<DX90_20b` and `<=DX90` tests are all false and
/// the HDR branch takes its `ldr` side. What is left is this list, in the
/// original's order, and *first match wins*.
const FALLBACK_SUFFIXES: [&str; 8] = [
    "GPU>=1",
    "GPU>=2",
    ">=DX90_20b",
    ">=DX90",
    ">DX90",
    "ldr",
    "srgb",
    "dx9",
];

/// Finds the block whose keys override the material's own, if there is one.
///
/// Each suffix is looked for twice — as a bare block name and as
/// `<shader>_<suffix>` — which is `CheckConditionalFakeShaderName`
/// (`cmaterial.cpp:1452`) and is why a `.vmt` can contain either
/// `">=DX90" { ... }` or `"LightmappedGeneric_dx9" { ... }`.
fn builtin_fallback_block<'a>(shader: &str, root: &'a Block) -> Option<&'a Block> {
    for suffix in FALLBACK_SUFFIXES {
        if let Some(block) = root.find_block(suffix) {
            return Some(block);
        }
        if let Some(block) = root.find_block(&format!("{shader}_{suffix}")) {
            return Some(block);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Patch materials
// ---------------------------------------------------------------------------

/// Resolves a `patch` document into the material it patches.
///
/// `ExpandPatchFile` + `AccumulateRecursiveVmtPatches` + `ApplyPatchKeyValues`
/// (`cmaterial.cpp:3489-3592`), which is three functions because the C++ had to
/// pass `KeyValues*` out through parameters.
///
/// Does nothing to a document that is not a patch. Otherwise it walks the
/// `include` chain, accumulating each level's `insert` and `replace` sections,
/// and applies them to the non-patch document at the end.
///
/// # Two orderings that are easy to get backwards
///
/// - **Deeper patches win.** Accumulation is `MergeKeyValues( src, dest )`
///   (`cmaterial.cpp:3437`), an overwrite, and it runs on the outer patch
///   first — so when a patch includes a patch, the *inner* one's keys survive.
/// - **`insert` runs before `replace`.** So a key that `insert` adds is then
///   visible to `replace`, which only touches keys that already exist.
pub fn expand_patch(vfs: &Vfs, path: &str, document: &mut Block) -> Result<(), VmtError> {
    if !is_patch(document) {
        return Ok(());
    }

    let mut inserts = Block::default();
    let mut replaces = Block::default();
    let mut current = document.clone();
    let mut current_path = path.to_owned();

    let mut depth = 0;
    while depth < MAX_PATCH_DEPTH && is_patch(&current) {
        let patch = current.first_block().expect("is_patch implies a block");
        if let Some(section) = patch.find_block("insert") {
            merge_values(section, &mut inserts);
        }
        if let Some(section) = patch.find_block("replace") {
            merge_values(section, &mut replaces);
        }

        let include = patch
            .find_string("include")
            .filter(|include| !include.is_empty())
            .ok_or_else(|| VmtError::PatchWithoutInclude {
                name: current_path.clone(),
            })?
            .to_owned();

        // The include is a whole path from the game root, `materials/` prefix
        // and `.vmt` extension included, and it is loaded verbatim.
        current = read_document(vfs, &include)?;
        current_path = include;
        depth += 1;
    }

    if depth >= MAX_PATCH_DEPTH {
        eprintln!("source-engine: materials: {path}: patch chain is {MAX_PATCH_DEPTH} deep; giving up (a cycle?)");
    }

    let root = current
        .first_block_mut()
        .ok_or_else(|| VmtError::NoShader {
            name: current_path.clone(),
        })?;
    insert_keys(root, &inserts, false);
    insert_keys(root, &replaces, true);

    *document = current;
    Ok(())
}

/// Whether the outermost key is `patch`.
fn is_patch(document: &Block) -> bool {
    document
        .first_block_key()
        .is_some_and(|key| key.eq_ignore_ascii_case("patch"))
}

/// `MergeKeyValues` (`cmaterial.cpp:3437`): copy leaf values across,
/// overwriting. Blocks are not merged — the original switches on the data type
/// and has no case for them.
fn merge_values(src: &Block, dst: &mut Block) {
    for (key, value) in src.values() {
        dst.set(key, Value::String(value.to_owned()));
    }
}

/// `InsertKeyValues` (`cmaterial.cpp:3369`).
///
/// With `only_existing` false this is the `insert` section: every leaf value is
/// written whether or not the key was there. With it true this is `replace`:
/// only keys already present are written, and nested blocks present in both are
/// recursed into. Blocks in an `insert` section are silently ignored, which is
/// the original's behaviour and not obviously deliberate — the switch it walks
/// has no branch for them.
fn insert_keys(dst: &mut Block, src: &Block, only_existing: bool) {
    for (key, value) in src.values() {
        if !only_existing || dst.find(key).is_some() {
            dst.set(key, Value::String(value.to_owned()));
        }
    }

    if !only_existing {
        return;
    }
    // Recurse into blocks that exist on both sides. Driven from `dst`, as the
    // original is, so a `replace` section naming a block the material does not
    // have does nothing.
    let names: Vec<String> = dst
        .entries()
        .iter()
        .filter(|entry| matches!(entry.value, Value::Block(_)))
        .map(|entry| entry.key.clone())
        .collect();
    for name in names {
        let Some(Value::Block(section)) = src.find(&name).cloned() else {
            continue;
        };
        if let Some(target) = dst.find_block_mut(&name) {
            insert_keys(target, &section, true);
        }
    }
}

/// Reads and parses one `.vmt`-shaped document out of the game's content.
fn read_document(vfs: &Vfs, path: &str) -> Result<Block, VmtError> {
    let bytes = vfs.read(path)?;
    // Same lossy conversion the KeyValues reader uses on its own tokens: a
    // stray non-UTF-8 byte in a comment must not fail the whole material.
    let text = String::from_utf8_lossy(&bytes);
    Ok(keyvalues::parse(path, &text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vmt(text: &str) -> Vmt {
        let document = keyvalues::parse("test.vmt", text).expect("valid keyvalues");
        Vmt::from_keyvalues("test.vmt", &document).expect("a shader block")
    }

    #[test]
    fn reads_the_shader_name_and_the_vars() {
        let vmt = vmt(r#"
"UnlitGeneric"
{
	"$basetexture" "metal/metalwall048a"
	"$alpha" "0.5"
	"$frame" "2"
}
"#);
        assert_eq!(vmt.shader, "UnlitGeneric");
        assert_eq!(
            vmt.var("$basetexture").and_then(MaterialVar::as_str),
            Some("metal/metalwall048a")
        );
        assert_eq!(vmt.var("$ALPHA").map(MaterialVar::as_f32), Some(0.5));
        assert_eq!(vmt.var("$frame"), Some(&MaterialVar::Int(2)));
        assert_eq!(vmt.var("$absent"), None);
    }

    #[test]
    fn flags_are_not_vars() {
        let vmt = vmt(r#"
"UnlitGeneric"
{
	"$translucent" "1"
	"$nocull" "0"
	"$basetexture" "x"
}
"#);
        assert!(vmt.flags.contains(MaterialFlags::TRANSLUCENT));
        assert!(!vmt.flags.contains(MaterialFlags::NOCULL));
        // Mentioned, and turned off: those are different states.
        assert!(vmt.flags_defined.contains(MaterialFlags::NOCULL));
        assert!(!vmt.flags_defined.contains(MaterialFlags::ADDITIVE));

        assert_eq!(vmt.var("$translucent"), None, "a flag is never a var");
        assert_eq!(vmt.vars.len(), 1);
    }

    #[test]
    fn editor_keys_are_dropped() {
        let vmt = vmt(r#"
"LightmappedGeneric"
{
	"%tooltexture" "tools/toolsnodraw"
	"%keywords" "portal2"
	"$basetexture" "x"
}
"#);
        assert_eq!(vmt.vars.len(), 1);
        assert!(vmt.var("%tooltexture").is_none());
    }

    #[test]
    fn conditional_keys_follow_the_fixed_capability_tier() {
        let vmt = vmt(r#"
"UnlitGeneric"
{
	"ldr?$basetexture"   "the ldr one"
	"hdr?$basetexture"   "the hdr one"
	"360?$detail"        "console only"
	"gpu>=2?$envmaptint" "[1 1 1]"
	"!hdr?$color"        "[1 0 0]"
}
"#);
        // SDR: the ldr key survives and the hdr one does not.
        assert_eq!(
            vmt.var("$basetexture").and_then(MaterialVar::as_str),
            Some("the ldr one")
        );
        assert!(vmt.var("$detail").is_none(), "console-only key");
        assert!(vmt.var("$envmaptint").is_some(), "gpu_level is 3");
        assert!(vmt.var("$color").is_some(), "!hdr is the ldr side");
    }

    #[test]
    fn a_conditional_key_replaces_the_plain_one() {
        // The exemption from the multiply-defined rule: this is how content
        // says "and this instead, under these conditions".
        let vmt = vmt(r#"
"UnlitGeneric"
{
	"$basetexture"     "plain"
	"ldr?$basetexture" "conditional"
}
"#);
        assert_eq!(
            vmt.var("$basetexture").and_then(MaterialVar::as_str),
            Some("conditional")
        );
        assert_eq!(vmt.vars.len(), 1, "still one var, not two");
    }

    #[test]
    fn the_first_definition_of_a_plain_key_wins() {
        let vmt = vmt(r#"
"UnlitGeneric"
{
	"$basetexture" "first"
	"$basetexture" "second"
}
"#);
        assert_eq!(
            vmt.var("$basetexture").and_then(MaterialVar::as_str),
            Some("first")
        );
    }

    #[test]
    fn a_fallback_block_overrides_the_material() {
        let vmt = vmt(r#"
"UnlitGeneric"
{
	"$basetexture" "base"
	"$alpha"       "1"

	">=DX90"
	{
		"$basetexture" "the dx9 one"
		"$translucent" "1"
	}
}
"#);
        assert_eq!(
            vmt.var("$basetexture").and_then(MaterialVar::as_str),
            Some("the dx9 one")
        );
        assert_eq!(vmt.var("$alpha").map(MaterialVar::as_f32), Some(1.0));
        assert!(vmt.flags.contains(MaterialFlags::TRANSLUCENT));
    }

    #[test]
    fn a_fallback_block_can_be_named_after_the_shader() {
        let vmt = vmt(r#"
"UnlitGeneric"
{
	"$basetexture" "base"
	"UnlitGeneric_dx9" { "$basetexture" "suffixed" }
}
"#);
        assert_eq!(
            vmt.var("$basetexture").and_then(MaterialVar::as_str),
            Some("suffixed")
        );
    }

    #[test]
    fn only_the_first_matching_fallback_block_applies() {
        // `GPU>=1` is tested before `dx9`, so it wins outright — the sections
        // do not compose.
        let vmt = vmt(r#"
"UnlitGeneric"
{
	"$basetexture" "base"
	"GPU>=1" { "$basetexture" "gpu" }
	"dx9"    { "$basetexture" "dx9"  "$alpha" "0.25" }
}
"#);
        assert_eq!(
            vmt.var("$basetexture").and_then(MaterialVar::as_str),
            Some("gpu")
        );
        assert!(vmt.var("$alpha").is_none(), "the dx9 block is not read");
    }

    #[test]
    fn a_malformed_value_produces_no_var() {
        let vmt = vmt(r#"
"UnlitGeneric"
{
	"$basetexture" ""
	"$color"       "[]"
	"$alpha"       "1"
}
"#);
        assert_eq!(vmt.vars.len(), 1);
        assert!(vmt.var("$basetexture").is_none());
        assert!(vmt.var("$color").is_none());
    }

    #[test]
    fn a_document_without_a_block_is_an_error() {
        let document = keyvalues::parse("test.vmt", "loose value").unwrap();
        assert!(matches!(
            Vmt::from_keyvalues("test.vmt", &document),
            Err(VmtError::NoShader { .. })
        ));
    }

    // -----------------------------------------------------------------------
    // Patch expansion. These need a filesystem, since `include` names a file.
    // -----------------------------------------------------------------------

    use crate::filesystem::{SearchPathOptions, Vfs};
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// A throwaway game install. Same shape as the one in
    /// `src/filesystem/mod.rs`'s tests, kept separate so that neither module's
    /// tests can move the other's ground.
    struct TempGame(PathBuf);

    impl TempGame {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("kisak-vmt-test-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("portal2")).unwrap();
            let game = TempGame(path);
            game.write(
                "gameinfo.txt",
                r#""GameInfo" { game "Portal 2" FileSystem { SearchPaths { Game |gameinfo_path|. } } }"#,
            );
            game
        }

        /// Writes a file into the mod directory.
        fn write(&self, rel: &str, text: &str) {
            let full = self.0.join("portal2").join(rel);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::File::create(&full)
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }

        fn mount(&self) -> Vfs {
            Vfs::mount_game(
                &self.0.join("portal2"),
                Path::new(&self.0),
                &SearchPathOptions::default(),
            )
            .unwrap()
        }
    }

    impl Drop for TempGame {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_patch_replaces_and_inserts_into_its_include() {
        let game = TempGame::new("patch");
        game.write(
            "materials/base.vmt",
            r#"
"UnlitGeneric"
{
	"$basetexture" "original"
	"$alpha"       "1"
}
"#,
        );
        game.write(
            "materials/skin.vmt",
            r#"
"patch"
{
	include "materials/base.vmt"
	replace { "$basetexture" "replaced" }
	insert  { "$color" "[1 0 0]" }
}
"#,
        );

        let vmt = Vmt::load(&game.mount(), "skin").unwrap();
        assert_eq!(vmt.shader, "UnlitGeneric", "the base's shader, not `patch`");
        assert_eq!(
            vmt.var("$basetexture").and_then(MaterialVar::as_str),
            Some("replaced")
        );
        assert_eq!(vmt.var("$alpha").map(MaterialVar::as_f32), Some(1.0));
        assert!(vmt.var("$color").is_some(), "insert adds a new key");
    }

    #[test]
    fn replace_only_touches_keys_that_are_already_there() {
        let game = TempGame::new("replaceonly");
        game.write(
            "materials/base.vmt",
            r#""UnlitGeneric" { "$basetexture" "original" }"#,
        );
        game.write(
            "materials/skin.vmt",
            r#"
"patch"
{
	include "materials/base.vmt"
	replace { "$basetexture" "replaced"  "$alpha" "0.5" }
}
"#,
        );

        let vmt = Vmt::load(&game.mount(), "skin").unwrap();
        assert_eq!(
            vmt.var("$basetexture").and_then(MaterialVar::as_str),
            Some("replaced")
        );
        assert!(
            vmt.var("$alpha").is_none(),
            "replace does not create keys — that is what insert is for"
        );
    }

    #[test]
    fn patches_chain_and_the_innermost_one_wins() {
        let game = TempGame::new("chain");
        game.write(
            "materials/base.vmt",
            r#""UnlitGeneric" { "$basetexture" "original"  "$alpha" "1" }"#,
        );
        game.write(
            "materials/middle.vmt",
            r#"
"patch"
{
	include "materials/base.vmt"
	replace { "$basetexture" "middle"  "$alpha" "0.5" }
}
"#,
        );
        game.write(
            "materials/outer.vmt",
            r#"
"patch"
{
	include "materials/middle.vmt"
	replace { "$basetexture" "outer" }
}
"#,
        );

        let vmt = Vmt::load(&game.mount(), "outer").unwrap();
        // Accumulation overwrites as it descends, so `middle` — read second —
        // is the one that survives. This looks backwards and is Valve's.
        assert_eq!(
            vmt.var("$basetexture").and_then(MaterialVar::as_str),
            Some("middle")
        );
        assert_eq!(vmt.var("$alpha").map(MaterialVar::as_f32), Some(0.5));
    }

    #[test]
    fn a_patch_with_a_missing_include_is_an_error() {
        let game = TempGame::new("noinclude");
        game.write(
            "materials/bad.vmt",
            r#""patch" { replace { "$alpha" "1" } }"#,
        );
        assert!(matches!(
            Vmt::load(&game.mount(), "bad"),
            Err(VmtError::PatchWithoutInclude { .. })
        ));

        game.write(
            "materials/gone.vmt",
            r#""patch" { include "materials/nothere.vmt" }"#,
        );
        assert!(matches!(
            Vmt::load(&game.mount(), "gone"),
            Err(VmtError::Read(_))
        ));
    }

    #[test]
    fn a_patch_cycle_terminates() {
        let game = TempGame::new("cycle");
        game.write(
            "materials/a.vmt",
            r#""patch" { include "materials/b.vmt"  replace { "$alpha" "1" } }"#,
        );
        game.write(
            "materials/b.vmt",
            r#""patch" { include "materials/a.vmt" }"#,
        );

        // Ten levels deep the walk gives up and hands back what it has, which
        // still names `patch` as its shader — an unknown shader, which the
        // material layer turns into the error material.
        let vmt = Vmt::load(&game.mount(), "a").unwrap();
        assert!(vmt.shader.eq_ignore_ascii_case("patch"));
    }

    #[test]
    fn loading_a_real_file_goes_through_the_materials_directory() {
        let game = TempGame::new("path");
        game.write(
            "materials/metal/wall.vmt",
            r#""UnlitGeneric" { "$basetexture" "metal/wall" }"#,
        );
        let vfs = game.mount();

        assert_eq!(
            Vmt::load(&vfs, "metal/wall").unwrap().shader,
            "UnlitGeneric"
        );
        assert!(matches!(
            Vmt::load(&vfs, "metal/absent"),
            Err(VmtError::Read(_))
        ));
    }
}
