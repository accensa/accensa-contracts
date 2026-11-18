# Events: Governance, UptoAuthorization and CrossChain

Companion to [`EVENTS.md`](EVENTS.md). It catalogues the events emitted by
`Governance`, `UptoAuthorization` and `CrossChain`, in the same format as the
main reference. All of it is verified against the contract source in
`contracts/governance`, `contracts/upto-authorization` and
`contracts/cross-chain`.

## Stability policy

The [Event Stability Policy](../CONTRIBUTING.md#event-stability-policy) applies
unchanged: topic tuples and field names are a public interface; changing them is
a breaking change. Indexers should filter on the exact topic tuples below and
tolerate unknown additional fields in the data map.

`#[contractevent]` events use the struct name in snake_case as their first topic
(for example `ProposalCreated` -> `"proposal_created"`). Fields marked
`#[topic]` follow in declaration order; the remaining fields form the data map.
Events declared without topics are published with just the leading name topic.

---

## `Governance` Events

### `ProposalCreated`
Emitted when a member creates a proposal (`propose`, or the simulation-gated
creation path).

- **Topics**: `("proposal_created", proposal_id: u64)`
- **Data Map**:
  - `proposer` (`Address`): The authorizing proposer.
  - `target` (`Address`): Contract the proposal calls on execution.
  - `function` (`Symbol`): Function invoked on `target`.
  - `deadline_ledger` (`u32`): Last ledger on which votes count.

### `VoteCast`
Emitted on every address-based vote.

- **Topics**: `("vote_cast", proposal_id: u64)`
- **Data Map**:
  - `voter` (`Address`): The voting member.
  - `support` (`bool`): Whether the vote is in favour.
  - `weight` (`u64`): Quadratic weight applied to the tally.

### `AnonymousVoteCast`
Emitted after a valid LSAG anonymous vote is accepted. Contains no voter address
or key image (see [`EVENTS.md`](EVENTS.md#anonymousvotecast-issue-441)).

- **Topics**: `("anonymous_vote_cast", proposal_id: u64)`
- **Data Map**:
  - `support` (`bool`)
  - `weight` (`u64`)

### `ProposalExecutedEvent`
Emitted once a proposal is executed.

- **Topics**: `("proposal_executed_event", proposal_id: u64)`
- **Data Map**:
  - `target` (`Address`)
  - `function` (`Symbol`)

### `RagequitEvent`
Emitted when a member ragequits from an approved proposal.

- **Topics**: `("ragequit_event", proposal_id: u64, voter: Address)`
- **Data Map**:
  - `deposit_burned` (`u64`): Raw deposit removed from the member.
  - `payout` (`i128`): Pro-rata treasury tokens paid to the member.

### Timelock

`TimelockEnabled` — emitted when the timelock is switched on.
- **Topics**: `("timelock_enabled",)`
- **Data Map**: empty.

`TransactionQueued` — emitted when a call is queued behind the timelock.
- **Topics**: `("transaction_queued", proposal_id: u64)`
- **Data Map**: `call_hash` (`BytesN<32>`), `execution_ledger` (`u32`).

### Optimistic proposals

| Event | Topics | Data Map |
|---|---|---|
| `OptimisticQueuedEvent` | `("optimistic_queued_event", proposal_id: u64)` | `proposer` (`Address`), `target` (`Address`), `function` (`Symbol`), `challenge_deadline_ledger` (`u32`) |
| `OptimisticVetoCastEvent` | `("optimistic_veto_cast_event", proposal_id: u64)` | `voter` (`Address`), `weight` (`u64`), `veto_weight` (`u64`, cumulative) |
| `OptimisticVetoedEvent` | `("optimistic_vetoed_event", proposal_id: u64)` | empty; emitted the ledger the supermajority veto threshold is crossed |
| `OptimisticExecutedEvent` | `("optimistic_executed_event", proposal_id: u64)` | `target` (`Address`), `function` (`Symbol`) |

### Quadratic funding bounties

| Event | Topics | Data Map |
|---|---|---|
| `QfBountyOpened` | `("qf_bounty_opened", bounty_id: u64)` | `recipient` (`Address`) |
| `QfDonationReceived` | `("qf_donation_received", bounty_id: u64, donor: Address)` | `amount` (`i128`), `donor_total` (`i128`) |
| `QfBountyCompleted` | `("qf_bounty_completed", bounty_id: u64)` | `recipient` (`Address`), `total_donated` (`i128`), `match_amount` (`i128`), `payout` (`i128`) |

### Vote-escrow and liquid staking

| Event | Topics | Data Map |
|---|---|---|
| `VeLocked` | `("ve_locked", user: Address)` | `amount` (`i128`), `unlock_ledger` (`u32`) |
| `VeWithdrawn` | `("ve_withdrawn", user: Address)` | `amount` (`i128`) |
| `Mint` | `("mint", caller: Address)` | `amount` (`u64`), `stacc_minted` (`u64`) |
| `Burn` | `("burn", caller: Address)` | `stacc_burned` (`u64`), `underlying_redeemed` (`u64`), `ledger` (`u32`) |

---

## `UptoAuthorization` Events

### `AuthorizeEvent`
Emitted when a buyer authorizes a payment cap.

- **Topics**: `("authorize_event", payment_id: BytesN<32>)`
- **Data Map**:
  - `from` (`Address`): The buyer.
  - `to` (`Address`): Recipient bound by the authorization.
  - `cap` (`i128`): Payment ceiling in the token's smallest unit.
  - `expiry` (`u32`): Ledger after which the authorization cannot settle.
  - `max_slippage_bps` (`u32`): Tolerated slippage above `cap` (`0` = none).

### `SettleEvent`
Emitted when a payment is settled.

- **Topics**: `("settle_event", payment_id: BytesN<32>)`
- **Data Map**:
  - `actual` (`i128`): Amount charged.
  - `from` (`Address`): The buyer.

### `PruneEvent`
Emitted when lapsed authorizations are pruned (`prune_authorization`,
`prune_authorizations`).

- **Topics**: `("prune_event", count: u32)`
- **Data Map**: empty.

### `EscrowCancelledInactivity`
Emitted when a dormant authorization is cancelled and its allowance released.

- **Topics**: `("escrow_cancelled_inactivity", payment_id: BytesN<32>)`
- **Data Map**:
  - `from` (`Address`): The buyer whose allowance was released.
  - `released` (`i128`): The cap that was locked and is now free.
  - `ledger` (`u32`): Ledger at which cancellation executed.

---

## `CrossChain` Events

### Outbound bridge withdrawal (`bridge` / `withdraw`)
Published by `execute_withdrawal_to_evm` after the wrapped asset is burned. This
event is **not** a `#[contractevent]`: it is published directly with constant
topics, and the data is an `OutboundBridgePayload` map. External relayers consume
it to mint or unlock on the destination EVM chain.

- **Topics**: `("bridge", "withdraw", sequence: u64)` (`BRIDGE_TOPIC`,
  `WITHDRAW_ACTION` in `outbound.rs`)
- **Data Map** (`OutboundBridgePayload`):
  - `sequence` (`u64`): Monotonic sequence for ordering and replay protection.
  - `sender` (`Address`): Caller whose wrapped assets were burned.
  - `evm_recipient` (`BytesN<20>`): Destination EVM address.
  - `amount` (`i128`): Amount burned and bridged.
  - `destination_chain_id` (`u32`): Target EVM chain id.
  - `token` (`Address`): Token contract burned on Soroban.
  - `timestamp` (`u64`): Ledger timestamp of the withdrawal.

### `AxelarDepositCreditedEvent`
Emitted when an inbound Axelar deposit is credited.

- **Topics**: `("axelar_deposit_credited_event", message_id: String)`
- **Data Map**:
  - `recipient` (`BytesN<32>`): Credited account id.
  - `amount` (`i128`)
  - `source_chain` (`String`)
  - `credited_total` (`i128`): Running total credited to `recipient`.
  - `credited_ledger` (`u32`)

### `DisputeResolvedEvent`
Emitted when a trusted LayerZero peer's dispute resolution is accepted.

- **Topics**: `("dispute_resolved_event", dispute_id: BytesN<32>)`
- **Data Map**:
  - `resolution_hash` (`BytesN<32>`)
  - `approved_amount` (`u64`)
  - `src_chain_id` (`u32`): LayerZero chain id the packet came from.
  - `peer` (`BytesN<32>`): The trusted peer that delivered it.
  - `nonce` (`u64`): Packet nonce, persisted to block replays.
  - `settled_ledger` (`u32`)

> Wormhole VAA verification emits no events.
