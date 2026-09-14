//! A [`Context`] without a [`Server`](super::Server), for tests that want one
//! entity rather than a level.
//!
//! Most of this module's behaviour is reachable through `Server::level_init`
//! and `Server::frame`, and the tests that can go through those do. This is
//! for the two that cannot: the declaration-versus-implementation invariant in
//! `classes`, which has to offer every class every input without a map, and
//! the class unit tests, which want to watch one entity in isolation.

use super::class::{Behaviour, ClassDef, Context, SpawnResult};
use super::entity::{Entity, EntityCore};
use super::io::{EventQueue, FieldType, Input, Variant};
use super::random::RandomStream;
use super::think::{ServerClock, DEFAULT_TICK_INTERVAL};

/// Everything a [`Context`] borrows, owned.
pub(super) struct Harness {
    pub queue: EventQueue,
    pub random: RandomStream,
    pub clock: ServerClock,
}

impl Harness {
    pub fn new() -> Harness {
        Harness {
            queue: EventQueue::new(),
            random: RandomStream::new(0),
            clock: ServerClock::new(DEFAULT_TICK_INTERVAL),
        }
    }

    /// Runs one entity's `Spawn`.
    pub fn spawn(&mut self, entity: &mut Entity) -> SpawnResult {
        let Entity { core, behaviour } = entity;
        let mut cx = Context::new(self.clock.time(), &mut self.queue, &mut self.random);
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
        let mut cx = Context::new(self.clock.time(), &mut self.queue, &mut self.random);
        super::movement::simulate(core, behaviour, &mut cx);
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
        let mut cx = Context::new(self.clock.time(), &mut self.queue, &mut self.random);
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
