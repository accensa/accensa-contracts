<div align="center">
  <h1>accensa-contracts</h1>
  <p><strong>Verifiable receipts and policy-bounded refunds for x402 payments on Stellar</strong></p>
  <p>
    <img src="https://img.shields.io/github/actions/workflow/status/accensa/accensa-contracts/ci.yml?branch=main" alt="CI Status" />
    <img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="License" />
    <img src="https://img.shields.io/badge/soroban--sdk-27.0.0-orange.svg" alt="soroban-sdk 27" />
    <img src="https://img.shields.io/badge/testnet-deployed-success.svg" alt="Deployed on testnet" />
  </p>
  <p>
    <a href="DEPLOYMENTS.md"><strong>Live on Testnet</strong></a> ·
    <a href="https://accensa-docs.vercel.app"><strong>Documentation</strong></a> ·
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
| `anchor_batch(root, count, period_start, period_end) -> u64` | Anchors a batch root, returns its `batch_id`. Merchant auth required. |
| `get_batch(batch_id) -> BatchRecord` | Reads an anchored batch. |
| `verify_receipt(batch_id, leaf, proof) -> bool` | Verifies a receipt against the anchored root. Read-only, free to call. |

Emits `Anchored` with topics `("anchored", batch_id)`.

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

Emits `Refunded` with topics `("refunded", payment_ref)`.

Enforced invariants, each covered by a test:

- **No double refunds** — a `payment_ref` can only be refunded once (`AlreadyRefunded`).
- **Time-bounded** — refunds past `refund_window_ledgers` are rejected (`WindowExpired`).
- **Float-bounded** — a refund can never exceed vault balance (`InsufficientFloat`).
- **Merchant-only** — every state-changing call requires merchant auth (`Unauthorized`).

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
cargo test                                      # 25 unit tests
cargo build --target wasm32v1-none --release    # wasm artifacts
```

### Deploy your own

```bash
./deploy.sh                      # testnet, identity "deployer"
TOKEN=<usdc-sac-id> ./deploy.sh  # settle refunds in USDC instead of XLM
```

Contract IDs are written to `deployments/<network>.env`.

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

The dashboard, indexer, and SDK that drive these contracts live in
[`accensa-app`](https://github.com/accensa/accensa-app).

## Testing

25 unit tests run against the Soroban test environment on every push, alongside
`cargo fmt --check` and `cargo clippy -D warnings`. CI does not swallow failures.

```
receipt-anchor   11 passed
refund-vault     14 passed
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security policy in [SECURITY.md](SECURITY.md).

## Contributors

<a href="https://github.com/accensa/accensa-contracts/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=accensa/accensa-contracts" />
</a>

## License

MIT — see [LICENSE](LICENSE).
