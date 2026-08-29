# Accensa Contracts Event Reference

This document describes the events emitted by the `accensa-contracts`. These events form a **public interface** for indexers.

## Stability Policy
As documented in [`CONTRIBUTING.md`](../CONTRIBUTING.md), event topics and field names are guaranteed to be stable. Any modification to an event's shape or topic tuple is considered a breaking change.

When consuming these events, indexers should:
- Filter by the exact topic tuple documented below.
- Tolerate any unknown additional fields that may be appended in future non-breaking updates.

---

## `ReceiptAnchor` Events

### 1. `AnchorEvent`
Emitted when a new batch of receipts is anchored by the merchant.

- **Topics**: `("anchor_event", batch_id: u64)`
- **Data Map**:
  - `root` (`BytesN<32>`): The Merkle root of the batch.
  - `count` (`u32`): Number of receipts in the batch.
  - `period_start` (`u64`): Start time of the batch period.
  - `period_end` (`u64`): End time of the batch period.
  - `anchored_ledger` (`u32`): The ledger sequence when the batch was anchored.

*Note: The data map is structurally identical to the `BatchRecord` returned by `get_batch`.*

### 2. `PruneEvent`
Emitted when old batches are pruned to reclaim rent.

- **Topics**: `("prune_event", start_batch_id: u64)`
- **Data Map**:
  - `end_batch_id` (`u64`): The upper bound (inclusive) of the pruned range.

---

## `RefundVault` Events

### 3. `DepositEvent`
Emitted when the merchant tops up the vault's float.

- **Topics**: `("deposit_event", from: Address)`
- **Data Map**:
  - `amount` (`i128`): The amount deposited (in the token's smallest unit).

### 4. `RefundEvent`
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

### 5. `WithdrawEvent`
Emitted when the merchant withdraws funds from the float.

- **Topics**: `("withdraw_event", to: Address)`
- **Data Map**:
  - `amount` (`i128`): The amount withdrawn (in the token's smallest unit).

### 6. `PauseEvent`
Emitted when the merchant pauses the vault, halting deposits, refunds and withdrawals.

- **Topics**: `("pause_event", ledger: u32)`
- **Data Map**: *(empty)*

### 7. `UnpauseEvent`
Emitted when the merchant unpauses the vault.

- **Topics**: `("unpause_event", ledger: u32)`
- **Data Map**: *(empty)*

The `ledger` topic lets an indexer reconstruct pause windows from the event log alone: a vault is paused between a `pause_event` and the next `unpause_event`.

### 8. `RefundWindowUpdatedEvent`
Emitted when the merchant changes the refund window.

- **Topics**: `("refund_window_updated_event", previous_window: u32, new_window: u32)`
- **Data Map**: *(empty)*

Both values are carried so a reader can tell whether a refund rejected at a given ledger was rejected under the old rule or the new one.

### 9. `PolicyProposedEvent`
Emitted when the merchant proposes a new refund policy (window and deadline). The change is not applied until the matching `PolicyExecutedEvent`.

- **Topics**: `("policy_proposed_event", window: u32)`
- **Data Map**:
  - `deadline` (`u64`): The wall-clock deadline (Unix timestamp) after which refund claims are rejected; `0` disables the deadline.
  - `proposed_at_ledger` (`u32`): The ledger sequence when the proposal was made.
  - `execute_after_ledger` (`u32`): The earliest ledger at which `execute_policy` may succeed (proposal + timelock).

### 10. `PolicyExecutedEvent`
Emitted when the merchant executes a pending policy change after the timelock.

- **Topics**: `("policy_executed_event", window: u32)`
- **Data Map**:
  - `deadline` (`u64`): The wall-clock deadline (Unix timestamp) now in force; `0` means no deadline.

### 11. `FeeConfigUpdatedEvent`
Emitted when the merchant changes the refund fee configuration (the basis-point rate or the recipient address). Each emission carries the **full effective configuration** — including the current value of the other field — keyed by the field that changed.

- **Topics**: `("fee_config_updated_event", field: Symbol)` where `field` is `"fee_bps"` (rate changed) or `"fee_recipient"` (recipient changed).
- **Data Map**:
  - `fee_bps` (`u32`): The fee rate in basis points in force after the change; `0` means no fee.
  - `fee_recipient` (`Address`): The effective fee recipient, resolved via the merchant fallback when none is configured.
