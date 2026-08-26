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
| `anchor_batch(root, count, period_start, period_end) -> u64` | Anchors a batch root, returns its `batch_id`. Merchant auth required. `count` must be $\le$ 1000 (`MAX_BATCH_SIZE`). |
| `get_batch(batch_id) -> BatchRecord` | Reads an anchored batch. |
| `get_batch_count() -> u64` | Returns the total number of anchored batches. Read-only. |
| `get_max_batch_size() -> u32` | Returns `MAX_BATCH_SIZE` (currently 1000). Read-only; clients should discover the limit via this getter rather than hard-coding it. |
| `verify_receipt(batch_id, leaf, proof) -> bool` | Verifies a receipt against the anchored root. Read-only, free to call. |
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
| `refund(payment_ref, recipient, amount, paid_at_ledger)` | Refunds a payment, subject to policy. |
| `withdraw(amount, to)` | Merchant withdraws float. |
| `set_refund_window(ledgers)` | Updates the window; `0` disables expiry. |
| `get_refund(payment_ref) -> Option<RefundRecord>` | Looks up a refund. |
| `pause()` | Pauses operations for emergency stops. Merchant auth required. |
| `unpause()` | Resumes paused operations. Merchant auth required. |
| `extend_refund_ttl(payment_ref)` | Extends the TTL of a refund record to prevent archival. Publicly callable. |

Emits:

| Event | Topics | Data |
|---|---|---|
| `DepositEvent` | `("deposit_event", from)` | `amount` |
| `RefundEvent` | `("refund_event", payment_ref)` | `amount`, `recipient`, `ledger` |
| `WithdrawEvent` | `("withdraw_event", to)` | `amount` |

The `RefundEvent` data map mirrors `RefundRecord`, so an indexer decodes it with the
same shape stored under the payment ref.

**Cross-Contract Joins**:
- **`payment_ref` ↔ receipt-leaf**: The `payment_ref` used to key refunds is identical to the `leaf` hash of the payment receipt anchored in `ReceiptAnchor`. This 1:1 mapping guarantees that the on-chain refund explicitly corresponds to the exact payment record provided to the agent.
- **Refunds outlive pruned batches**: Archiving or pruning a batch in `ReceiptAnchor` has no effect on the `RefundVault`. A payment can be successfully refunded even if its original anchor batch has been pruned, provided it still falls within the refund window.

Enforced invariants, each covered by a test:

- **No double refunds** — a `payment_ref` can only be refunded once (`AlreadyRefunded`).
- **Time-bounded** — refunds past `refund_window_ledgers` are rejected (`WindowExpired`).
- **Float-bounded** — a refund can never exceed vault balance (`InsufficientFloat`).
- **Merchant-only** — every state-changing call requires merchant auth (`Unauthorized`).
- **Pausable** — operations are halted if the vault is paused (`Paused`).

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
