# Changelog

All notable changes to `ReceiptAnchor` and `RefundVault` are recorded here.

The two contracts are versioned together and share a tag. Versioning follows the
policy in [`docs/RELEASING.md`](docs/RELEASING.md): while the project is pre-1.0,
breaking changes bump the **minor** version, and they are called out as such.

## [Unreleased]

### Added

- **Admin events for `RefundVault`** (issue #114): `PauseEvent` and
  `UnpauseEvent` carry the ledger sequence so a pause window is reconstructible
  from the event log alone, and `RefundWindowUpdatedEvent` carries both the
  previous and the new window (old value captured before overwrite). All three
  follow the existing `#[contractevent]` convention and are documented in
  `docs/EVENTS.md` and the README event table.
- **Trustworthy build provenance in `contractmeta`** (issue #164): both
  `build.rs` files now fail loudly (a `cargo:warning`) when the git commit hash
  cannot be resolved instead of silently embedding `"unknown"`, embed a new
  `commit_dirty` key computed from `git status --porcelain`, and re-run on
  `.git/HEAD`, the resolved branch ref, the index and `src/` so a cached build
  cannot report a stale hash. A `test_commit_meta_is_well_formed` test in both
  crates pins the embedded commit to 40 hex characters.
- **Commit-reveal API on `RefundVault`** (issue #128): new `commit`,
  `reveal_refund`, `reveal_withdraw`, `get_commitment` and
  `get_commit_reveal_delay` functions. A merchant commits an opaque
  `sha256(plaintext || salt)` hash, waits the minimum
  `COMMIT_REVEAL_DELAY` (10 ledgers), then reveals the plaintext + salt to
  execute the identical refund/withdraw. The commitment is consumed on success.
  New error codes: `CommitmentNotFound` (302), `CommitmentMismatch` (303),
  `CommitmentNotDue` (304), `CommitmentAlreadyUsed` (305).

### Changed

- **Advanced WASM Memory Management for Merkle Proofs** (issue #139):
  Refactored `ReceiptShard::verify_receipt` to copy host vector inputs into a stack-allocated
  static buffer (`proof_buffer: [[u8; 32]; 128]`) and perform intermediate hashing using the pure Wasm
  `sha2` crate. This eliminates all guest heap allocations and host roundtrips for intermediate hashes,
  ensuring a flat guest memory footprint across all Merkle tree depths.
- **`RefundVault` token generality is documented and pinned** (issue #166): the
  vault treats all amounts as raw integer units in the token's smallest unit and
  performs no decimal arithmetic, so any SEP-41 precision behaves identically.
  New `token_agnostic_tests.rs` proves the full lifecycle (deposit, refund,
  withdraw, float-bound check) against 0- and 2-decimal tokens, including the
  smallest unit, i128 extremes, and a refund exactly equal to the float.
  Documented in `docs/storage-audit.md` (Token Generality) and
  `docs/contracts.mdx`.

### Security

- **Merchant-only float funding is a documented guarantee** (issue #157):
  `docs/SECURITY_MODEL.md` now states it explicitly — only the merchant's own
  funds are ever at stake, a third party cannot contribute float the merchant
  has not authorised, and `withdraw` stays merchant-only. The existing
  `test_deposit_from_non_merchant_fails` pins the behaviour and is annotated as
  deliberate.
- **Front-running of refunds/withdraws is mitigated** (issue #128): the
  merchant no longer reveals a refund/withdraw's full parameters in a single
  callable transaction. Instead they `commit` an opaque `sha256(plaintext ||
  salt)` hash on-chain first and only `reveal` the plaintext + salt after the
  minimum `COMMIT_REVEAL_DELAY`. A mempool observer cannot reconstruct or
  reorder the operation from the commitment alone. The `commit` on the revealed
  plaintext is verified against the stored hash (`CommitmentMismatch` on
  mismatch) and consumed on success, so a front-running replay is impossible.
  Security audit tests in `commit_reveal_tests.rs` pin the delay boundary, the
  mismatch rejection and the opacity of the commitment.

## [0.3.0] — 2026-08-26

### ⚠️ Breaking

- **`refund` gained a required `payment_amount` argument** (issue #99). Refunds
  are now cumulative: each call adds `amount` to a running total for the
  `payment_ref`, and the total can never exceed the `payment_amount` ceiling.
  The refund window is still measured from `paid_at_ledger`, never from a
  partial.
- **`RefundRecord` layout changed and is stored under a new key.** The single
  `amount` field is replaced by `amount_refunded` + `payment_amount`, and
  records are stored under a new `RefundV2` storage key. A `Refund` key written
  by the 0.2.0 single-refund rule is still recognised and treated as a
  fully-refunded payment (rejected with `ExceedsPayment`), never mis-decoded.
- **Error codes are unified across both contracts** (issue #98). Both contracts
  now return the single `accensa-common` `Error` enum; the two codes that used
  to collide (`AlreadyInitialized`, `NotInitialized`) keep their original
  values, and the anchor-only codes moved to a dedicated block (100+) so no two
  variants overlap. See the error table in the README.

### Added

`RefundVault`:

- **Partial refunds** — a payment may be refunded across multiple calls, each
  emitting a `RefundEvent` carrying both the per-call amount and the cumulative
  total, so an indexer never has to sum history.
- **Multisig contract-account admin support is verified and documented** (issue
  #97). Tests prove both contracts work with a `__check_auth` contract account
  as merchant — see `contracts/multisig-account`,
  `contracts/refund-vault/tests/multisig_admin_vault.rs` and
  `contracts/receipt-anchor/tests/multisig_admin_anchor.rs`.
- **Tests for the two README cross-contract claims** (issue #163):
  `readme_claim_payment_ref_is_receipt_leaf` and
  `readme_claim_refunds_outlive_pruned_batches` in
  `contracts/refund-vault/tests/integration_test.rs`.

### Added

- CI job enforcing `CHANGELOG.md` updates on contract changes and checking version alignment (#192).
- Shared cross-implementation test vectors and conformance suite for `RefundVault` (#184).
- Dependabot configuration for `cargo` and `github-actions` (#185).
- CI WASM artifact uploading and size budget enforcement gate (#186).

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
  `deposits - refunds - withdrawals` and never goes negative, cumulative
  refunds per `payment_ref` never exceed the supplied ceiling, paused
  operations never mutate state, and TTL extension never shortens a TTL while
  missing records always error. Budgets
  are tunable via `FUZZ_CASES`/`FUZZ_SEQ_LEN` with longer `#[ignore]`d local
  profiles.

### Deployment status

Like `0.2.0`, this is a source release: the live testnet addresses in
[`DEPLOYMENTS.md`](DEPLOYMENTS.md) still run `0.1.0`, and the new `refund`
signature, event shapes and error codes **do not exist at those addresses**.

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
- `verify_receipt` remains pinned to conformance vectors shared with the
  TypeScript SDK, so off-chain and on-chain verification are proven to agree.

### Deployment status

**The testnet deployment has deliberately not been updated to `0.2.0`.** The
contracts live at:

| Contract | Contract ID | Version deployed |
|---|---|---|
| `ReceiptAnchor` | `CBHRJU7CF4XIFRNDITFHNQHABKBMFM2FYFHLGWN3JGSFYYCDSMDAWPRV` | `0.1.0` |
| `RefundVault` | `CCMBM44EJUGD52G4LSMGHSXMAH2KSAQZX7VOYY4TTBF5BK4D7M4IHRQA` | `0.1.0` |

Soroban deployment mints a new contract ID. Redeploying would invalidate every
published address — including the ones the public receipt verifier at
<https://accensa-dashboard.vercel.app/verify> reads live, and every contract link
in this repository and in `accensa-app`. So `0.2.0` is a **source release**: the
tag, the notes and the reproducible build are the artifact. A redeployment is a
coordinated change across both repositories and is tracked separately in
[#59](https://github.com/accensa/accensa-contracts/issues/59), which also covers
pubnet.

Practical consequence: the new functions above and the new event topics exist in
the source and in the tagged build, **not at those two addresses**. Anything
reading the live contracts should keep treating them as `0.1.0`.

## [0.1.0] — 2026-07-14

First testnet deployment. `ReceiptAnchor` with `anchor_batch`, `get_batch`,
`verify_receipt` and `initialize`; `RefundVault` with `deposit`, `refund`,
`withdraw`, `get_refund`, `set_refund_window` and `initialize`. Contract IDs and
the transactions that created them are recorded in
[`DEPLOYMENTS.md`](DEPLOYMENTS.md).

[0.3.0]: https://github.com/accensa/accensa-contracts/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/accensa/accensa-contracts/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/accensa/accensa-contracts/releases/tag/v0.1.0
