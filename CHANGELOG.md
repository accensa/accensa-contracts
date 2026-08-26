# Changelog

All notable changes to `ReceiptAnchor` and `RefundVault` are recorded here.

The two contracts are versioned together and share a tag. Versioning follows the
policy in [`docs/RELEASING.md`](docs/RELEASING.md): while the project is pre-1.0,
breaking changes bump the **minor** version, and they are called out as such.

## [Unreleased]

### ⚠️ Breaking

- **`RefundVault::get_refund` now returns `Result<RefundRecord, Error>` instead of `Option<RefundRecord>` (#<issue>).**
  This aligns `get_refund` with `ReceiptAnchor::get_batch` for API consistency and uses the `RefundNotFound` error variant that already existed. Callers consuming bindings and SDK clients (such as `accensa-app`) must update their calls and handle `Result` or `Error::RefundNotFound`.

### Fixed

- **Build was broken on `main` after the yield-strategy merge (#200).** The
  `YieldStrategy` trait used `#[contractimpl]`, which cannot generate a client on
  a bare trait; it is now `#[contractclient(name = "YieldStrategyClient")]`.
  `deploy_to_yield` also transferred tokens to the strategy without notifying it
  (`strategy_client.deposit`), so the strategy never recorded the principal and
  later withdrawals failed. `yield_tests.rs` additionally used event APIs that
  do not exist in this SDK. No deployed contract is affected — this restores a
  compiling, green test suite.

### Tested

- Property-based fuzz suites in `contracts/*/src/fuzz_test.rs` now generate
  random operation sequences and assert invariants after every step: pruning
  stays a contiguous prefix with a monotonic `PrunedUpTo` cursor, Merkle
  verification rejects every wrong proof shape (wrong leaf/sibling/length/batch
  and reversed level order), vault float always equals
  `deposits - refunds - withdrawals` and never goes negative, a `payment_ref`
  can never be refunded twice, paused operations never mutate state, and TTL
  extension never shortens a TTL while missing records always error. Budgets
  are tunable via `FUZZ_CASES`/`FUZZ_SEQ_LEN` with longer `#[ignore]`d local
  profiles.

## [0.2.0] — 2026-08-14

Everything below has been merged and tested on `main`. **It is not what is deployed
on testnet** — see [Deployment status](#deployment-status).

### ⚠️ Breaking

- **Event topics changed and any indexer written against `0.1.0` matches nothing.**
  `0.1.0` published events by hand as `("anchored", batch_id)` and
  `("refunded", payment_ref)`. Both contracts now derive their events with
  `#[contractevent]`, which emits the topics `anchor_event`, `prune_event`,
  `deposit_event`, `refund_event`, and `withdraw_event`. The README advertised the
  old topics for three weeks after the code had changed; that is fixed, and the
  shapes are now pinned as a contract in [`docs/EVENTS.md`](docs/EVENTS.md) with an
  Event Stability Policy in [`CONTRIBUTING.md`](CONTRIBUTING.md) so it cannot drift
  again silently.

### Added

`ReceiptAnchor`:

- `extend_batch_ttl(batch_id)` — public and unauthenticated, so anyone can stop an
  anchored batch being archived.
- `prune_batches(before_ledger)` — merchant-authorised, walking forward from a
  persisted `PrunedUpTo` cursor and stopping at the first batch not old enough, so
  the pruned range stays a contiguous prefix and no batch is ever removed from the
  middle.
- `get_batch_count()` — exposes the batch count; a maximum batch size is now
  enforced on `anchor_batch`.
- `AnchorEvent` and `PruneEvent`.

`RefundVault`:

- `pause()` / `unpause()` under merchant auth. Deposit, refund and withdraw all
  reject while paused.
- `extend_refund_ttl(payment_ref)` — public and unauthenticated, same rationale as
  above.
- `DepositEvent`, `RefundEvent` and `WithdrawEvent`, so the vault is indexable
  rather than poll-only.

Both:

- `contractmeta!` embedding `name`, `version`, `repo` and the build's `GIT_SHA`
  via a `build.rs`, so a deployed contract can be traced to its exact source
  commit. `deploy.sh` now records wasm `sha256sum` alongside the contract IDs.

### Changed

- `soroban-sdk` 27.0.0 → 27.0.4.
- TTL constants set to roughly 30 days of ledgers, with a threshold so a bump is
  not written on every call. Archival and restore implications are documented.
- `refund` now validates `amount > 0`.

### Fixed

- `RefundVault` storage `.set()` calls corrected.
- README test counts and event-topic names no longer contradict the code.

### Documentation

- [`docs/EVENTS.md`](docs/EVENTS.md) — the indexer-facing event contract.
- [`docs/storage-audit.md`](docs/storage-audit.md) — rewritten from a single line
  of escaped text into an audit of all 13 `DataKey` variants, with storage class,
  justification, TTL strategy and projected rent.
- [`docs/ADR-002`](docs/ADR-002-upto-scheme.md) — design notes on the x402 `upto`
  scheme for Stellar. Status **DRAFT**: the construction has not been validated
  against the upstream spec, a running contract, or Soroban's authorization
  semantics, and §6 lists what must be confirmed first.
- [`docs/RELEASING.md`](docs/RELEASING.md), [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md),
  and a SEP-41 section in [`docs/SECURITY_MODEL.md`](docs/SECURITY_MODEL.md)
  recording why `RefundVault` lets a missing-trustline transfer panic at the token
  rather than paying the budget cost of a pre-check.

### Testing

- 25 → **58 tests**: `receipt-anchor` 24, `refund-vault` 29, and 5 cross-contract
  integration tests that replaced a placeholder asserting nothing. The integration
  tests cover receipt correspondence, double-refund against a valid proof, refund
  of a payment inside a pruned batch, TTL archival across both contracts, and the
  pause interaction.
- `verify_receipt` remains pinned to conformance vectors shared
