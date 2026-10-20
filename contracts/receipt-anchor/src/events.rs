//! Canonical, indexer-facing event schema for `ReceiptAnchor` (issue #379).
//!
//! Every receipt-scoped event is published under one topic tuple —
//! `(receipt, action, anchor_id)` — so an indexer subscribes to a single topic
//! and routes on the `action` symbol instead of maintaining a per-event topic
//! table (Horizon, Mercury and SubQuery all filter on topic tuples).
//!
//! The data map is a typed [`AnchorPayload`] / [`PrunePayload`] struct encoded
//! with its field names as keys. It always leads with `schema_version` and
//! `timestamp`, so a reader can reject payloads it cannot parse and can order
//! receipt logs by wall-clock time as well as by ledger.
//!
//! Admin and factory events (`initialized_event`, `shard_created_event`,
//! `rate_limit_updated_event`, `anchor_interval_updated_event`, the admin
//! transfer pair) are not receipt logs and keep their own documented shapes in
//! [`docs/EVENTS.md`](../../../docs/EVENTS.md).

use soroban_sdk::{contracttype, BytesN, Env, IntoVal, Symbol, Val};

/// Version of the receipt event payload layout. Bump on any non-additive
/// change to a payload's field set so an indexer can detect a layout it does
/// not understand instead of mis-decoding it.
pub const SCHEMA_VERSION: u32 = 1;

/// The action discriminator carried in the second topic. Each variant names a
/// state transition an indexer must be able to replay from the log alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiptAction {
    /// A batch root was anchored. `anchor_id` is the `batch_id` assigned
    /// within that shard's own stream.
    Anchor,
    /// A contiguous prefix of a shard's batch stream was pruned. `anchor_id`
    /// is the first deleted batch id.
    Prune,
}

impl ReceiptAction {
    /// The symbol this action is published under, e.g. `anchor`.
    pub fn topic(&self, env: &Env) -> Symbol {
        match self {
            Self::Anchor => Symbol::new(env, "anchor"),
            Self::Prune => Symbol::new(env, "prune"),
        }
    }
}

/// Data map of the `anchor` action: the schema envelope (`schema_version`,
/// `timestamp`) plus everything the legacy `AnchorEvent` carried, with
/// `shard_id` moved out of the topics and into the payload.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorPayload {
    /// Version number of the anchor payload schema.
    pub schema_version: u32,
    /// Timestamp recorded for this anchor event.
    pub timestamp: u64,
    /// 32-byte root carried by this anchor payload.
    pub root: BytesN<32>,
    /// Identifier of the shard represented by this anchor.
    pub shard_id: u64,
    /// Count recorded for this anchor.
    pub count: u32,
    /// Start of the period represented by this anchor.
    pub period_start: u64,
    /// End of the period represented by this anchor.
    pub period_end: u64,
    /// Ledger number recorded for this anchor.
    pub anchored_ledger: u32,
}

/// Data map of the `prune` action: the schema envelope plus the closed range
/// of deleted batch ids. Pruning deletes ids rather than naming a root, so
/// this payload carries no `root`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrunePayload {
    /// Payload layout version; `prune_batches` writes it as [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Ledger timestamp of the prune call, captured by `prune_batches` from `env.ledger().timestamp()`.
    pub timestamp: u64,
    /// Logical shard whose stream was pruned — the `shard_id` argument `prune_batches` receives.
    pub shard_id: u64,
    /// First batch id deleted: inclusive lower bound of the pruned range, set by `prune_batches`.
    pub start_batch_id: u64,
    /// Last batch id deleted: inclusive upper bound (`cursor - 1`), set by `prune_batches`.
    pub end_batch_id: u64,
}

/// Publishes the canonical receipt event: topics
/// `(receipt, action, anchor_id)` with `payload` as the data map.
///
/// This is the only way `ReceiptAnchor` writes receipt-scoped events, so the
/// topic tuple cannot drift per call site.
#[allow(deprecated)]
pub fn publish<P: IntoVal<Env, Val>>(env: &Env, action: ReceiptAction, anchor_id: u64, payload: P) {
    env.events().publish(
        (Symbol::new(env, "receipt"), action.topic(env), anchor_id),
        payload,
    );
}
