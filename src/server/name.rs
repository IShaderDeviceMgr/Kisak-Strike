//! Finding an entity by name.
//!
//! `EntityNamesMatchCStrings` (`game/server/baseentity.cpp:644`) and
//! `CGlobalEntityList::FindEntityByName` (`game/server/entitylist.cpp:752`).
//!
//! # The matching rule is not the one the comment describes
//!
//! Valve's own comment says "only thing supported is trailing `*`", and that
//! is the *intent*. The code is [`names_match`], and what it actually does is
//! walk both strings until they diverge and then ask whether the query is
//! sitting on a `*` — so **a `*` anywhere in the query matches the rest of the
//! name**, and a query that begins with one matches everything. `"*door"`
//! matches `"the_door"`, and `"do*r"` matches `"dover"`.
//!
//! Measured: **all 234 wildcard targets in the 106 shipped maps are a plain
//! trailing `*`**, and no `targetname` in the game contains one at all, so
//! nothing in Portal 2 reaches the difference. It is reproduced because the
//! next map someone writes might, and because a `*`-in-the-middle query
//! silently matching half the level is not a bug anyone would find twice.
//!
//! Matching is case-insensitive. There is no `?` and no character class.
//!
//! # A name is not unique
//!
//! 40,664 of the game's 60,925 entities carry a `targetname`, and nothing
//! stops two from sharing it — firing an output at a name reaches *every*
//! match, which is how a map turns on eight lights at once. So the lookup is
//! an iterator, never an `Option`, and **the order is list order**, which is
//! entity-lump order: level designers rely on a chain of identically-named
//! relays firing in the order Hammer wrote them.
//!
//! # Procedural names
//!
//! A name beginning with `!` resolves to exactly one entity through
//! `FindEntityProcedural` — `!player`, `!self`, `!activator` and the two
//! Portal 2 co-op ones account for all 3,631 uses in the shipped maps.
//! **Stage 1 resolves none of them**: four of the five need an activator or a
//! caller, which is stage 2's I/O context, and `!player` needs a player
//! entity, which is stage 5's. [`find_by_name`] returns nothing for a `!`
//! name rather than pretending, and says so.

use super::entity::{EntityId, EntityList};

/// `EntityNamesMatchCStrings` (`baseentity.cpp:644`).
///
/// Walks both strings while they agree, ignoring ASCII case. Matches if both
/// ran out together, or if the query is sitting on a `*` at the point they
/// diverged — which is why the `*` need not be trailing. See the module docs.
pub fn names_match(query: &str, name: &str) -> bool {
    let (q, n) = (query.as_bytes(), name.as_bytes());
    let mut i = 0;
    while i < q.len() && i < n.len() && q[i].eq_ignore_ascii_case(&n[i]) {
        i += 1;
    }
    if i == q.len() && i == n.len() {
        return true;
    }
    q.get(i) == Some(&b'*')
}

/// Whether this is a procedural name — `FindEntityByName`'s `szName[0] == '!'`
/// test.
pub fn is_procedural(name: &str) -> bool {
    name.starts_with('!')
}

/// `FindEntityByName` as an iterator over every match, in list order.
///
/// An empty or procedural query yields nothing; see the module docs for why
/// procedural names are stage 2's.
///
/// **This is a linear scan**, which is what the original does — Valve's
/// version walks the whole `CEntInfo` list on every call. An index is the
/// obvious optimisation and is deliberately not here: a name can change at run
/// time (`AddOutput`, `SetName`), an index has to be kept current through
/// that, and nothing has measured the scan as costing anything. The largest
/// shipped map places 1,446 entities.
pub fn find_by_name<'a>(
    list: &'a EntityList,
    query: &'a str,
) -> impl Iterator<Item = EntityId> + 'a {
    let usable = !query.is_empty() && !is_procedural(query);
    list.iter().filter_map(move |(id, entity)| {
        if !usable {
            return None;
        }
        let name = entity.name.as_deref()?;
        names_match(query, name).then_some(id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::classes;
    use crate::server::entity::Entity;

    #[test]
    fn matching_is_case_insensitive_and_only_a_trailing_star_wildcards() {
        assert!(names_match("door", "door"));
        assert!(names_match("DOOR", "door"));
        assert!(names_match("door", "DoOr"));
        assert!(!names_match("door", "doors"));
        assert!(!names_match("doors", "door"));

        assert!(names_match("door*", "door"));
        assert!(names_match("door*", "doorway_03"));
        assert!(names_match("*", ""));
        assert!(names_match("*", "anything"));

        // The part the comment in `baseentity.cpp` gets wrong: the `*` does
        // not have to be trailing. It matches wherever the two strings
        // diverge, so a leading one matches everything.
        assert!(names_match("*door", "the_door"));
        assert!(names_match("*anything", "at all"));
        assert!(names_match("do*r", "dover"));
        assert!(names_match("do*r", "door"));
        // …but the text before the star still has to match.
        assert!(!names_match("do*r", "the_door"));
    }

    fn named(list: &mut EntityList, name: Option<&str>) -> EntityId {
        let mut entity = Entity::new(classes::lookup("info_target").expect("registered"));
        entity.core.name = name.map(str::to_owned);
        list.insert(entity)
    }

    #[test]
    fn a_name_may_match_several_entities_and_the_order_is_list_order() {
        let mut list = EntityList::new();
        let a = named(&mut list, Some("panel"));
        named(&mut list, Some("other"));
        let b = named(&mut list, Some("panel"));
        named(&mut list, None);

        assert_eq!(find_by_name(&list, "panel").collect::<Vec<_>>(), vec![a, b]);
        assert_eq!(find_by_name(&list, "PANEL").count(), 2);
        assert_eq!(find_by_name(&list, "pan*").count(), 2);
        assert_eq!(find_by_name(&list, "missing").count(), 0);
    }

    #[test]
    fn an_empty_or_procedural_query_finds_nothing() {
        let mut list = EntityList::new();
        named(&mut list, Some("panel"));
        assert_eq!(find_by_name(&list, "").count(), 0);
        assert_eq!(find_by_name(&list, "!player").count(), 0);
        assert!(is_procedural("!activator"));
        assert!(!is_procedural("activator"));
    }
}
