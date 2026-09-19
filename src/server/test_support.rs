//! A [`Context`] without a [`Server`](super::Server), for tests that want one
//! entity rather than a level.
//!
//! Most of this module's behaviour is reachable through `Server::level_init`
//! and `Server::frame`, and the tests that can go through those do. This is
//! for the two that cannot: the declaration-versus-implementation invariant in
//! `classes`, which has to offer every class every input without a map, and
//! the class unit tests, which want to watch one entity in isolation.

use super::attachment::{self, Attachments};
use super::class::{Behaviour, ClassDef, Context, SpawnResult};
use super::entity::{Entity, EntityCore, EntityId, EntityList};
use super::io::{EventQueue, FieldType, Input, Variant};
use super::random::RandomStream;
use super::sequences::SequenceTable;
use super::think::{ServerClock, DEFAULT_TICK_INTERVAL};

/// Everything a [`Context`] borrows, owned.
///
/// The list is always empty here — a class under test is held by the caller
/// rather than inserted, which is exactly the state
/// [`EntityList::detach`](super::entity::EntityList::detach) leaves the real
/// server in during a dispatch. A test that needs a class to *reach* another
/// entity (a filter, a teleport destination) goes through `Server` and a real
/// map instead; see `tests`' stage-4 group.
pub(super) struct Harness {
    pub queue: EventQueue,
    pub random: RandomStream,
    pub clock: ServerClock,
    pub entities: EntityList,
    pub player: Option<EntityId>,
    /// What `studio/` would have said about this level's models.
    ///
    /// **Empty by default**, which is the state every `Spawn` in the real game
    /// runs in too — see [`sequences`](super::sequences). A test that wants a
    /// prop's animation to *finish* fills it in first.
    pub sequences: SequenceTable,
    /// What `studio/` would have said about their **attachment points**.
    ///
    /// [`NoAttachments`](super::attachment::NoAttachments) by default, so a
    /// `SetParentAttachment` is refused exactly as it is against a level whose
    /// models have not loaded. A test that wants one to land swaps this.
    pub attachments: Box<dyn Attachments>,
}

impl Harness {
    pub fn new() -> Harness {
        Harness {
            queue: EventQueue::new(),
            random: RandomStream::new(0),
            clock: ServerClock::new(DEFAULT_TICK_INTERVAL),
            entities: EntityList::new(),
            player: None,
            sequences: SequenceTable::new(),
            attachments: Box::new(attachment::NoAttachments),
        }
    }

    /// A [`Context`] over this harness. Every method below builds one the same
    /// way; it is separate so that a test can drive a behaviour directly.
    pub fn context(&mut self) -> Context<'_> {
        Context::new(
            self.clock.time(),
            &mut self.queue,
            &mut self.random,
            &mut self.entities,
            self.player,
            &self.sequences,
            self.attachments.as_ref(),
        )
    }

    /// Runs one entity's `Spawn`.
    pub fn spawn(&mut self, entity: &mut Entity) -> SpawnResult {
        let Entity { core, behaviour } = entity;
        let mut cx = Context::new(
            self.clock.time(),
            &mut self.queue,
            &mut self.random,
            &mut self.entities,
            self.player,
            &self.sequences,
            self.attachments.as_ref(),
        );
        behaviour.spawn(core, &mut cx)
    }

    /// One server tick against one entity: advance the clock, then
    /// `Physics_SimulateEntity`.
    ///
    /// No [`ThinkList`](super::think::ThinkList), so the caller is standing in
    /// for the simulation list — which is what makes this useful for a mover:
    /// `movement::simulate` re-checks the think tick itself, so an entity
    /// handed to it out of turn does nothing rather than thinking early.
    pub fn tick(&mut self, core: &mut EntityCore, behaviour: &mut dyn Behaviour) {
        self.clock.advance();
        let mut cx = Context::new(
            self.clock.time(),
            &mut self.queue,
            &mut self.random,
            &mut self.entities,
            self.player,
            &self.sequences,
            self.attachments.as_ref(),
        );
        // No collision either, so nothing a mover pushes against can block it
        // — see [`TouchQuery::push_trace`](super::TouchQuery::push_trace)'s
        // default. A test that wants a door to be blocked hands a real query
        // to a real [`Server`](super::Server).
        super::movement::simulate(core, behaviour, &mut cx, &mut super::NoTouchQuery);
    }

    /// Whether `class`'s handler takes `name`, given a value of the type it
    /// declared. The invariant test in `classes` is the only caller.
    pub fn offer_input(&mut self, class: &'static ClassDef, name: &str, field: FieldType) -> bool {
        let mut entity = Entity::new(class);
        // A value of the declared type, so that a handler reading it gets
        // what it expects rather than a zero from the wrong union arm.
        let value = match field {
            FieldType::Void => Variant::Void,
            FieldType::Bool => Variant::Bool(true),
            FieldType::Int => Variant::Int(1),
            FieldType::Float => Variant::Float(1.0),
            FieldType::String | FieldType::Input => Variant::String(String::from("1")),
            FieldType::Vector => Variant::Vector(glam::Vec3::ONE),
            FieldType::Color32 => Variant::Color32([1, 2, 3, 4]),
        };
        let Entity { core, behaviour } = &mut entity;
        let mut cx = Context::new(
            self.clock.time(),
            &mut self.queue,
            &mut self.random,
            &mut self.entities,
            self.player,
            &self.sequences,
            self.attachments.as_ref(),
        );
        let input = Input {
            name,
            value,
            activator: None,
            caller: None,
            output_id: 0,
        };
        behaviour.accept_input(core, &input, &mut cx)
    }

    /// The inputs this harness's queue is holding, as
    /// `(target, input, parameter, fire time)`, in fire order.
    pub fn queued(&self) -> Vec<(String, String, String, f32)> {
        self.queue
            .iter()
            .map(|event| {
                let target = match &event.target {
                    super::io::Target::Name(name) => name.clone(),
                    super::io::Target::Entity(id) => format!("#{}", id.slot()),
                };
                (
                    target,
                    event.input.clone(),
                    event.value.to_string(),
                    event.fire_time,
                )
            })
            .collect()
    }
}

/// Lets a test build a bare entity and give it a handle in one line.
pub(super) fn entity(class: &'static ClassDef) -> Entity {
    Entity::new(class)
}
