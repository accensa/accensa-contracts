<div align="center">
  <h1>accensa-contracts</h1>
  <p><strong>Verifiable receipts and policy-bounded refunds for x402 payments on Stellar</strong></p>
  <p>
    <img src="https://img.shields.io/github/actions/workflow/status/accensa/accensa-contracts/ci.yml?branch=main" alt="CI Status" />
    <img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="License" />
    <img src="https://img.shields.io/badge/soroban--sdk-27.0.4-orange.svg" alt="soroban-sdk 27" />
    <img src="https://img.shields.io/badge/testnet-deployed-success.svg" alt="Deployed on testnet" />
  </p>
  <p>
    <a href="DEPLOYMENTS.md"><strong>Live on Testnet</strong></a> ·
    <a href="https://accensa.github.io/accensa-app/docs/contracts/overview"><strong>Documentation</strong></a> ·
    <a href="https://accensa-dashboard.vercel.app"><strong>Dashboard</strong></a> ·
    <a href="https://github.com/accensa/accensa-app"><strong>accensa-app</strong></a>
  </p>
</div>

> Part of the **[Accensa](https://github.com/accensa)** merchant back-office for
> x402 sellers on Stellar. This repo holds the on-chain half; the indexer,
> dashboard, and SDK live in [`accensa-app`](https://github.com/accensa/accensa-app).

## The Problem

x402 turns any HTTP endpoint into a paid resource: an AI agent hits your API, gets a
`402 Payment Required`, pays, and retries. That works — but it leaves both sides
without recourse.

**The agent cannot prove it was charged correctly.** Its receipt comes from the
seller's own API, attesting to the seller's own behaviour. When an autonomous agent
makes thousands of sub-cent calls a day across dozens of vendors, "trust the seller's
dashboard" is not an auditing story. Any disagreement is unresolvable, because the
only record is held by the party with an interest in it.

**The merchant cannot offer refunds without becoming a custodian.** Manual refunds
don't scale to per-request payments, and an unbounded refund key over merchant float
is exactly the thing a seller does not want sitting in a web backend.

`accensa-contracts` fixes both on-chain. Receipts are anchored in Merkle batches that
anyone can verify without asking the merchant. Refunds run through a vault with an
enforced time window and double-refund protection, so the policy lives in the contract
rather than in a support inbox.

Both contracts are **immutable**: they ship with no upgrade entry point and no
`update_current_contract_wasm`, so once deployed, nobody — not even the merchant —
can change the refund policy or how receipts verify. This is a deliberate security
property (see [ADR 003](docs/ADR-003-upgradeability.md)); a logic change means a
new contract ID and the migration procedure documented there.

## Why Stellar

This design is only economical on Stellar:

- **Sub-cent fees make per-request payments viable at all.** x402 is about
  micropayments; on most chains the settlement fee exceeds the payment itself.
- **Batched anchoring amortises to near zero.** One `anchor_batch` call covers an
  entire billing period, so verifiability costs a fraction of a cent per receipt.
- **USDC is native.** Merchant float and refunds settle in the asset merchants
  actually price in, through the Stellar Asset Contract, with no bridge.
- **Soroban's fee model is predictable**, so a merchant can bound the cost of their
  refund policy in advance rather than guessing at gas.

## Contracts

### `ReceiptAnchor`

Stores Merkle roots of batched payment receipts so agents can independently verify
they were charged correctly, with no trusted API in the path.

| Function | Purpose |
|---|---|
| `initialize(merchant)` | Binds the contract to a merchant admin address. |
| `anchor_batch(root, count, period_start, period_end) -> u64` | Anchors a batch root, returns its `batch_id`. Merchant auth required. `count` must be $\le$ 1000 (`MAX_BATCH_SIZE`). Rate-limited if `min_anchor_interval > 0`. |
| `get_batch(batch_id) -> BatchRecord` | Reads an anchored batch. |
| `get_batch_count() -> u64` | Returns the total number of anchored batches. Read-only. |
| `get_admin() -> Address` | Returns the configured merchant admin address. Read-only; fails with `NotInitialized` before `initialize`. |
| `get_pruned_up_to() -> u64` | Returns the internal `PrunedUpTo` cursor: the lower bound of the pruned prefix. Read-only; fails with `NotInitialized` before `initialize`. |
| `get_max_batch_size() -> u32` | Returns `MAX_BATCH_SIZE` (currently 1000). Read-only; clients should discover the limit via this getter rather than hard-coding it. |
| `set_min_anchor_interval(interval)` | Sets the minimum seconds between anchors (0 = disabled, max 86,400). Merchant auth required. |
| `get_min_anchor_interval() -> u32` | Returns the current minimum anchor interval in seconds. Read-only. |
| `verify_receipt(batch_id, leaf, proof) -> bool` | Verifies a receipt against the anchored root. Read-only, free to call. Returns `ProofTooLong` if the proof exceeds `MAX_PROOF_LEN` (10). |
| `verify_receipt_by_root(root, leaf, proof) -> bool` | Verifies a receipt against any root in the historical ring buffer. Returns `ProofTooLong` if the proof exceeds `MAX_PROOF_LEN`. |
| `get_root_buffer() -> Vec<BytesN<32>>` | Returns the current ring buffer of historical roots. Read-only. |
| `get_root_buffer_size() -> u32` | Returns `ROOT_BUFFER_SIZE` (currently 100). Read-only. |
| `get_max_proof_len() -> u32` | Returns `MAX_PROOF_LEN` (currently 10). Read-only; clients should discover the limit via this getter. |
| `extend_batch_ttl(batch_id)` | Extends the TTL of a batch to prevent archival. Publicly callable. |
| `prune_batches(before_ledger)` | Deletes anchored batches older than `before_ledger` to reclaim rent. Merchant auth required. |

Pruning walks forward from an internal `PrunedUpTo` cursor and stops at the first batch
that is not old enough, so the deleted range always stays a contiguous prefix — a batch
is never removed from the middle while older ones remain readable.

`MAX_BATCH_SIZE` (1000) caps how many receipts may appear in one `anchor_batch`. Call `get_max_batch_size` to discover the limit at runtime instead of hard-coding it.

Emits:

| Event | Topics | Data |
|---|---|---|
| `AnchorEvent` | `("anchor_event", batch_id)` | `root`, `count`, `period_start`, `period_end` |
| `PruneEvent` | `("prune_event", start_batch_id)` | `end_batch_id` |

The `AnchorEvent` data map mirrors `BatchRecord`, so an indexer decodes it with the same
shape `get_batch` returns.

Proofs use **sorted-pair SHA-256**: siblings are concatenated smaller-hash-first, so
proofs carry no left/right position flags. The TypeScript SDK in
[`accensa-app`](https://github.com/accensa/accensa-app) implements the identical
convention, and both are checked against the same anchored batch on testnet — see
[DEPLOYMENTS.md](DEPLOYMENTS.md#verifying-the-live-deployment-yourself).

### `RefundVault`

Holds merchant float and executes refunds bounded by an on-chain policy.

| Function | Purpose |
|---|---|
| `initialize(merchant, token, refund_window_ledgers)` | Sets admin, settlement token, and refund window. |
| `deposit(from, amount)` | Merchant tops up float. |
| `refund(payment_ref, recipient, amount, paid_at_ledger, payment_amount)` | Refunds part or all of a payment, subject to policy. `amount` is added to the cumulative total for `payment_ref`; `payment_amount` is the original payment amount and the hard ceiling on cumulative refunds. A configured fee (if any) is deducted before the payout. |
| `claim_batch(claims)` | Refunds multiple claims in one transaction (`Vec<RefundClaim>`, one struct per `refund` call). Atomic: one failing claim reverts the whole batch. One merchant signature, one reentrancy lock, and a `RefundEvent` per claim. Per-element float checks mean it can never overdraw the vault. |
| `process_batch(refunds)` | Best-effort batch refunds (`Vec<RefundParam>`, same shape as `RefundClaim`). Returns `Vec<bool>` — one entry per claim (`true` = applied), and a failing claim does **not** roll back the others. Capped at 100 claims per call (`BatchTooLarge`). Every claim runs the identical per-claim logic as `refund`, including the policy deadline check and the configured fee. Non-atomic by design: use `claim_batch` when all-or-nothing semantics are required. |
| `withdraw(amount, to)` | Merchant withdraws float. |
| `propose_policy(ledgers, deadline)` | Proposes a new refund policy — a window (in ledgers) plus a wall-clock deadline (Unix timestamp; `0` = no deadline); subject to timelock. |
| `execute_policy()` | Executes a pending policy change after the timelock. Applies both the new window and the new deadline. |
| `get_pending_policy()` | Returns the current pending policy proposal, if any. |
| `get_policy_timelock()` | Returns the policy timelock delay in ledgers (read-only). |
| `get_refund_deadline()` | Returns the configured policy deadline as a Unix timestamp (`0` = none, read-only). |
| `set_fee_bps(bps)` | Sets the refund fee rate in basis points (0–10_000, default 0). Merchant auth, emits `FeeConfigUpdatedEvent`. |
| `set_fee_recipient(recipient)` | Sets the address that collects the refund fee; rejects the vault's own address. Merchant auth, emits `FeeConfigUpdatedEvent`. |
| `get_fee_bps()` | Returns the configured fee rate in basis points (read-only). |
| `get_fee_recipient()` | Returns the configured fee recipient, if any (read-only; falls back to the merchant at claim time). |
| `get_refund(payment_ref) -> Option<RefundRecord>` | Looks up a refund. |
| `get_admin() -> Address` | Returns the admin (merchant) address. Read-only; fails with `NotInitialized` before `initialize`. |
| `get_token() -> Address` | Returns the settlement token address. Read-only; fails with `NotInitialized` before `initialize`. |
| `get_refund_window() -> u32` | Returns the refund window in ledgers (`0` = no time bound). Read-only; fails with `NotInitialized` before `initialize`. |
| `is_paused() -> bool` | Returns whether the vault is paused. Read-only; fails with `NotInitialized` before `initialize`, `false` otherwise. |
| `pause()` | Pauses operations for emergency stops. Merchant auth required. |
| `unpause()` | Resumes paused operations. Merchant auth required. |
| `extend_refund_ttl(payment_ref)` | Extends the TTL of a refund record to prevent archival. Publicly callable. |

**Config getters are individual, not a batch `get_config`.** Exposing the four
stored values as separate read-only calls (`get_admin`, `get_token`,
`get_refund_window`, `is_paused`) — rather than a single struct-returning
`get_config` — keeps the publish ABI compositional and stable as new
configuration is added: a client that only needs one value reads exactly one
storage key, the `#[contracttype]` payload does not change shape when config
grows, and the `is_paused` distinction (missing admin ⇒ `NotInitialized`,
initialized ⇒ `false`) could not be expressed faithfully in one struct anyway.
The status quo is the supported way to read config; do not decode raw ledger
entries by storage key (see issue #195).

Emits:

| Event | Topics | Data |
|---|---|---|
| `DepositEvent` | `("deposit_event", from)` | `amount` |
| `RefundEvent` | `("refund_event", payment_ref)` | `amount` (this call), `fee` (this call), `cumulative_refunded`, `recipient`, `ledger` |
| `WithdrawEvent` | `("withdraw_event", to)` | `amount` |
| `PauseEvent` | `("pause_event", ledger)` | — |
| `UnpauseEvent` | `("unpause_event", ledger)` | — |
| `RefundWindowUpdatedEvent` | `("refund_window_updated_event", previous_window, new_window)` | — |
| `PolicyProposedEvent` | `("policy_proposed_event", window)` | `deadline`, `proposed_at_ledger`, `execute_after_ledger` |
| `PolicyExecutedEvent` | `("policy_executed_event", window)` | `deadline` |
| `FeeConfigUpdatedEvent` | `("fee_config_updated_event", field)` | `fee_bps`, `fee_recipient` (full effective config) |

Each partial refund emits its own `RefundEvent` carrying **both** the amount for
that call (`amount`) and the running total (`cumulative_refunded`), so an indexer
knows the state of a payment without summing history. A batch of claims emits one
`RefundEvent` per item, in claim order. `RefundRecord` stores the
cumulative total (`amount_refunded`) plus the `payment_amount` ceiling, the
`paid_at_ledger` the window is measured from, and the recipient. When a fee is
configured, each `RefundEvent` also carries the `fee` deducted from the claim,
and the fee is paid to the `fee_recipient` alongside the recipient's payout.

**Cross-Contract Joins** (both claims below are pinned by tests in
`contracts/refund-vault/tests/integration_test.rs`):
- **`payment_ref` ↔ receipt-leaf** *(covered by `readme_claim_payment_ref_is_receipt_leaf`)*: The `payment_ref` used to key refunds is identical to the `leaf` hash of the payment receipt anchored in `ReceiptAnchor`. This 1:1 mapping guarantees that the on-chain refund explicitly corresponds to the exact payment record provided to the agent.
- **Refunds outlive pruned batches** *(covered by `readme_claim_refunds_outlive_pruned_batches`)*: Archiving or pruning a batch in `ReceiptAnchor` has no effect on the `RefundVault`. A payment can be successfully refunded even if its original anchor batch has been pruned, provided it still falls within the refund window.

Enforced invariants, each covered by a test:

- **Partial refunds within a ceiling** — a `payment_ref` may be refunded across
  multiple calls, but cumulative refunds can never exceed the original
  `payment_amount`; an over-ceiling call is rejected (`ExceedsPayment`). A batch
  accumulates against the same ceiling across its own elements.
- **Atomic batches** — `claim_batch` is all-or-nothing: a single failing claim
  reverts the transfers, records and events of every claim in the call.
- **Per-item float bound** — the float is read from the token contract before
  every claim (single or batched), so a batch can never overdraw the vault any
  more than the equivalent set of single refunds (`InsufficientFloat`).
- **Window from the original payment** — the refund window is measured from
  `paid_at_ledger` (the original payment), never extended by a partial
  (`WindowExpired`).
- **Deadline from the policy** — refunds stop being claimable once the
  configured wall-clock deadline has strictly passed (`RefundExpired`); a
  deadline of `0` disables expiry.
- **Fee-bounded split** — when configured, each claim is split into the
  recipient's payout and a fee that rounds **up** (sub-unit remainders accrue
  to the fee recipient); `payout + fee == amount` exactly, so the fee never
  expands the claim, the `payment_amount` ceiling, or the float check. Without
  a configured recipient the fee defaults to the merchant.
- **Float-bounded** — a refund can never exceed vault balance (`InsufficientFloat`).
- **Merchant-only** — every state-changing call requires merchant auth
  (`Unauthorized`); the admin may be a contract account (see
  [`docs/SECURITY_MODEL.md`](docs/SECURITY_MODEL.md#1-the-admin-merchant)).
- **Pausable** — operations are halted if the vault is paused (`Paused`).

## Error Codes

Both contracts return errors from a **single, shared enum** in
[`contracts/common`](contracts/common/src/lib.rs) (issue #98). Every variant has
an explicit, distinct `u32` value, so a frontend keeps one mapping across both
contracts instead of per-contract tables.

| Code | Variant | Meaning |
|---|---|---|
| 1 | `AlreadyInitialized` | `initialize` called twice. |
| 2 | `NotInitialized` | State-changing call before `initialize`. |
| 3 | `Unauthorized` | Caller is not the authorized merchant/admin. |
| 4 | `AlreadyRefunded` | Legacy single-refund marker (pre-#99). Retained for interface stability; the vault reports `ExceedsPayment` (19) for over-ceiling and legacy records since cumulative partial refunds. |
| 5 | `WindowExpired` | Refund window (from the original payment) has expired. |
| 6 | `InsufficientFloat` | Vault float is insufficient. |
| 7 | `InvalidAmount` | Amount was not strictly positive. |
| 8 | `Paused` | Vault is paused. |
| 9 | `RefundNotFound` | No refund record for the payment ref. |
| 12 | `NoPendingTransfer` | No admin transfer pending. |
| 13 | `StrategyNotSet` | No yield strategy configured. |
| 14 | `InsufficientReserve` | Yield deployment would breach the minimum reserve. |
| 15 | `DeploymentExceedsMax` | Yield deployment would exceed the max ratio. |
| 16 | `NothingToWithdraw` | Nothing to withdraw from the yield strategy. |
| 17 | `NothingToHarvest` | Nothing to harvest from the yield strategy. |
| 18 | `InvalidRatio` | A configured ratio was out of range. |
| 19 | `ExceedsPayment` | Cumulative refunds would exceed the payment ceiling. |
| 23 | `RefundExpired` | A refund claim was submitted after the policy deadline passed. |
| 100 | `BatchNotFound` | The requested batch does not exist (or was pruned). |
| 101 | `BatchTooLarge` | A batch larger than `MAX_BATCH_SIZE` was submitted. |
| 102 | `ShardCallFailed` | A shard call returned an unexpected shape. |
| 103 | `DuplicateRoot` | The anchored Merkle root equals the currently active root. |
| 200 | `RootNotFound` | The Merkle root is not in the historical ring buffer. |
| 201 | `ProofTooLong` | The Merkle proof exceeds `MAX_PROOF_LEN`. |
| 202 | `AnchorRateLimited` | An anchor was submitted before the minimum interval elapsed. |
| 300 | `NoPendingPolicy` | No pending policy change exists to execute. |
| 301 | `TimelockNotExpired` | The policy timelock period has not yet elapsed. |
| 302 | `FuturePaidAtLedger` | A refund reported `paid_at_ledger` in the future (greater than the current ledger sequence). |

Codes are stable: new variants are appended with fresh values, never renumbered.
Note that `10`/`11` are deliberately unassigned (`MetadataTooLong` and
`AmountExceedsMax` were dead variants removed in #170), and `4`
(`AlreadyRefunded`) is reserved after the `RefundV2` migration — surviving codes
keep their published values.

## Storage Archival

Soroban uses state archival to manage ledger bloat. The contracts are configured with a Time-To-Live (TTL) strategy that ensures active records remain in persistent storage for approximately 30 days (~518,400 ledgers) before they become eligible for archival.

If a `BatchRecord` or `RefundRecord` is archived, it must be restored by submitting a restore transaction before it can be read again. Anyone can proactively prevent archival and reset the 30-day window by calling the public TTL extension functions:
- `extend_batch_ttl(batch_id)` on `ReceiptAnchor`
- `extend_refund_ttl(payment_ref)` on `RefundVault`

For a complete breakdown of what is stored, why it is persistent, and the rent cost implications, read the [Storage Audit](docs/storage-audit.md).

## Live on Testnet

| Contract | ID |
|---|---|
| `ReceiptAnchor` | [`CBHRJU7CF4XIFRNDITFHNQHABKBMFM2FYFHLGWN3JGSFYYCDSMDAWPRV`](https://stellar.expert/explorer/testnet/contract/CBHRJU7CF4XIFRNDITFHNQHABKBMFM2FYFHLGWN3JGSFYYCDSMDAWPRV) |
| `RefundVault` | [`CCMBM44EJUGD52G4LSMGHSXMAH2KSAQZX7VOYY4TTBF5BK4D7M4IHRQA`](https://stellar.expert/explorer/testnet/contract/CCMBM44EJUGD52G4LSMGHSXMAH2KSAQZX7VOYY4TTBF5BK4D7M4IHRQA) |

Batch #1 is anchored and live. You can verify a receipt against it — and watch a
forged receipt get rejected — with two read-only commands that cost nothing:
see [DEPLOYMENTS.md](DEPLOYMENTS.md#verifying-the-live-deployment-yourself).

## Getting Started

### Prerequisites

```bash
rustup target add wasm32v1-none
cargo install --locked stellar-cli
```

### Build and test

```bash
cargo test
cargo build --target wasm32v1-none --release    # wasm artifacts
```

### Deploy your own

```bash
./deploy.sh                      # testnet, identity "deployer"
TOKEN=<usdc-sac-id> ./deploy.sh  # settle refunds in USDC instead of XLM
```

Contract IDs are written to `deployments/<network>.env`.

For mainnet deployment instructions and fee/rent analysis, see the [Mainnet Deployment Guide](docs/MAINNET_DEPLOYMENT.md).

## How the Pieces Fit

```
   agent pays ──▶ x402 endpoint (SDK middleware)
                        │
                        ▼
              Go indexer  ──reads SAC transfers──▶  Stellar
                        │
              batches receipts, builds Merkle root
                        │
                        ▼
              ReceiptAnchor.anchor_batch  ──▶  on-chain root
                        │
   agent ──verify_receipt(leaf, proof)──▶  true / false
```

For a full visual walkthrough including the refund flow and cross-contract
relationship, see the [Architecture Guide](docs/ARCHITECTURE.md).

The dashboard, indexer, and SDK that drive these contracts live in
[`accensa-app`](https://github.com/accensa/accensa-app).

## Testing

Tests run against the Soroban test environment on every push, alongside
`cargo fmt --check` and `cargo clippy -D warnings`. CI does not swallow failures.

Both contracts carry property-based fuzz suites (`src/fuzz_test.rs`) that generate
random operation sequences and assert invariants after every step — pruning stays a
contiguous prefix, Merkle verification rejects every wrong proof shape, vault float
always equals `deposits - refunds - withdrawals`, and a `payment_ref` can never be
refunded twice. CI runs a bounded budget; a longer profile is available locally:

```sh
cargo test -- --ignored          # longer profile
FUZZ_CASES=2000 FUZZ_SEQ_LEN=256 cargo test -- --ignored   # even longer
```

See the module headers in `contracts/*/src/fuzz_test.rs` for the approach and its
limits.


## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security policy in [SECURITY.md](SECURITY.md) and threat model in [docs/SECURITY_MODEL.md](docs/SECURITY_MODEL.md). For deployment errors, see [TROUBLESHOOTING.md](TROUBLESHOOTING.md).

## Contributors

<a href="https://github.com/accensa/accensa-contracts/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=accensa/accensa-contracts" />
</a>

## License

MIT — see [LICENSE](LICENSE).
