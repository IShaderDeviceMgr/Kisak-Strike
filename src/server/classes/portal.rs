//! `prop_portal` — the hole in the wall, minus the hole.
//!
//! `game/server/portal/prop_portal.cpp`'s `CProp_Portal` and the half of
//! `game/server/portal/portal_base2d.cpp`'s `CPortal_Base2D` that is about
//! *placement and linkage* rather than about simulation. This is
//! `portdocs/PORTAL.md` **stage 2 of five**: the class exists, it links to its
//! partner, it computes the teleport matrix, and `world/` draws a coloured
//! oval where it is. It does not carve the wall (stage 3) and it does not
//! teleport anybody (stage 4).
//!
//! ```text
//!     21  prop_portal   CProp_Portal : CPortal_Base2D : CBaseAnimating
//! ```
//!
//! **Twenty-one entities across ten of the 106 maps, and two of them are on
//! `sp_a1_intro1`** — which makes this the rare module whose test bed is the
//! map the port already loads by default. All 21 start `Activated 0`, none
//! writes `LinkageGroupID`, and none writes `HalfWidth`/`HalfHeight`, so the
//! whole of shipped content is "two default-sized portals in group 0, switched
//! on by map logic". The game fires **31 `SetActivatedState`** and **4
//! `NewLocation`** at one, and carries exactly **one** output connection on one
//! — `sp_a1_intro1`'s `portal_red_0.OnPlayerTeleportFromMe`.
//!
//! # The four things here that read as bugs until you check the reference
//!
//! **Linkage is by group and size, never by `PortalTwo`.**
//! [`update_linkage`](PropPortal::update_linkage) takes the first portal in the
//! group that is active, unlinked and the same size, and then *overwrites*
//! `PortalTwo` from the partner. So the key decides colour and nothing else —
//! `portal_base2d.h:38` says so in as many words ("For teleportation, this
//! doesn't matter, but for drawing and moving, it matters").
//!
//! **The portal that activates *second* keeps its colour.** The forcing line
//! is `m_bIsPortal2 = !m_hLinkedPortal->m_bIsPortal2` in the *base* class's
//! `UpdatePortalLinkage` (`portal_base2d.cpp:1324`), and it runs on the
//! partner first (through the recursion at `prop_portal.cpp:584`) and on the
//! activating portal second — by which time the partner has already been made
//! the opposite, so the second assignment is a no-op. Both of `sp_a1_intro1`'s
//! are already opposite, so nothing moves there; a map that activated two
//! blues would end up with the *first* turned orange.
//!
//! **The teleport matrix has a 180° turn about up baked into it**, and it is
//! the single easiest thing in this module to leave out. See
//! [`teleport_matrix`].
//!
//! **A portal's `right` is the *negation* of its angle matrix's second
//! column.** `UpdatePortalTeleportMatrix` reads the three columns out and then
//! writes `m_vRight = -m_vRight` (`portal_base2d.cpp:1213`), because Valve's
//! `matrix3x4_t` column 1 is *left*. Everything that consumes the basis —
//! `UpdateCorners`, the quad the renderer draws, the matrix itself — is
//! written against the negated form, so [`PropPortal::right`] does the
//! negation once and nothing downstream repeats it.
//!
//! # Deliberately absent, each with what would bring it back
//!
//! - **The placement snap.** `CProp_Portal::ActivatePortal` (`:700`) traces one
//!   unit in front of the portal to eight units behind it, takes the surface
//!   normal as the new angles, and re-places itself there through
//!   `VerifyPortalPlacementAndFizzleBlockingPortals`. That is the placement
//!   system, which needs the gun (`portdocs/PORTAL.md` §8), and this port
//!   activates a portal where the map put it. The assumption is checked rather
//!   than assumed: `server::tests::every_shipped_portal_is_on_a_wall` sweeps
//!   the player hull backwards through all 21 and asserts each is within a
//!   unit of solid geometry.
//! - **`Fizzle`'s effect.** `InputFizzle` is `DoFizzleEffect` (particles and a
//!   sound) followed by `DeactivatePortalNow`; the second half is here and the
//!   first has no subsystem to run in. **No shipped map fires it.**
//! - **The microphone and speaker pair**, the ambient loop, the placement
//!   particles, the portal detectors (`func_portal_detector`, 31 entities, not
//!   ported), `CPhysicsCloneArea`, `CFunc_Portalled`, `SetMobileState` (one
//!   map, `sv_allow_mobile_portals` defaults to 0) and `PunchAllPenetratingPlayers`.
//! - **The five outputs are declared and none can fire yet**, because all five
//!   are teleport notifications and nothing teleports until stage 4. They are
//!   declared so that the one shipped connection parses as an output rather
//!   than as an unknown key, which is the same reason `prop_floor_button`
//!   declares its two co-op outputs.

use glam::{Mat4, Vec3};

use crate::math::angle_matrix;
use crate::server::class::{Behaviour, Context, InputDef, InputDefs, ModelState, SpawnResult};
use crate::server::entity::{EntityCore, EntityId};
use crate::server::io::{FieldType, Input};
use crate::server::keyvalue::{atof, atoi, string_to_float_array};
use crate::server::movement::{ModelBounds, MoveType, Solid, FSOLID_NOT_SOLID, FSOLID_TRIGGER};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// `PORTAL_LINKAGE_GROUP_INVALID` (`prop_portal.h:23`) — "not in any group".
///
/// A portal with this id is never added to a group and so can never link.
/// Nothing in the shipped game writes it: it is what `InputSetLinkageGroupId`
/// refuses and what a portal the gun has not placed yet would carry.
pub const LINKAGE_GROUP_INVALID: u8 = 255;

/// `DEFAULT_PORTAL_HALF_WIDTH` (`prop_portal_shared.h:24`).
pub const DEFAULT_HALF_WIDTH: f32 = 32.0;

/// `DEFAULT_PORTAL_HALF_HEIGHT` (`prop_portal_shared.h:25`) — **56, and the
/// tree's own file-scope initializer says 14.**
///
/// `prop_portal_shared.cpp:167` reads
/// `ms_DefaultPortalHalfHeight = 0.25 * DEFAULT_PORTAL_HALF_HEIGHT`, under a
/// comment that says exactly what it is: *"default to sane-looking but
/// incorrect portal height for CEG - Updated in constructor"*. The constructor
/// then overwrites it with `CEG_GET_CONSTANT_VALUE( DefaultPortalHalfHeight )`,
/// an anti-tamper macro whose real value is not in this tree. The `#define` is,
/// and the shipped game's portal is 64 x 112 units, so 56 it is.
pub const DEFAULT_HALF_HEIGHT: f32 = 56.0;

/// How far in front of its own plane a portal's trigger box reaches —
/// `GetLocalMaxs().x` (`portal_base2d.h:174`).
///
/// The box is `(0, -halfWidth, -halfHeight)` to `(64, halfWidth, halfHeight)`
/// in the portal's own frame, so it is one-sided: it covers the room in front
/// of the portal and nothing behind it.
pub const OBB_DEPTH: f32 = 64.0;

/// The two models `ResetModel` (`prop_portal.cpp:265`) picks between.
///
/// **Neither is drawn, and that is correct.** Both wear
/// `models/portals/portal_1_anims.vmt`, whose shader is `writez` — a
/// depth-only material that exists to punch a hole for the recursive view to
/// composite into. `PropPortal::model_state` therefore answers `None`, which
/// keeps the model out of the seam entirely; carrying it would draw a
/// four-vertex quad in the error checkerboard, because `writez` is not a
/// shader this port has. The visible portal is `world/`'s overlay quad.
pub const MODEL_PORTAL_1: &str = "models/portals/portal1.mdl";
pub const MODEL_PORTAL_2: &str = "models/portals/portal2.mdl";

// ---------------------------------------------------------------------------
// The teleport matrix
// ---------------------------------------------------------------------------

/// `UTIL_Portal_ComputeMatrix_ForReal` (`portal_util_shared.cpp:2802`) — the
/// transform that takes a point at the entrance and puts it at the exit.
///
/// Read it as three steps rather than as a product, which is how it survives
/// the change of matrix convention: **into the entrance's frame, turn around,
/// out of the exit's frame.**
///
/// ```text
///     T(exit.origin) * R(exit.angles) * diag(-1,-1,1) * R(entrance.angles)^T * T(-entrance.origin)
/// ```
///
/// # The 180° is the whole thing
///
/// `diag(-1, -1, 1)` is a half turn about the portal's *up* axis, and leaving
/// it out is the failure this module is most likely to have: the picture is
/// plausible — a point near the entrance plane still lands near the exit plane
/// — and everything comes out facing backwards, as though the exit portal were
/// mirrored. `CPortal_Base2D_Shared::UpdatePortalTransformationMatrix`
/// (`portal_base2d_shared.cpp:78`) writes it as an explicit `matRotation` with
/// `m[0][0] = m[1][1] = -1`; the function this is actually ported from folds
/// it into the sign of the first two rows.
///
/// # What it means, which is not what it first looks like
///
/// A point **in front of** the entrance maps to **behind** the exit, and that
/// is right rather than inverted. It is what makes the matrix a *camera*
/// transform — to draw the view through portal A you put a virtual eye behind
/// portal B looking out through it — and the teleport reads the same way from
/// the other side: the player crosses the entrance plane, so the point being
/// transformed is a hair *behind* it, and it comes out a hair in *front* of
/// the exit.
///
/// # Convention
///
/// Valve's `VMatrix` is row-major with vectors on the right; this port's
/// `Mat4` is column-major with vectors on the left
/// (`rustdocs/MATERIALS.md`'s first rule). A transcribed product is silently
/// inverted, so this is derived from the geometry instead. The one place the
/// two spellings can be compared is
/// `crate::math::angle_matrix`, whose columns are Valve's
/// `matrix3x4_t` columns — forward, **left**, up — which is exactly the basis
/// `VMatrix( m_vForward, -m_vRight, m_vUp )` builds at
/// `portal_util_shared.cpp:2824`.
///
/// Both arguments are `(origin, angles)`; `angles` is pitch, yaw, roll.
pub fn teleport_matrix(entrance: (Vec3, Vec3), exit: (Vec3, Vec3)) -> Mat4 {
    let (entrance_origin, entrance_angles) = entrance;
    let (exit_origin, exit_angles) = exit;

    // World -> entrance frame. The rotation is orthonormal, so the inverse is
    // the transpose — `VectorIRotate`, and the reason Valve's comment at
    // `:2795` about avoiding `MatrixInverseTR` does not apply here: that note
    // is about client and server having to agree bit for bit, and this port is
    // one process.
    let into_entrance = Mat4::from_mat3(angle_matrix(entrance_angles).transpose())
        * Mat4::from_translation(-entrance_origin);

    // The half turn about up, in portal-local axes (x forward, y left,
    // z up).
    let flip = Mat4::from_cols(
        [-1.0, 0.0, 0.0, 0.0].into(),
        [0.0, -1.0, 0.0, 0.0].into(),
        [0.0, 0.0, 1.0, 0.0].into(),
        [0.0, 0.0, 0.0, 1.0].into(),
    );

    // Exit frame -> world.
    let out_of_exit =
        Mat4::from_translation(exit_origin) * Mat4::from_mat3(angle_matrix(exit_angles));

    out_of_exit * flip * into_entrance
}

// ---------------------------------------------------------------------------
// CProp_Portal
// ---------------------------------------------------------------------------

/// `CProp_Portal` (`prop_portal.cpp:79`) — one half of a linked pair.
pub struct PropPortal {
    /// `m_bActivated` (`Activated`). **All 21 shipped portals write `0`**, so
    /// every portal in the game starts switched off and is turned on by map
    /// logic through `SetActivatedState`.
    pub activated: bool,
    /// `m_bOldActivatedState` (`OldActivated`) — what
    /// [`activated`](PropPortal::activated) was before the last
    /// [`set_active`](PropPortal::set_active).
    ///
    /// A keyfield in the datadesc and written by no shipped map; its one
    /// reader is `CProp_Portal::NewLocation`, deciding whether a move also
    /// turned the portal on.
    pub old_activated: bool,
    /// `m_bIsPortal2` (`PortalTwo`) — **colour, and nothing else**.
    ///
    /// It picks the model and the overlay material. It does *not* pick the
    /// partner, and linking overwrites it — see the module docs.
    pub is_portal2: bool,
    /// `m_iLinkageGroupID` (`LinkageGroupID`), `FIELD_CHARACTER`.
    ///
    /// Which pool this portal may find a partner in. No shipped map writes the
    /// key, so all 21 are group 0 and every pair in the game is found in the
    /// same pool. [`LINKAGE_GROUP_INVALID`] means "in no pool".
    pub linkage_group: u8,
    /// `m_fNetworkHalfWidth` (`HalfWidth`). Half of 64.
    pub half_width: f32,
    /// `m_fNetworkHalfHeight` (`HalfHeight`). Half of 112 — see
    /// [`DEFAULT_HALF_HEIGHT`], which the reference tree gets wrong on purpose.
    pub half_height: f32,
    /// `m_hLinkedPortal`. `None` until two portals in a group are active at
    /// once.
    pub linked: Option<EntityId>,
    /// `m_matrixThisToLinked`, [`teleport_matrix`] of this portal and its
    /// partner.
    ///
    /// **The identity while unlinked**, which is Valve's
    /// `matPortal.Identity(); //don't accidentally teleport objects to zero
    /// space`. Use it as `matrix.transform_point3(p)` for a position and
    /// `matrix.transform_vector3(v)` for a velocity; the third form,
    /// `UTIL_Portal_AngleTransform`, arrives with the teleport in stage 4.
    pub matrix: Mat4,
    /// The server clock at the last activation or move — what the *renderer*
    /// measures `$PortalOpenAmount` and `$PortalStatic` from.
    ///
    /// `C_Prop_Portal::OnActiveStateChanged` (`c_prop_portal.cpp:425`) sets
    /// `m_fOpenAmount = 0` and `m_fStaticAmount = 1` and lets `ClientThink`
    /// run them back up; `OnPortalMoved` resets the first alone. Both are
    /// pure functions of "how long ago was that", so the seam carries the
    /// timestamp and `world/` derives the two curves — the same split
    /// `ModelState::anim_time` already has, and for the same reason: a 64 Hz
    /// tick would step an effect that should be smooth.
    pub opened_at: f32,
}

/// The inputs (`prop_portal.cpp:66`).
///
/// **`NewLocation` and `Resize` are `FIELD_STRING`** and parse their own
/// numbers out of the value — six floats for the first, two for the second —
/// which is why neither is a `FIELD_VECTOR`.
pub static PORTAL_INPUTS: InputDefs = &[
    InputDef::new("SetActivatedState", FieldType::Bool),
    InputDef::new("Fizzle", FieldType::Void),
    InputDef::new("NewLocation", FieldType::String),
    InputDef::new("Resize", FieldType::String),
    InputDef::new("SetLinkageGroupId", FieldType::Int),
];

/// The outputs (`portal_base2d.cpp:97`). All five are teleport notifications
/// and none can fire before stage 4; see the module docs.
pub static PORTAL_OUTPUTS: &[&str] = &[
    "OnPlacedSuccessfully",
    "OnEntityTeleportFromMe",
    "OnPlayerTeleportFromMe",
    "OnEntityTeleportToMe",
    "OnPlayerTeleportToMe",
];

/// The keys, split across the two classes: `LinkageGroupID` is
/// `CProp_Portal`'s and the other five are `CPortal_Base2D`'s.
///
/// Measured over the 106 shipped maps: **only `Activated` and `PortalTwo` are
/// ever written**, 21 times each. The other four are declared because the
/// datadesc declares them and because a hand-made map could write one.
pub static PORTAL_KEYS: &[&str] = &[
    "LinkageGroupID",
    "Activated",
    "OldActivated",
    "PortalTwo",
    "HalfWidth",
    "HalfHeight",
];

impl PropPortal {
    pub(super) fn create() -> Box<dyn Behaviour> {
        Box::new(PropPortal {
            activated: false,
            old_activated: false,
            is_portal2: false,
            linkage_group: 0,
            half_width: DEFAULT_HALF_WIDTH,
            half_height: DEFAULT_HALF_HEIGHT,
            linked: None,
            matrix: Mat4::IDENTITY,
            opened_at: 0.0,
        })
    }

    /// `m_vForward` — `GetVectors( &vForward, ... )`, the first column of the
    /// angle matrix. The direction the portal faces *out of* the wall.
    pub fn forward(entity: &EntityCore) -> Vec3 {
        angle_matrix(entity.angles) * Vec3::X
    }

    /// `m_vRight` — **the negated second column**, which is what
    /// `UpdatePortalTeleportMatrix` stores (`portal_base2d.cpp:1213`).
    ///
    /// `angle_matrix`'s column 1 is Valve's *left*; every consumer of the
    /// portal basis wants right.
    pub fn right(entity: &EntityCore) -> Vec3 {
        -(angle_matrix(entity.angles) * Vec3::Y)
    }

    /// `m_vUp` — the third column.
    pub fn up(entity: &EntityCore) -> Vec3 {
        angle_matrix(entity.angles) * Vec3::Z
    }

    /// `m_plane_Origin` (`portal_base2d.cpp:1230`) — the portal's own plane,
    /// as `(normal, distance)`.
    ///
    /// # No caller yet
    ///
    /// This, [`corners`](PropPortal::corners) and
    /// [`is_floor_portal`](PropPortal::is_floor_portal) are the three pieces of
    /// `CPortal_Base2D` that stage 2 builds and stage 4 reads: the plane is
    /// what `HandlePortalling` tests the player's centre against, the corners
    /// are the quad it then checks the crossing is inside, and the floor test
    /// picks the minimum exit speed. They are here rather than with the
    /// teleport because they are *placement*, they are one line each, and each
    /// is pinned by a test — which is what keeps the conventions they encode
    /// (the negated right vector, the plane through the origin rather than the
    /// near face) from having to be rediscovered.
    ///
    /// **This is the plane the teleport triggers on** (stage 4), and it is the
    /// plane through the portal's *origin*, not through the near face of its
    /// trigger box: the two are the same here only because the box starts at
    /// zero.
    #[allow(dead_code)]
    pub fn plane(entity: &EntityCore) -> (Vec3, f32) {
        let normal = Self::forward(entity);
        (normal, normal.dot(entity.origin))
    }

    /// `UpdateCorners` (`portal_base2d.cpp:1750`) — the four corners of the
    /// visible quad, in Valve's order: `+r+u`, `-r+u`, `-r-u`, `+r-u`.
    #[allow(dead_code)]
    pub fn corners(&self, entity: &EntityCore) -> [Vec3; 4] {
        let right = Self::right(entity) * self.half_width;
        let up = Self::up(entity) * self.half_height;
        let origin = entity.origin;
        [
            origin + right + up,
            origin - right + up,
            origin - right - up,
            origin + right - up,
        ]
    }

    /// `IsActivedAndLinked` (`portal_base2d_shared.cpp:874`) — Valve's
    /// spelling, typo and all.
    ///
    /// **It does not check that the partner is still alive**, only that this
    /// portal thinks it has one and is itself on; the handle resolving is the
    /// caller's problem. That is the C++'s shape too, where a stale handle
    /// reads as null.
    pub fn is_active_and_linked(&self) -> bool {
        self.activated && self.linked.is_some()
    }

    /// `IsFloorPortal( threshold )` (`portal_base2d_shared.cpp:879`) — is this
    /// portal in the floor, facing up?
    ///
    /// The threshold is a parameter because Valve calls it with two: the
    /// default `0.8` guards `PunchAllPenetratingPlayers`, and `0.9` is what
    /// the exit-speed rules use (stage 4).
    #[allow(dead_code)]
    pub fn is_floor_portal(entity: &EntityCore, threshold: f32) -> bool {
        Self::forward(entity).z > threshold
    }

    /// `SetActive` (`portal_base2d_shared.cpp:868`) — two assignments, and the
    /// order is the point: the old state is captured *before* the new one is
    /// written.
    fn set_active(&mut self, active: bool) {
        self.old_activated = self.activated;
        self.activated = active;
    }

    /// `CProp_Portal::UpdatePortalLinkage` (`prop_portal.cpp:544`) and the
    /// base class's (`portal_base2d.cpp:1316`), as one function.
    ///
    /// # What it does
    ///
    /// An *active* portal with no live partner searches its group, in spawn
    /// order, for the first other portal that is **active**, **unlinked** and
    /// **the same size**, and links to it. A *deactivated* portal drops its
    /// partner and lets the partner look for someone else.
    ///
    /// # Why it is not recursive here and is there
    ///
    /// The C++ recurses into `pOther->UpdatePortalLinkage()` in three places,
    /// and reading what each one actually does collapses all three:
    ///
    /// - into a partner that is now linked to us (`prop_portal.cpp:584`): it
    ///   takes the "already linked to an active portal" early exit, so all it
    ///   does is force that partner's `PortalTwo` and recompute its matrix;
    /// - into a partner that has gone *inactive* (`:557`): it takes the
    ///   deactivation branch, which just clears a link that is already clear;
    /// - into the partner of a portal that has just deactivated (`:598`): the
    ///   only one that can do real work — that partner is still active and now
    ///   unlinked, so it re-runs the search and may find a *third* portal.
    ///
    /// So the depth is two and the second level is the search, which is
    /// written out below. That matters because this module cannot recurse
    /// anyway: `Server::dispatch` has lifted this entity out of the list, so
    /// the partner is reachable as *data*
    /// ([`Context::behaviour_mut`]) and never as code.
    ///
    /// # The matrices
    ///
    /// Recomputed for the **whole group** at the end rather than for the
    /// portals this call touched. A portal's matrix is a pure function of its
    /// own placement and its partner's, the group is at most four entities in
    /// the shipped game (`sp_a1_intro2`), and the alternative is four
    /// bookkeeping paths that have to agree.
    ///
    /// [`Context::behaviour_mut`]: crate::server::class::Context::behaviour_mut
    fn update_linkage(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let me = entity.id();
        let group = self.linkage_group;

        if self.activated {
            // "no old link, or inactive old link"
            let live = self
                .linked
                .filter(|&id| read(cx, id).is_some_and(|other| other.activated));
            if live.is_none() {
                if let Some(old) = self.linked {
                    // The partner went inactive. Break its side of the link;
                    // its own `UpdatePortalLinkage` would do no more than this.
                    if let Some(other) = cx.behaviour_mut::<PropPortal>(old) {
                        if other.linked == Some(me) {
                            other.linked = None;
                        }
                    }
                }
                self.linked = find_partner(cx, me, group, self.half_width, self.half_height);
                if let Some(partner) = self.linked {
                    // The base class's forcing line, running on the *partner*
                    // first — which is what leaves the activating portal's own
                    // colour alone. See the module docs.
                    let colour = !self.is_portal2;
                    if let Some(other) = cx.behaviour_mut::<PropPortal>(partner) {
                        other.linked = Some(me);
                        other.is_portal2 = colour;
                    }
                }
            } else {
                self.linked = live;
            }

            // `BaseClass::UpdatePortalLinkage`'s active branch: make the link
            // symmetric and take the partner's colour. The second assignment is
            // a no-op when this call is the one that just forced the partner,
            // and is the whole effect when it is not.
            if let Some(partner) = self.linked {
                let colour = read(cx, partner).map(|other| other.is_portal2);
                if let Some(other) = cx.behaviour_mut::<PropPortal>(partner) {
                    other.linked = Some(me);
                }
                if let Some(colour) = colour {
                    self.is_portal2 = !colour;
                }
            }
        } else if let Some(remote) = self.linked.take() {
            // The deactivation branch, and its one recursion: the partner is
            // still active, so it looks for somebody else in the group.
            let remote_state = read(cx, remote).map(|other| {
                (
                    other.activated,
                    other.linkage_group,
                    other.half_width,
                    other.half_height,
                )
            });
            if let Some(other) = cx.behaviour_mut::<PropPortal>(remote) {
                other.linked = None;
            }
            if let Some((true, group, half_width, half_height)) = remote_state {
                if let Some(third) = find_partner(cx, remote, group, half_width, half_height) {
                    let colour = read(cx, third).map(|other| other.is_portal2);
                    if let Some(other) = cx.behaviour_mut::<PropPortal>(third) {
                        other.linked = Some(remote);
                    }
                    if let Some(other) = cx.behaviour_mut::<PropPortal>(remote) {
                        other.linked = Some(third);
                        if let Some(colour) = colour {
                            other.is_portal2 = !colour;
                        }
                    }
                }
            }
        }

        self.update_matrices(entity, cx);
    }

    /// `UpdatePortalTeleportMatrix` (`portal_base2d.cpp:1192`) for this portal
    /// and, through `UTIL_Portal_ComputeMatrix`, for everything whose partner
    /// may have changed.
    ///
    /// `UTIL_Portal_ComputeMatrix` computes **both** ends of a pair
    /// (`portal_util_shared.cpp:2841`), which is why this is a sweep rather
    /// than a single assignment.
    fn update_matrices(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        let me = (entity.origin, entity.angles);
        self.matrix = match self.linked.and_then(|id| cx.entity(id)) {
            Some(other) => teleport_matrix(me, (other.core.origin, other.core.angles)),
            None => Mat4::IDENTITY,
        };

        for id in portals_in_group(cx, self.linkage_group) {
            let Some(entrance) = cx.entity(id).map(|e| (e.core.origin, e.core.angles)) else {
                continue;
            };
            let exit = match read(cx, id).and_then(|portal| portal.linked) {
                // The partner may be the entity being dispatched, which is not
                // in the list — its placement is `me`.
                Some(partner) if partner == entity.id() => Some(me),
                Some(partner) => cx.entity(partner).map(|e| (e.core.origin, e.core.angles)),
                None => None,
            };
            let matrix = match exit {
                Some(exit) => teleport_matrix(entrance, exit),
                None => Mat4::IDENTITY,
            };
            if let Some(other) = cx.behaviour_mut::<PropPortal>(id) {
                other.matrix = matrix;
            }
        }
    }

    /// `CProp_Portal::NewLocation` (`prop_portal.cpp:635`) and the base's
    /// (`portal_base2d.cpp:1482`), reduced to what moves.
    ///
    /// **It activates the portal as a side effect** — `SetActive( true )` sits
    /// in the middle of the base class's version — so `NewLocation` on a
    /// switched-off portal switches it on. All four shipped connections fire
    /// it at a tractor-beam portal in `sp_a4_finale1`/`2` that is already on.
    ///
    /// Not here: `WakeNearbyEntities`, the microphone and speaker moves, the
    /// "did I land on something that moves" trace and the parenting it drives
    /// (`sv_allow_mobile_portals` is `0` outside one map), and
    /// `PunchAllPenetratingPlayers`.
    pub fn new_location(
        &mut self,
        entity: &mut EntityCore,
        origin: Vec3,
        angles: Vec3,
        cx: &mut Context<'_>,
    ) {
        entity.origin = origin;
        entity.angles = angles;
        self.set_active(true);
        self.opened_at = cx.curtime();
        self.update_linkage(entity, cx);
    }

    /// `CPortal_Base2D::Resize` (`portal_base2d.cpp:1700`).
    ///
    /// **Resizing breaks a link between differently-sized portals**, because
    /// the search requires an exact match on both half-extents: *"different
    /// portal sizes, unsupported, unlink. Scaling is a whole different ball of
    /// wax."* Nothing in the shipped game fires `Resize`.
    fn resize(
        &mut self,
        entity: &mut EntityCore,
        half_width: f32,
        half_height: f32,
        cx: &mut Context<'_>,
    ) {
        if half_width == self.half_width && half_height == self.half_height {
            return;
        }
        self.half_width = half_width;
        self.half_height = half_height;
        self.apply_size(entity);

        if let Some(partner) = self.linked {
            let mismatched = read(cx, partner)
                .is_some_and(|o| o.half_width != half_width || o.half_height != half_height);
            if mismatched {
                if let Some(other) = cx.behaviour_mut::<PropPortal>(partner) {
                    other.linked = None;
                }
                self.linked = None;
            }
        }
        self.update_linkage(entity, cx);
    }

    /// `ResetModel` (`prop_portal.cpp:265`) plus
    /// `CPortal_Base2D::UpdateCollisionShape` (`portal_base2d_shared.cpp:910`),
    /// which between them are the model name and the trigger box.
    ///
    /// The box is **one-sided**: `(0, -hw, -hh)` to `(+64, hw, hh)` in the
    /// portal's own frame, so it covers the room in front of the portal and
    /// nothing inside the wall. The mobile-portal variant that shortens it to
    /// 4 units is skipped along with the rest of `SetMobileState`.
    fn apply_size(&self, entity: &mut EntityCore) {
        entity.model = Some(
            match self.is_portal2 {
                false => MODEL_PORTAL_1,
                true => MODEL_PORTAL_2,
            }
            .to_owned(),
        );
        entity.model_bounds = ModelBounds {
            mins: Vec3::new(0.0, -self.half_width, -self.half_height),
            maxs: Vec3::new(OBB_DEPTH, self.half_width, self.half_height),
        };
    }
}

/// Every `prop_portal` in a linkage group, in spawn order.
///
/// `s_PortalLinkageGroups[iLinkageGroupID]` (`prop_portal.cpp:44`), which is a
/// file-scope array of 256 vectors maintained by `AddToLinkageGroup` and the
/// destructor. **This port scans instead**, and the scan is the honest trade: a
/// `static mut` cannot hold per-`Server` state (every test in this module
/// builds its own), the list is in spawn order either way because
/// `AddToLinkageGroup` runs in `Spawn`, and the whole game has 21 portals with
/// no map holding more than four.
///
/// [`LINKAGE_GROUP_INVALID`] matches nothing, which is `AddToLinkageGroup`'s
/// own guard rather than a special case here.
fn portals_in_group(cx: &Context<'_>, group: u8) -> Vec<EntityId> {
    if group == LINKAGE_GROUP_INVALID {
        return Vec::new();
    }
    cx.find_all_of_class("prop_portal")
        .into_iter()
        .filter(|&id| read(cx, id).is_some_and(|portal| portal.linkage_group == group))
        .collect()
}

/// This portal's class state, if the handle still resolves to one.
fn read<'a>(cx: &'a Context<'_>, id: EntityId) -> Option<&'a PropPortal> {
    cx.entity(id)?.behaviour.downcast_ref::<PropPortal>()
}

/// The search loop of `CProp_Portal::UpdatePortalLinkage` (`:576`): the first
/// portal in the group that is active, unlinked, not `me`, and exactly the same
/// size.
///
/// **The size comparison is exact**, `==` on two `f32`s, and that is Valve's.
/// It is safe here because the only two values either half-extent ever takes
/// are the defaults and whatever a `Resize` input wrote, and both sides of a
/// pair get them by the same route.
fn find_partner(
    cx: &Context<'_>,
    me: EntityId,
    group: u8,
    half_width: f32,
    half_height: f32,
) -> Option<EntityId> {
    portals_in_group(cx, group).into_iter().find(|&id| {
        id != me
            && read(cx, id).is_some_and(|other| {
                other.activated
                    && other.linked.is_none()
                    && other.half_width == half_width
                    && other.half_height == half_height
            })
    })
}

impl Behaviour for PropPortal {
    fn key_value(&mut self, _entity: &mut EntityCore, key: &str, value: &str) -> bool {
        // `FIELD_CHARACTER`, so the value is truncated to a byte the way the
        // datadesc's own parser would — and `SetLinkageGroupId` refuses
        // anything outside 0..255 where a *key* is simply wrapped.
        if key.eq_ignore_ascii_case("LinkageGroupID") {
            self.linkage_group = atoi(value) as u8;
            return true;
        }
        if key.eq_ignore_ascii_case("Activated") {
            self.activated = atoi(value) != 0;
            return true;
        }
        if key.eq_ignore_ascii_case("OldActivated") {
            self.old_activated = atoi(value) != 0;
            return true;
        }
        if key.eq_ignore_ascii_case("PortalTwo") {
            self.is_portal2 = atoi(value) != 0;
            return true;
        }
        if key.eq_ignore_ascii_case("HalfWidth") {
            self.half_width = atof(value);
            return true;
        }
        if key.eq_ignore_ascii_case("HalfHeight") {
            self.half_height = atof(value);
            return true;
        }
        false
    }

    /// `CProp_Portal::Spawn` (`prop_portal.cpp:191`) and
    /// `CPortal_Base2D::Spawn` (`portal_base2d.cpp:309`).
    ///
    /// `AddToLinkageGroup` has no counterpart — [`portals_in_group`] is the
    /// group — and the size clamp is the one piece of `CProp_Portal::Spawn`
    /// that survives whole: a portal whose map wrote a zero or negative
    /// half-extent is put back to the default rather than being left with a
    /// degenerate trigger box.
    fn spawn(&mut self, entity: &mut EntityCore, _cx: &mut Context<'_>) -> SpawnResult {
        if self.half_width <= 0.0 || self.half_height <= 0.0 {
            self.half_width = DEFAULT_HALF_WIDTH;
            self.half_height = DEFAULT_HALF_HEIGHT;
        }
        self.apply_size(entity);

        // `SetSolid( SOLID_OBB )` and `SetSolidFlags( FSOLID_TRIGGER |
        // FSOLID_NOT_SOLID | FSOLID_CUSTOMBOXTEST | FSOLID_CUSTOMRAYTEST )`.
        // The two `CUSTOM*` bits route `ClipRayToCollideable` through
        // `CPortal_Base2D::TestCollision`, which is a box sweep against
        // exactly the box `model_bounds` now holds — so here they say nothing
        // the pair above does not, and the port has no flag for them.
        //
        // **`SetSolidFlags` replaces where `AddSolidFlags` would add**, which
        // is what keeps `FSOLID_TRIGGER_TOUCH_DEBRIS` and its neighbours off a
        // portal.
        entity.solid = Solid::Obb;
        entity.solid_flags = FSOLID_NOT_SOLID | FSOLID_TRIGGER;
        entity.move_type = MoveType::None;

        // `m_matrixThisToLinked.Identity(); //don't accidentally teleport
        // objects to zero space` — already the constructor's value here, and
        // restated because it is the invariant an unlinked portal has.
        self.matrix = Mat4::IDENTITY;
        SpawnResult::Ok
    }

    /// `CPortal_Base2D::Activate` (`portal_base2d.cpp:632`).
    ///
    /// The whole of what survives is `UpdatePortalLinkage`, and at level start
    /// it finds nothing: **all 21 shipped portals are `Activated 0`**, so every
    /// one of them takes the deactivation branch, which on a portal that has
    /// never been linked does nothing at all. The call is here because a
    /// hand-made map could ship `Activated 1` on a pair, and that pair must
    /// link without waiting for an input.
    fn activate(&mut self, entity: &mut EntityCore, cx: &mut Context<'_>) {
        self.update_linkage(entity, cx);
    }

    fn accept_input(
        &mut self,
        entity: &mut EntityCore,
        input: &Input<'_>,
        cx: &mut Context<'_>,
    ) -> bool {
        // `InputSetActivatedState` (`:777`) — 31 of the game's 35 connections
        // into a portal. `ActivatePortal`/`DeactivatePortal` reduce to the two
        // calls that are not sound, particles or placement.
        if input.name.eq_ignore_ascii_case("SetActivatedState") {
            let active = input.value.bool();
            self.set_active(active);
            if active && !self.old_activated {
                self.opened_at = cx.curtime();
            }
            self.update_linkage(entity, cx);
            return true;
        }

        // `InputFizzle` (`:791`) — `DoFizzleEffect` then `DeactivatePortalNow`.
        // The effect has no subsystem; what is left is the deactivation, and
        // it is *immediate* rather than scheduled, which is the difference
        // between `DeactivatePortalNow` and the commented-out
        // `DeactivatePortalOnThink` beside it. No shipped map fires this.
        if input.name.eq_ignore_ascii_case("Fizzle") {
            self.set_active(false);
            self.update_linkage(entity, cx);
            return true;
        }

        // `InputNewLocation` (`:801`) — six floats in one string, and the
        // comment above it says what it is for: *"Map can call new location,
        // so far it's only for debugging purposes so it's not made to be very
        // robust."* Four shipped connections fire it, all in
        // `sp_a4_finale1`/`2` and all sending both tractor-beam portals to the
        // same point.
        if input.name.eq_ignore_ascii_case("NewLocation") {
            let value = input.value.to_string();
            let mut numbers = [0.0f32; 6];
            string_to_float_array(&value, &mut numbers);
            let origin = Vec3::new(numbers[0], numbers[1], numbers[2]);
            let angles = Vec3::new(numbers[3], numbers[4], numbers[5]);
            self.new_location(entity, origin, angles, cx);
            return true;
        }

        // `InputResize` (`:828`) — two floats in one string.
        if input.name.eq_ignore_ascii_case("Resize") {
            let value = input.value.to_string();
            let mut numbers = [0.0f32; 2];
            string_to_float_array(&value, &mut numbers);
            self.resize(entity, numbers[0], numbers[1], cx);
            return true;
        }

        // `InputSetLinkageGroupId` (`:841`). **The bound is `< 255`, not
        // `<= 255`**, so `LINKAGE_GROUP_INVALID` cannot be reached by input —
        // and the re-link is spelled out as deactivate, change, reactivate so
        // that the portal leaves its old group's pool before it joins the new
        // one.
        if input.name.eq_ignore_ascii_case("SetLinkageGroupId") {
            let group = input.value.int();
            if !(0..255).contains(&group) {
                return true;
            }
            let was_active = self.activated;
            if was_active {
                self.set_active(false);
                self.update_linkage(entity, cx);
            }
            self.linkage_group = group as u8;
            if was_active {
                self.set_active(true);
                self.update_linkage(entity, cx);
            }
            return true;
        }

        false
    }

    /// **`None`, and that is the class working rather than a gap.**
    ///
    /// `portal1.mdl` is four vertices wearing `writez`, a depth-only shader
    /// that punches a hole for the recursive view to composite into. This port
    /// has no recursive view and no `writez`, so returning a model state would
    /// put a magenta error quad on the wall. See [`MODEL_PORTAL_1`].
    fn model_state(&self) -> Option<ModelState<'_>> {
        None
    }

    fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Activated", self.activated.to_string()),
            ("PortalTwo", self.is_portal2.to_string()),
            ("LinkageGroupID", self.linkage_group.to_string()),
            ("HalfWidth", self.half_width.to_string()),
            ("HalfHeight", self.half_height.to_string()),
            (
                "linked",
                match self.linked {
                    Some(id) => format!("{}", id.slot()),
                    None => "none".to_owned(),
                },
            ),
        ]
    }
}
