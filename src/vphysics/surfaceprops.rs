//! The surface property database — `scripts/surfaceproperties*.txt`.
//!
//! `legacy/vphysics/physics_material.cpp`, reduced to the two numbers that
//! reach a simulation: **friction** and **elasticity**. `density`, `thickness`
//! and `dampening` are read because they are cheap and because the file is the
//! only record of them, but nothing here consumes them — they belong to
//! buoyancy, `func_breakable` and impact sounds, none of which is ported
//! (`portdocs/VPHYSICS.md` §9).
//!
//! # Two parsing rules that are easy to miss and both matter
//!
//! `CPhysicsSurfaceProps::ParseSurfaceData` (`physics_material.cpp:400`) opens
//! each block by copying from somewhere, and *which* somewhere is not obvious:
//!
//! 1. **A name that is already defined re-opens that definition.** The block
//!    starts from the existing surface and overwrites it in place rather than
//!    appending a second one. That is what the manifest's three-file order is
//!    for: `surfaceproperties_portal2.txt` amends what `surfaceproperties.txt`
//!    established.
//! 2. **A name that is new starts from `"default"`** — before its own `base`
//!    key is looked at. So `default_silent`, whose entire body is
//!    `"gamematerial" "X"`, is friction 0.8 and elasticity 0.25 rather than
//!    zero, and a block with neither `base` nor any physics key is a copy of
//!    `default` under a new name.
//!
//! `base` is then applied **in file order, like any other key**, and copies
//! the whole of the named surface over whatever has accumulated. The shipped
//! files say so in a comment — *"base must appear as the first key in a
//! material"* (`surfaceproperties_portal.txt:35`) — which is a convention the
//! parser does not enforce and this one does not either.
//!
//! An unknown `base` is a no-op rather than an error, because
//! `CopyPhysicsProperties` takes `GetSurfaceIndex`'s -1 and returns without
//! copying.
//!
//! # How two surfaces combine, and the one place this port cannot follow
//!
//! IVP **multiplies** both coefficients across the contacting pair
//! (`ivp_material.cxx:13`) and then **clamps the product** to `[0, 1]`
//! (`physics_material.cpp:79` and `:94`). Rapier multiplies —
//! `CoefficientCombineRule::Multiply` — but has no rule that clamps
//! afterwards, so [`Surface::restitution`] clamps each *factor* instead.
//!
//! The two agree exactly wherever both factors are already in range, which is
//! every pair Portal 2 can form except those involving one of the handful of
//! deliberately superelastic surfaces — `energyball` at 1000, `metal_bouncy`
//! at 1000, and three more between 1.2 and 3. `surface_properties_bound_their_
//! own_coefficients` names them.

use std::collections::HashMap;

use crate::filesystem::keyvalues::{self, Block, Value};

/// One surface property. `surfacedata_t::physics`
/// (`legacy/public/vphysics_interface.h:943`).
#[derive(Debug, Clone, PartialEq)]
pub struct Surface {
    /// As written in the file, with its original case.
    pub name: String,
    /// `surfacephysicsparams_t::friction`. Dimensionless; Valve's comment is
    /// "value of 1.0 means object stands on a 45 degree hill".
    pub friction: f32,
    /// `surfacephysicsparams_t::elasticity` — the coefficient of restitution.
    /// **Not clamped here**; see [`Surface::restitution`].
    pub elasticity: f32,
    /// kg/m³. Read, never used — see the module docs.
    pub density: f32,
    /// Inches, for sheet materials. Read, never used.
    pub thickness: f32,
    /// Read, never used.
    pub dampening: f32,
}

impl Default for Surface {
    /// All zeros, which is what `memset( &prop.data, 0, sizeof(prop.data) )`
    /// leaves behind and therefore what the very first block — `"default"`
    /// itself, whose own name is not yet in the table — starts from.
    fn default() -> Surface {
        Surface {
            name: String::new(),
            friction: 0.0,
            elasticity: 0.0,
            density: 0.0,
            thickness: 0.0,
            dampening: 0.0,
        }
    }
}

impl Surface {
    /// What goes on a Rapier collider: [`elasticity`](Surface::elasticity)
    /// clamped to `[0, 1]`.
    ///
    /// See the module docs for why the clamp is here rather than on the
    /// product, and for the five Portal 2 surfaces where that is observable.
    pub fn restitution(&self) -> f32 {
        self.elasticity.clamp(0.0, 1.0)
    }

    /// What goes on a Rapier collider: [`friction`](Surface::friction) clamped
    /// to `[0, 1]`, which is `CIVPMaterialManager::get_friction_factor`'s own
    /// `clamp(factor, 0.0, 1.0)` moved one step earlier.
    pub fn friction_coefficient(&self) -> f32 {
        self.friction.clamp(0.0, 1.0)
    }
}

/// The parsed database. `CPhysicsSurfaceProps`.
///
/// Surfaces keep their file order, because that order **is** the index space a
/// map's `materialtable` and a ledge's seven-bit `material_index` refer to.
#[derive(Debug, Clone, Default)]
pub struct SurfaceProps {
    surfaces: Vec<Surface>,
    /// Lowercased name → index. `GetSurfaceIndex` is case-insensitive.
    by_name: HashMap<String, usize>,
}

/// The index `"default"` resolves to when the database is empty — which is
/// what every test that does not load the game's scripts sees.
static FALLBACK: Surface = Surface {
    name: String::new(),
    // `CPhysicsSurfaceProps::ParseSurfaceData`'s reserved shadow material
    // (`physics_material.cpp:608`), which is the only default Valve writes in
    // code rather than in content.
    friction: 0.8,
    elasticity: 1e-3,
    density: 2000.0,
    thickness: 0.0,
    dampening: 0.0,
};

impl SurfaceProps {
    /// Reads the files a `surfaceproperties_manifest.txt` names, **in the
    /// order it names them**, each as `(name, text)`.
    ///
    /// `AddFileToDatabase` refuses a file whose *name* it has already seen —
    /// "The physics system does not understand mods and will not parse the
    /// same file (compared by name) twice", as the shipped manifest's own
    /// comment puts it — so duplicates in the list are skipped rather than
    /// applied twice.
    pub fn parse(files: &[(&str, &str)]) -> SurfaceProps {
        let mut out = SurfaceProps::default();
        let mut seen: Vec<&str> = Vec::new();
        for (name, text) in files {
            if seen.iter().any(|s| s.eq_ignore_ascii_case(name)) {
                continue;
            }
            seen.push(name);
            let Ok(block) = keyvalues::parse(name, text) else {
                continue;
            };
            out.add(&block);
        }
        out
    }

    /// The file names a manifest lists, in order.
    ///
    /// `SURFACEPROP_MANIFEST_FILE` is `scripts/surfaceproperties_manifest.txt`
    /// (`physics_shared.cpp:41`) and its shape is a single block of repeated
    /// `"file"` keys — which is why this returns a `Vec` and not a map.
    pub fn manifest(name: &str, text: &str) -> Vec<String> {
        let Ok(block) = keyvalues::parse(name, text) else {
            return Vec::new();
        };
        let Some(inner) = block.first_block() else {
            return Vec::new();
        };
        inner
            .values()
            .filter(|(key, _)| key.eq_ignore_ascii_case("file"))
            .map(|(_, value)| value.to_owned())
            .collect()
    }

    fn add(&mut self, document: &Block) {
        for entry in document.entries() {
            let Value::Block(body) = &entry.value else {
                continue;
            };
            let key = entry.key.to_ascii_lowercase();
            // Rule 1, then rule 2: an existing name re-opens itself, a new one
            // starts from `default`.
            let mut surface = match self.by_name.get(&key) {
                Some(&at) => self.surfaces[at].clone(),
                None => self.find("default").cloned().unwrap_or_default(),
            };
            surface.name = entry.key.clone();
            for (field, value) in body.values() {
                let number = || value.trim().parse::<f32>().ok();
                match field.to_ascii_lowercase().as_str() {
                    // Applied in file order, exactly as the C++ does: a `base`
                    // after a `friction` discards that friction.
                    "base" => {
                        if let Some(from) = self.find(value) {
                            let name = std::mem::take(&mut surface.name);
                            surface = from.clone();
                            surface.name = name;
                        }
                    }
                    "friction" => surface.friction = number().unwrap_or(surface.friction),
                    "elasticity" => surface.elasticity = number().unwrap_or(surface.elasticity),
                    "density" => surface.density = number().unwrap_or(surface.density),
                    "thickness" => surface.thickness = number().unwrap_or(surface.thickness),
                    "dampening" => surface.dampening = number().unwrap_or(surface.dampening),
                    // Everything else is a game, audio or sound-script field.
                    // Valve asserts on an unrecognised key; this does not,
                    // because half the keys in the shipped files are ones this
                    // port has deliberately not got.
                    _ => {}
                }
            }
            match self.by_name.get(&key) {
                Some(&at) => self.surfaces[at] = surface,
                None => {
                    self.by_name.insert(key, self.surfaces.len());
                    self.surfaces.push(surface);
                }
            }
        }
    }

    /// `GetSurfaceIndex` — case-insensitive, `None` for a name the database
    /// has not got.
    pub fn index(&self, name: &str) -> Option<usize> {
        self.by_name.get(&name.to_ascii_lowercase()).copied()
    }

    /// The surface at an index, or `None`.
    pub fn at(&self, index: usize) -> Option<&Surface> {
        self.surfaces.get(index)
    }

    /// `GetSurfaceIndex` followed by the lookup, without the index.
    pub fn find(&self, name: &str) -> Option<&Surface> {
        self.index(name).and_then(|at| self.at(at))
    }

    /// What a body gets when its `.phy` names a surface property this database
    /// has not got, or names none at all.
    ///
    /// `PhysModelCreate` passes `-1` in that case and `CreatePhysicsObject`
    /// turns it into `GetSurfaceIndex("default")`
    /// (`physics_object.cpp:1486`). When even *that* is missing — which is
    /// every unit test that does not load the game's scripts — the reserved
    /// material's numbers stand in.
    pub fn resolve(&self, name: &str) -> &Surface {
        self.find(name)
            .or_else(|| self.find("default"))
            .unwrap_or(&FALLBACK)
    }

    /// How many surfaces the database holds — 92 for Portal 2.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.surfaces.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    /// Every surface, in file order — which is the index space a map's
    /// `materialtable` refers to.
    #[allow(dead_code)]
    pub fn iter(&self) -> impl Iterator<Item = &Surface> {
        self.surfaces.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
        "default"
        {
            "density"    "2000"
            "elasticity" "0.25"
            "friction"   "0.8"
            "gamematerial" "C"
        }
        "solidmetal"
        {
            "density"    "2700"
            "elasticity" "0.1"
            "friction"   "0.8"
        }
        "metal"
        {
            "base"       "solidmetal"
            "elasticity" "0.25"
            "thickness"  "0.1"
        }
        "default_silent"
        {
            "gamematerial" "X"
        }
        "glass"
        {
            "density"    "2700"
            "elasticity" "0.2"
            "friction"   "0.5"
        }
    "#;

    const PORTAL2: &str = r#"
        "Metal_Box"
        {
            "base"       "solidmetal"
            "thickness"  "0.1"
        }
        "reflective"
        {
            "base"       "metal"
        }
        "energyball"
        {
            "base"       "glass"
            "elasticity" "1000"
        }
    "#;

    fn props() -> SurfaceProps {
        SurfaceProps::parse(&[("a.txt", BASE), ("b.txt", PORTAL2)])
    }

    #[test]
    fn base_copies_the_whole_of_what_it_names() {
        let p = props();
        let metal = p.find("metal").expect("metal");
        assert_eq!(metal.friction, 0.8, "inherited from solidmetal");
        assert_eq!(metal.density, 2700.0, "inherited");
        assert_eq!(metal.elasticity, 0.25, "overridden after the base");
        assert_eq!(metal.thickness, 0.1);
    }

    /// The rule that catches people out, and the reason `default_silent` is in
    /// the fixture: a block with no `base` and no physics keys is **not**
    /// zeroed, it is a copy of `default`.
    #[test]
    fn a_block_that_declares_nothing_physical_inherits_default() {
        let p = props();
        let silent = p.find("default_silent").expect("default_silent");
        assert_eq!(silent.friction, 0.8);
        assert_eq!(silent.elasticity, 0.25);
        assert_eq!(silent.density, 2000.0);
    }

    #[test]
    fn a_later_file_amends_an_earlier_definition_rather_than_shadowing_it() {
        let before = props().len();
        let amended = SurfaceProps::parse(&[
            ("a.txt", BASE),
            ("b.txt", PORTAL2),
            ("c.txt", r#" "glass" { "friction" "0.25" } "#),
        ]);
        assert_eq!(amended.len(), before, "no second `glass` was appended");
        let glass = amended.find("glass").unwrap();
        assert_eq!(glass.friction, 0.25, "overwritten");
        assert_eq!(glass.elasticity, 0.2, "and the rest survives");
    }

    #[test]
    fn the_same_file_is_never_parsed_twice() {
        let once = SurfaceProps::parse(&[("a.txt", BASE)]);
        let twice = SurfaceProps::parse(&[("a.txt", BASE), ("A.TXT", BASE)]);
        assert_eq!(once.len(), twice.len());
    }

    #[test]
    fn lookup_is_case_insensitive_the_way_get_surface_index_is() {
        let p = props();
        assert_eq!(p.index("metal_box"), p.index("Metal_Box"));
        assert_eq!(p.find("METAL_BOX").unwrap().friction, 0.8);
    }

    /// The cube's own chain, end to end: `metal_box` → `solidmetal`.
    #[test]
    fn the_weighted_cubes_surface_property_resolves_to_solid_metal() {
        let p = props();
        let cube = p.resolve("Metal_Box");
        assert_eq!(cube.friction, 0.8);
        assert_eq!(cube.elasticity, 0.1);
        assert_eq!(cube.density, 2700.0);
    }

    #[test]
    fn an_unknown_name_falls_back_to_default_and_then_to_valves_reserved_one() {
        let p = props();
        assert_eq!(p.resolve("not_a_material").friction, 0.8);
        assert_eq!(p.resolve("not_a_material").elasticity, 0.25, "default's");

        // With no database at all — every test that has no game directory.
        let empty = SurfaceProps::default();
        assert_eq!(empty.resolve("anything").friction, 0.8);
        assert_eq!(empty.resolve("anything").elasticity, 1e-3);
    }

    /// The one place this port cannot follow IVP: Valve clamps the *product*
    /// and Rapier can only clamp the factors. See the module docs.
    #[test]
    fn surface_properties_bound_their_own_coefficients() {
        let p = props();
        let ball = p.find("energyball").expect("energyball");
        assert_eq!(ball.elasticity, 1000.0, "as written in the file");
        assert_eq!(ball.restitution(), 1.0, "as it reaches a collider");
        assert_eq!(p.find("metal").unwrap().restitution(), 0.25);
    }

    #[test]
    fn the_manifest_is_a_list_of_files_in_order() {
        let text = r#"
            surfaceproperties_manifest
            {
                "file" "scripts/surfaceproperties.txt"
                "file" "scripts/surfaceproperties_portal.txt"
                "file" "scripts/surfaceproperties_portal2.txt"
            }
        "#;
        assert_eq!(
            SurfaceProps::manifest("m.txt", text),
            vec![
                "scripts/surfaceproperties.txt",
                "scripts/surfaceproperties_portal.txt",
                "scripts/surfaceproperties_portal2.txt",
            ]
        );
    }
}
