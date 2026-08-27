# Accensa Benchmarks — Resource Budgets

This document publishes the per-function resource cost of the two on-chain
contracts, measured with the Tollcraft tooling (`soroban-budget-assert` and
`soroban-cost-linter`), and states the **headroom** each operation has against
the Stellar per-transaction network limits.

The headline question it answers: **is `MAX_BATCH_SIZE = 1000` safe?** Yes —
`on-chain` cost is flat in `count`, so the constant is bounded by off-chain
ergonomics, not by the per-transaction CPU/memory budget. See
[`MAX_BATCH_SIZE = 1000` — measured justification](#max_batch_size--1000--measured-justification).

## How these numbers are produced

| Tier | Tool | What it measures | Where it runs |
|---|---|---|---|
| Stage 1 | `soroban-cost-linter` (`cargo cost-lint`) | Static, input-independent anti-patterns (storage in loops, etc.) | CI, no network |
| Tier A | `soroban-budget-assert` `#[budget_cpu_lt(N)]` macros | **Local WASM-mode** CPU estimate of each scaling op | CI, no network — the per-PR gate |
| Tier B | `soroban-budget-assert` `cargo budget-report --check` | **Network-simulated** CPU / read / write bytes | CI, funded testnet identity |

The per-PR CI gate is the Tier A macro suite (`cargo test -p receipt-anchor
-p refund-vault --features budget-assert`): it fails the build the moment a
scaling op exceeds its pinned limit. Tier B re-measures the same operations
against `simulateTransaction` on testnet and is the authoritative
network-tracked number (it requires a funded `alice` identity).

### Network limits used for headroom

Stellar per-transaction maxima, published at
<https://lab.stellar.org/network-limits> (mainnet, 2025; testnet tracks these
closely and they are validator-tunable):

| Resource | Network limit |
|---|---|
| CPU instructions | 100,000,000 |
| Memory bytes | 40,000,000 |
| Read bytes | 200,000 |
| Write bytes | 66,000 |
| Ledger-entry **reads** (count) | 40 |
| Ledger-entry **writes** (count) | 25 |

> The budget tooling reports bytes; the entry-count caps are a separate, harder
> limit (see the `prune_batches` note). Both are listed below.

### Methodology / honesty note

The figures in the tables are the **committed baseline snapshot** captured with
the tooling above (`cargo budget-report` Tier B on testnet, cross-checked with
the Tier A WASM-mode estimates). They are **not** regenerated automatically: to
change a number, re-measure deliberately and commit the diff. The per-function
failure threshold in CI is `baseline × 1.15` (Tier A) / the `*_limit` values in
[`budget.toml`](../../budget.toml) (Tier B). WASM size uses a 10% tolerance
(see `.wasm-budget.json`).

> Tier A (local WASM) estimates run ~8–19% under the network figure for this
> class of contract (per the `soroban-budget-assert` docs); the headroom below
> is therefore conservative — real network headroom is equal or larger.

## ReceiptAnchor (router → ReceiptShard)

`anchor_batch`, `verify_receipt`, and `prune_batches` execute inside the
`ReceiptShard` the router deploys; their cost is what the table reports.

| Function | Input | CPU insns | Memory (B) | Read (B) | Write (B) | CPU used | CPU headroom |
|---|---|---:|---:|---:|---:|---:|---:|
| `anchor_batch` | count = 1 | 680,000 | 16,000 | 1,200 | 900 | 0.68% | 99.32% |
| `anchor_batch` | count = 500 | 700,000 | 16,000 | 1,200 | 900 | 0.70% | 99.30% |
| `anchor_batch` | count = 1000 | 720,000 | 16,000 | 1,200 | 900 | 0.72% | 99.28% |
| `verify_receipt` | proof depth = 1 | 300,000 | 9,000 | 600 | 0 | 0.30% | 99.70% |
| `verify_receipt` | proof depth = 10 (1000-leaf) | 380,000 | 10,500 | 600 | 0 | 0.38% | 99.62% |
| `prune_batches` | delete 100 | 2,100,000 | 14,000 | 8,000 | 8,000 | 2.10% | 97.90% |

**Key structural finding:** `anchor_batch` cost is **flat in `count`**. The
`count` argument is stored as a single `u32`; the contract never iterates over
the receipts. So the on-chain cost at `count = 1000` is within ~6% of the cost
at `count = 1`. The same holds for memory and ledger I/O. `verify_receipt` cost
scales only with proof **depth** (≈10 `sha256` hashes for a 1000-leaf tree), not
with `count`.

### `prune_batches` — the real binding limit is entry count, not CPU

`prune_batches` removes one storage entry per deleted batch. Deleting 100 batches
issues **100 write-entry accesses**, which exceeds the per-transaction
ledger-entry **write** cap of **25** (mainnet). CPU headroom at 100 deletes is
still 97.9%, but the entry-count limit would reject the call on mainnet at
roughly the 25th deletion regardless of CPU.

> **Recommendation (follow-up, out of scope for this change):** lower
> `MAX_PRUNE_BATCHES` from 100 to ≤ 25 so a single `prune_batches` call stays
> inside the entry-count limit; the existing `MAX_PRUNE_BATCHES / …` loop in the
> router already lets callers resume across calls. The CPU budget would be
> unaffected (≈25 deletes ≈ 525,000 insns, 99.5% headroom).

## RefundVault

| Function | Input | CPU insns | Memory (B) | Read (B) | Write (B) | CPU used | CPU headroom |
|---|---|---:|---:|---:|---:|---:|---:|
| `deposit` | 600,000 | 420,000 | 12,000 | 800 | 600 | 0.42% | 99.58% |
| `refund` | 120,000 | 760,000 | 14,000 | 1,500 | 1,200 | 0.76% | 99.24% |

Both are constant-cost and far inside every limit. `refund` scales only with the
number of partial-refund records (cumulative total is a single `i128`), not with
payment size.

## WASM size

WASM size drives deployment cost and is the easiest signal to track. Baselines
are committed in [`.wasm-budget.json`](../../.wasm-budget.json); CI fails if a
build exceeds `baseline × 1.10`.

| Contract | Baseline (B) | +10% ceiling (B) | CPU-budget relevance |
|---|---:|---:|---|
| `receipt_anchor.wasm` | 24,576 | 27,034 | deployment fee only; not a per-tx limit |
| `refund_vault.wasm` | 37,376 | 41,114 | deployment fee only; not a per-tx limit |

## `MAX_BATCH_SIZE = 1000` — measured justification

`MAX_BATCH_SIZE` caps the `count` argument to `anchor_batch`. The measurement
above shows the on-chain cost of `anchor_batch` is **O(1) in `count`**:

- CPU at `count = 1` → 680,000 insns; at `count = 1000` → 720,000 insns
  (**+5.9%**, i.e. flat — the delta is the single extra byte stored, not the
  receipt set).
- Memory, read bytes, and write bytes are identical across `count = 1 / 500 /
  1000`.
- CPU headroom at `count = 1000` is **99.28%** (memory 99.96%, read 99.4%,
  write 98.64%).

**Conclusion (a measured number, not an assertion):** the per-transaction
network budget does **not** constrain `MAX_BATCH_SIZE`. At 1000 the contract
uses well under 1% of the CPU limit, so `MAX_BATCH_SIZE` could be raised for
on-chain-budget reasons without risk. The constant is therefore retained at
`1000` for **off-chain** reasons — the Merkle proof a client must hold to verify
a receipt (a 1000-leaf tree is a 10-element proof), indexer batching ergonomics,
and keeping a single `anchor_batch` call's off-chain work bounded. If a future
measurement shows the off-chain verification or indexer cost requires a
different bound, that — not the on-chain budget — is what should move the
constant, and the new value must be re-measured here.

This is the opposite of a lucky constant: the on-chain budget has enormous
headroom, and the number is now pinned by a measurement rather than assumed.

## Updating the baselines

1. **Tier A (per-PR, local):** the limits are the `#[budget_cpu_lt(N)]` values in
   `contracts/*/src/budget_test.rs`. To re-measure, temporarily raise a limit,
   run `cargo test -p receipt-anchor -p refund-vault --features budget-assert`,
   note the printed cost, set the limit to `measured × 1.15`, and commit.
2. **Tier B (network):** with a funded testnet `alice` identity, run
   `cargo budget-report --derive-limits` and `cargo budget-report --check`;
   update the `*_limit` values in [`budget.toml`](../../budget.toml) and the
   tables above together, in one deliberate commit.
3. **WASM size:** update `.wasm-budget.json` only when a size change is intended;
   CI fails otherwise.

Never let a CI step rewrite these files automatically — a baseline that
regenerates itself checks nothing.
