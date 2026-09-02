# Accensa Contracts Event Reference

This document describes the events emitted by the `accensa-contracts`. These events form a **public interface** for indexers.

## Stability Policy
As documented in [`CONTRIBUTING.md`](../CONTRIBUTING.md), event topics and field names are guaranteed to be stable. Any modification to an event's shape or topic tuple is considered a breaking change.

When consuming these events, indexers should:
- Filter by the exact topic tuple documented below.
- Tolerate any unknown additional fields that may be appended in future non-breaking updates.

---

## `ReceiptAnchor` Events

`ReceiptAnchor` partitions receipts into logical shards. Every batch-scoped
event therefore carries a leading `shard_id` topic: each `shard_id` owns an
independent batch stream, so `batch_id` alone does not identify a batch.

> **Breaking change (unreleased):** `AnchorEvent` and `PruneEvent` gained a
> leading `shard_id` topic. An indexer filtering on the previous two-element
> tuples matches nothing. Filter on `("anchor_event", shard_id, batch_id)` and
> read `shard_id` from the topics, not the data map.

### 1. `AnchorEvent`
Emitted when a new batch of receipts is anchored by the merchant.

- **Topics**: `("anchor_event", shard_id: u64, batch_id: u64)`
- **Data Map**:
  - `root` (`BytesN<32>`): The Merkle root of the batch.
  - `count` (`u32`): Number of receipts in the batch.
  - `period_start` (`u64`): Start time of the batch period.
  - `period_end` (`u64`): End time of the batch period.
  - `anchored_ledger` (`u32`): The ledger sequence when the batch was anchored.

*Note: The data map is structurally identical to the `BatchRecord` returned by `get_batch(shard_id, batch_id)`.*

### 2. `PruneEvent`
Emitted when old batches are pruned from a shard's stream to reclaim rent. Not
emitted when a call deletes nothing (the cursor did not advance).

- **Topics**: `("prune_event", shard_id: u64, start_batch_id: u64)`
- **Data Map**:
  - `end_batch_id` (`u64`): The upper bound (inclusive) of the pruned range.

The deleted batch ids are exactly `[start_batch_id, end_batch_id]` — inclusive
on both ends — so a reader can drop the whole closed range from its index.

### 3. `ShardCreatedEvent`
Emitted when the router factory-deploys a new storage shard to hold a fresh
capacity range within a logical shard's batch stream. One is emitted per
`SHARD_CAPACITY` (200) batches per `shard_id`, on the anchor that first lands in
the new range.

- **Topics**: `("shard_created_event", shard_id: u64, shard_index: u64)`
- **Data Map**:
  - `shard_address` (`Address`): The deployed `ReceiptShard` contract.
  - `start_batch_id` (`u64`): First batch id this storage shard holds (inclusive).
  - `end_batch_id` (`u64`): Upper bound of the range (exclusive).

*Note: `shard_index` is the capacity index within `shard_id`'s stream, not a
logical shard id. `(shard_id, shard_index)` together identify a storage shard.*

### 4. `InitializedEvent`
Emitted once by `initialize(merchant, shard_wasm_hash)`, after the admin, shard
wasm hash and default configuration have been written.

- **Topics**: `("initialized_event", merchant: Address)`
- **Data Map**:
  - `shard_wasm_hash` (`BytesN<32>`): The `ReceiptShard` wasm the anchor deploys storage shards from.
  - `ledger` (`u32`): The ledger sequence of the initialization.

### 5. `RateLimitUpdatedEvent`
Emitted when the merchant reconfigures the `anchor_batch` token-bucket rate
limit via `set_anchor_rate_limit(burst_capacity, refill_interval_secs)`.

- **Topics**: `("rate_limit_updated_event", previous_burst_capacity: u32, previous_refill_interval_secs: u32)`
- **Data Map**:
  - `new_burst_capacity` (`u32`): The burst capacity in force after the change.
  - `new_refill_interval_secs` (`u32`): The refill interval in force after the change.
  - `ledger` (`u32`): The ledger sequence of the change.

The topics carry the configuration in force *before* the change and the data
map the one in force *after* it, so a reader reconstructing the limiter never
needs to join two events. A `{0, 0}` pair (on either side) means rate limiting
is disabled.

### 6. `AnchorIntervalUpdatedEvent`
Emitted when the merchant reconfigures the minimum interval (in seconds)
between consecutive anchors via `set_min_anchor_interval(interval)`.

- **Topics**: `("anchor_interval_updated_event", previous_interval: u32)`
- **Data Map**:
  - `new_interval` (`u32`): The interval in force after the change; `0` disables the check.
  - `ledger` (`u32`): The ledger sequence of the change.

---

## `RefundVault` Events

### 7. `DepositEvent`
Emitted when the merchant tops up the vault's float.

- **Topics**: `("deposit_event", from: Address)`
- **Data Map**:
  - `amount` (`i128`): The amount deposited (in the token's smallest unit).

### 8. `RefundEvent`
Emitted when a payment is refunded to an agent. A `claim_batch` or
`process_batch` call emits one `RefundEvent` per applied claim, in claim order
(the same event as a single `refund`). A claim that fails emits no event — in
`claim_batch` the whole call reverts; in `process_batch` the claim is simply
not applied and reported as `false` in the returned `Vec<bool>`.

- **Topics**: `("refund_event", payment_ref: BytesN<32>)`
- **Data Map**:
  - `amount` (`i128`): The amount refunded in this call, before the fee is deducted (in the token's smallest unit).
  - `fee` (`i128`): The fee deducted from `amount` and paid to the fee recipient in this call. `0` when no fee is configured.
  - `cumulative_refunded` (`i128`): The running total across all refunds for this `payment_ref` (pre-fee), so an indexer knows the state without summing history.
  - `recipient` (`Address`): The address that received the payout.
  - `ledger` (`u32`): The ledger sequence of the refund.
- **Fee accounting:** `amount == payout + fee` exactly; the total outflow per claim equals `amount`, so fees never expand the `payment_amount` ceiling or the float check. When a fee is charged and no recipient is configured, the fee defaults to the merchant.

*Note: `fee` and `cumulative_refunded` are appended fields; per the stability policy, indexers must tolerate them rather than expect the historical `(amount, recipient, ledger)` shape.*

### 9. `BatchRefundEvent`
Emitted once per `process_batch` call instead of one `RefundEvent` per item.
Keeping the batch to a single compact event is what lets 50+ refunds fit inside
a transaction's 16 KiB contract-event budget (a per-refund event would cap
batches at ~30).

- **Topics**: `("batch_refund_event",)`
- **Data Map**:
  - `payment_refs` (`Vec<BytesN<32>>`): The payment refs, in submission order.
  - `results` (`Vec<bool>`): Per-item outcome, aligned 1:1 with `payment_refs`
    (`true` = refund executed; `false` = item failed validation and was skipped).

*Per-item outcomes are not persisted in the event; call `get_refund(payment_ref)`
to inspect a refund record.*

### 10. `WithdrawEvent`
Emitted when the merchant withdraws funds from the float.

- **Topics**: `("withdraw_event", to: Address)`
- **Data Map**:
  - `amount` (`i128`): The amount withdrawn (in the token's smallest unit).

### 11. `PauseEvent`
Emitted when the merchant pauses the vault, halting deposits, refunds and withdrawals.

- **Topics**: `("pause_event", ledger: u32)`
- **Data Map**: *(empty)*

### 12. `UnpauseEvent`
Emitted when the merchant unpauses the vault.

- **Topics**: `("unpause_event", ledger: u32)`
- **Data Map**: *(empty)*

The `ledger` topic lets an indexer reconstruct pause windows from the event log alone: a vault is paused between a `pause_event` and the next `unpause_event`.

### 13. `RefundWindowUpdatedEvent`
Emitted when the merchant changes the refund window.

- **Topics**: `("refund_window_updated_event", previous_window: u32, new_window: u32)`
- **Data Map**: *(empty)*

Both values are carried so a reader can tell whether a refund rejected at a given ledger was rejected under the old rule or the new one.

### 14. `OraclePolicySetEvent`
Emitted when the merchant installs (or replaces) the dynamic oracle policy
that gates refunds.

- **Topics**: `("oracle_policy_set_event", feed_id: BytesN<32>)`
- **Data Map**:
  - `threshold` (`i128`): The median value (in the feed's scale) at which the condition flips.
  - `refund_when_below` (`bool`): `true` = refunds allowed while the median is strictly below the threshold; `false` = allowed while strictly above.
  - `max_staleness_ledgers` (`u32`): Maximum allowed age of a feed value; `0` = never stale.

The data map carries the full condition, so an indexer can reconstruct the
policy in force from the event log alone.

### 15. `OraclePolicyClearedEvent`
Emitted when the merchant removes the dynamic oracle policy, restoring purely
time-window-based refunds.

- **Topics**: `("oracle_policy_cleared_event", feed_id: BytesN<32>)`
- **Data Map**: *(empty)*

The `feed_id` is the feed of the policy that was in force, captured before it
was removed, so a reader can correlate the clear with the preceding set event.
