//! `SQTable` — Squirrel's hash table, which is Lua 4.0's (`sqtable.cpp`).
//!
//! **Replicated rather than replaced with a `HashMap`**, because its node
//! layout *is* a language semantic: `foreach` over a table walks the node array
//! front to back (`SQTable::Next`), so the order a script sees its keys in is
//! whatever this particular chained-scatter table did with them. A map that
//! prints its keys, or a `foreach` that stops at the first match, behaves
//! differently under any other hash.
//!
//! What the port cannot reproduce is the order of keys that hash by
//! **address** — tables, closures, instances used as keys — because
//! `hashptr` is the object's address shifted right by three, and an address is
//! not something two runs of either program agree on. Strings, integers,
//! floats and bools hash by value and come out in Valve's order.

use super::value::{raw_equal, Value};

/// `MINPOWER2` (`sqobject.h:55`) — the smallest node array a table has.
const MIN_POWER2: usize = 4;

#[derive(Clone)]
struct Node {
    /// `Value::Null` is an empty node: `NewSlot` refuses a null key, so the
    /// two can never be confused.
    key: Value,
    val: Value,
    next: Option<usize>,
}

impl Node {
    fn empty() -> Node {
        Node {
            key: Value::Null,
            val: Value::Null,
            next: None,
        }
    }
}

/// `SQTable`, minus the delegate, which lives on the owning object so that a
/// class's member table (which has none) can use this type too.
pub struct Table {
    nodes: Vec<Node>,
    /// `_firstfree` as an index. It points *at* a node, and the insert path
    /// walks it downwards until it finds one that is free.
    first_free: usize,
    used: usize,
}

/// `HashObj` (`sqtable.h:19`).
fn hash_of(key: &Value) -> u32 {
    match key {
        Value::String(s) => s.hash(),
        // `(SQHash)((SQInteger)_float(key))` — the *truncated* value, so 1.5
        // and 1.25 share a chain. Equality is still by bits.
        Value::Float(f) => super::value::float_to_int(*f) as u32,
        Value::Integer(i) => *i as u32,
        Value::Bool(b) => *b as u32,
        other => (other.address() >> 3) as u32,
    }
}

impl Table {
    /// `SQTable::Create( ss, nInitialSize )`. The size is a *hint* rounded up
    /// to a power of two no smaller than four, and it matters: a table
    /// literal is created with its key count, which fixes where its keys land
    /// and therefore the order `foreach` visits them in.
    pub fn new(initial_size: usize) -> Table {
        let mut size = MIN_POWER2;
        while initial_size > size {
            size <<= 1;
        }
        let mut table = Table {
            nodes: Vec::new(),
            first_free: 0,
            used: 0,
        };
        table.alloc_nodes(size);
        table
    }

    fn alloc_nodes(&mut self, size: usize) {
        self.nodes = vec![Node::empty(); size];
        self.first_free = size - 1;
    }

    fn main_position(&self, key: &Value) -> usize {
        (hash_of(key) as usize) & (self.nodes.len() - 1)
    }

    /// `SQTable::_Get`.
    fn find(&self, key: &Value) -> Option<usize> {
        let mut n = Some(self.main_position(key));
        while let Some(i) = n {
            let node = &self.nodes[i];
            if raw_equal(&node.key, key) {
                return Some(i);
            }
            n = node.next;
        }
        None
    }

    /// `SQTable::CountUsed`.
    pub fn len(&self) -> usize {
        self.used
    }

    /// `SQTable::Get`. A weak reference stored as a value reads back as its
    /// referent — `_realval`.
    pub fn get(&self, key: &Value) -> Option<Value> {
        if key.is_null() {
            return None;
        }
        self.find(key).map(|i| self.nodes[i].val.real())
    }

    /// Whether `key` is present, without dereferencing anything.
    pub fn contains(&self, key: &Value) -> bool {
        !key.is_null() && self.find(key).is_some()
    }

    /// `SQTable::Set` — replaces an existing slot, and only that.
    pub fn set(&mut self, key: &Value, val: Value) -> bool {
        if key.is_null() {
            return false;
        }
        match self.find(key) {
            Some(i) => {
                self.nodes[i].val = val;
                true
            }
            None => false,
        }
    }

    /// `SQTable::NewSlot` — returns whether a new slot was created.
    ///
    /// Lua 4.0's `luaH_set` insert, including its quirks: when a key's main
    /// position is held by a node that does not belong there, that node is
    /// moved to the free position and the new key takes its place.
    pub fn new_slot(&mut self, key: Value, val: Value) -> bool {
        debug_assert!(!key.is_null());
        let h = self.main_position(&key);
        if let Some(i) = self.find(&key) {
            self.nodes[i].val = val;
            return false;
        }

        let mut mp = h;
        if !self.nodes[mp].key.is_null() {
            let n = self.first_free;
            let other = self.main_position(&self.nodes[mp].key);
            if mp > n && other != mp {
                // The colliding node is not in its main position: move it to
                // the free slot, and re-thread its chain through `n`.
                let mut prev = other;
                while self.nodes[prev].next != Some(mp) {
                    prev = self.nodes[prev]
                        .next
                        .expect("sqtable: a node out of place is on a chain");
                }
                self.nodes[prev].next = Some(n);
                self.nodes[n] = self.nodes[mp].clone();
                self.nodes[mp] = Node::empty();
            } else {
                // The new key goes into the free position, chained after mp.
                self.nodes[n].next = self.nodes[mp].next;
                self.nodes[mp].next = Some(n);
                mp = n;
            }
        }
        self.nodes[mp].key = key.clone();

        loop {
            let free = &self.nodes[self.first_free];
            if free.key.is_null() && free.next.is_none() {
                self.nodes[mp].val = val;
                self.used += 1;
                return true;
            } else if self.first_free == 0 {
                break;
            } else {
                self.first_free -= 1;
            }
        }
        self.rehash(true);
        self.new_slot(key, val)
    }

    /// `SQTable::Remove` — clears the node **in place** and leaves its chain
    /// pointer, exactly as Lua 4.0 does; `rehash` is what eventually tidies
    /// it.
    pub fn remove(&mut self, key: &Value) {
        if let Some(i) = self.find(key) {
            self.nodes[i].key = Value::Null;
            self.nodes[i].val = Value::Null;
            self.used -= 1;
            self.rehash(false);
        }
    }

    /// `SQTable::Rehash`.
    fn rehash(&mut self, force: bool) {
        let real_old_size = self.nodes.len();
        let old_size = real_old_size.max(4);
        let used = self.used;
        let new_size = if used >= old_size - old_size / 4 {
            old_size * 2
        } else if used <= old_size / 4 && old_size > MIN_POWER2 {
            old_size / 2
        } else if force {
            old_size
        } else {
            return;
        };
        let old = std::mem::take(&mut self.nodes);
        self.alloc_nodes(new_size);
        self.used = 0;
        for node in old {
            if !node.key.is_null() {
                self.new_slot(node.key, node.val);
            }
        }
    }

    /// `SQTable::Next` — the next occupied node at or after `position`, and the
    /// position to resume from. Values come back raw (not dereferenced) only
    /// when `weak_refs` is set, which is `Clone`'s use.
    pub fn next(&self, position: usize, weak_refs: bool) -> Option<(usize, Value, Value)> {
        let mut i = position;
        while i < self.nodes.len() {
            let node = &self.nodes[i];
            if !node.key.is_null() {
                let val = match weak_refs {
                    true => node.val.clone(),
                    false => node.val.real(),
                };
                return Some((i + 1, node.key.clone(), val));
            }
            i += 1;
        }
        None
    }

    /// Every key and value, in `foreach` order — a snapshot, for a test; a
    /// script's `foreach` resumes by position instead, so that it sees what
    /// the loop body did to the table.
    #[cfg(test)]
    pub fn entries(&self) -> Vec<(Value, Value)> {
        let mut out = Vec::with_capacity(self.used);
        let mut position = 0;
        while let Some((next, key, val)) = self.next(position, false) {
            out.push((key, val));
            position = next;
        }
        out
    }

    /// `SQTable::Clone` — a fresh table the same size, filled in `Next` order.
    pub fn clone_table(&self) -> Table {
        let mut table = Table::new(self.nodes.len());
        let mut position = 0;
        while let Some((next, key, val)) = self.next(position, true) {
            table.new_slot(key, val);
            position = next;
        }
        table
    }

    /// `SQTable::Clear`.
    pub fn clear(&mut self) {
        for node in &mut self.nodes {
            node.key = Value::Null;
            node.val = Value::Null;
        }
        self.used = 0;
        self.rehash(true);
    }

    /// Drops every value, for [`Vm`](super::Vm)'s teardown — `Finalize`, which
    /// is how `sq_close` breaks the cycles reference counting cannot.
    pub(super) fn finalize(&mut self) {
        for node in &mut self.nodes {
            node.key = Value::Null;
            node.val = Value::Null;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vscript::value::SqStr;

    fn s(text: &str) -> Value {
        Value::String(SqStr::new(text.as_bytes()))
    }

    #[test]
    fn a_table_keeps_what_it_is_given_and_forgets_what_is_removed() {
        let mut t = Table::new(0);
        for i in 0..100 {
            // Not asserting the return value: when an insert fills the table,
            // `NewSlot` rehashes with the key already placed and then finds
            // it, so it reports "not new" — in the C as well.
            t.new_slot(Value::Integer(i), Value::Integer(i * 2));
        }
        assert_eq!(t.len(), 100);
        for i in 0..100 {
            assert!(matches!(t.get(&Value::Integer(i)), Some(Value::Integer(v)) if v == i * 2));
        }
        for i in (0..100).step_by(2) {
            t.remove(&Value::Integer(i));
        }
        assert_eq!(t.len(), 50);
        assert!(t.get(&Value::Integer(4)).is_none());
        assert!(matches!(t.get(&Value::Integer(5)), Some(Value::Integer(10))));
        assert_eq!(t.entries().len(), 50);
    }

    #[test]
    fn new_slot_on_an_existing_key_replaces_rather_than_adds() {
        let mut t = Table::new(0);
        assert!(t.new_slot(s("a"), Value::Integer(1)));
        assert!(!t.new_slot(s("a"), Value::Integer(2)));
        assert_eq!(t.len(), 1);
        assert!(matches!(t.get(&s("a")), Some(Value::Integer(2))));
        assert!(!t.set(&s("b"), Value::Integer(3)));
    }

    #[test]
    fn a_float_key_is_found_by_its_bits_not_its_truncation() {
        let mut t = Table::new(0);
        t.new_slot(Value::Float(1.5), Value::Integer(1));
        assert!(t.get(&Value::Float(1.25)).is_none());
        assert!(t.get(&Value::Integer(1)).is_none());
        assert!(t.get(&Value::Float(1.5)).is_some());
    }

    /// The order is the hash layout's, not insertion order. These are the
    /// keys `CHAPTER_TITLES`' rows are built from, in a nine-key literal.
    #[test]
    fn iteration_order_is_the_node_array_not_insertion() {
        let mut t = Table::new(5);
        for key in ["map", "title_text", "subtitle_text", "displayOnSpawn", "displaydelay"] {
            t.new_slot(s(key), Value::Null);
        }
        let order: Vec<String> = t
            .entries()
            .into_iter()
            .map(|(k, _)| k.to_display_string())
            .collect();
        assert_eq!(order.len(), 5);
        let mut sorted = order.clone();
        sorted.sort();
        let mut expected = vec!["map", "title_text", "subtitle_text", "displayOnSpawn", "displaydelay"];
        expected.sort();
        assert_eq!(sorted, expected);
    }
}
