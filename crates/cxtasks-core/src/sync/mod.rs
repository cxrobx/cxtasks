//! Making one database the same as another, boringly.
//!
//! The shape is deliberately dull, because three consistency stories in one
//! system is how a task app becomes a bespoke CRDT project. There is exactly
//! one: **whole rows, last writer wins, intents replayed in order.** No
//! field-level merge anywhere — it manufactures rows `tasks::update` would
//! refuse to write (a tripwire spec from one side with `tripwire_state=NULL`
//! from the other) and nothing catches them, because they were not written by
//! `update`.

pub mod apply;
pub mod auto;
pub mod blobs;
pub mod http;
pub mod leases;
pub mod outbox;
pub mod pair;
pub mod reconcile;
pub mod snapshot;
pub mod state;
pub mod sweep;
pub mod tables;
