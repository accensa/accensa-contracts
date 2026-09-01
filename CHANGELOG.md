# Changelog

All notable changes to `ReceiptAnchor` and `RefundVault` are recorded here.

The two contracts are versioned together and share a tag. Versioning follows the
policy in [`docs/RELEASING.md`](docs/RELEASING.md): while the project is pre-1.0,
breaking changes bump the **minor** version, and they are called out as such.

## [Unreleased]

### Fixed

- **Repaired source corruption that left `main` unable to compile.** Two bad
  merges (`a6e234b`, then `8eb4fa6` "Resolve conflicts in PR 263") committed
  literal conflict markers into `receipt-anchor/src/lib.rs` and spliced
  function bodies into the wrong signatures across `refund-vault/src/lib.rs`,
  `src/fuzz_test.rs` and `src/test.rs`. Nothing in the workspace built, so
  every check on every branch had been failing. Specifically:
  - `receipt-anchor`: removed the committed `<<<<<<< ours` markers; restored
    `get_shard_capacity`, `get_shard_count`, `get_shard_address` and
    `set_min_anchor_interval`, whose bodies the bad resolution had swallowed
    into `set_anchor_rate_limit`; restored `get_min_anchor_interval` and the
    `MinAnchorInterval` key and `MAX_ANCHOR_INTERVAL` cap they need (the
    governance suite drives this pair end to end).
  - `refund-vault`: rebuilt `__constructor` (it had been left half-merged with
    the removed `initialize` signature), and restored `set_reserve_ratio`,
    `set_max_deploy_ratio`, `deploy_to_yield`, `withdraw_from_yield`,
    `harvest_yield`, `pause`, `set_fee_bps`, `set_fee_recipient`,
    `set_oracle_policy` and `clear_oracle_policy`, each of which had been
    carrying a neighbouring function's body.
  - `fuzz_test.rs`: dropped a dead second op-model (`arb_op`, `execute_op`,
    referencing `Op` variants that no longer exist) that the merge had left
    interleaved with the live one, and restored the `execute` driver and
    `amount_strategy` the live tests call.
- **`Error::InvalidRateLimitConfig` (204)** was referenced by
  `ReceiptAnchor::set_anchor_rate_limit` and its tests but never existed on the
  shared `Error` enum. Added.
- **`test_extend_refund_ttl_fails_if_missing`** called `get_refund` (which
  returns `Option` and cannot yield `RefundNotFound`) instead of
  `extend_refund_ttl`, so it never tested what it was named for.
- **`test_nonce_does_not_increment_on_failed_operation`** called the removed
  `set_refund_window`, and with a window that contradicted its own
  `WindowExpired` assertion. Removed the stale call.

### Changed

- **`receipt_anchor` WASM budget raised 48000 -> 59000 bytes.** The 48000 figure
  was set before sharding, the token-bucket rate limiter and the zk verifier
  landed, and was never re-validated because the contract had not compiled
  since. 59000 reflects the contract as actually built (58847 bytes).

### Removed

- **`RefundVault::test_uninitialized_calls_fail`** and
  **`test_set_yield_strategy_uninitialized_fails`** asserted `NotInitialized`
  on a vault registered without arguments. Issue #129 moved initialization into
  `__constructor`, so that state is now unreachable — registration itself
  fails. Replaced by `test_vault_cannot_be_deployed_without_config`, which
  asserts the stronger constructor-level property; the auth path stays covered
  by `test_set_yield_strategy_requires_auth`.

- **CI and Toolchain Configuration**: updated `.github/workflows/ci.yml` to use stable `dtolnay/rust-toolchain` action references and synchronized `Cargo.lock` with dependency changes.

### Added

- **`RefundVaultFactory` with constructor-wired vaults** (issue #129): vaults
  are now created by a singleton factory via `deploy_vault(vault_init)` and are
  fully initialized in their constructor — there is no `initialize` window to
  front-run. The factory owns the deployment inputs a merchant must not be able
  to pick (`vault_wasm_hash`, and the stateless time/VDF policy contract
  addresses), derives a deterministic salt per merchant, and authorizes the
  merchant so griefing on someone else's salt family is impossible. Policy
  resolution: a policy set on the merchant's `VaultInit` wins; `None` falls
  back to the factory's global policy addresses. A vault deployed with a `None`
  policy on an active gate is still deployable but refuses the gate at claim
  time with `PolicyContractsNotConfigured` (317). The time and VDF gates are
  delegated to new stateless `TimePolicy` and `VdfPolicy` contracts
  (`contracts/refund-policy-time`, `contracts/refund-policy-vdf`) that
  evaluate a claim's window/deadline and Wesolowski proof respectively, keeping
  per-vault storage and upgrade surface small. Direct (non-factory)
  deployments of `RefundVault` remain supported.

- **Never persist `Option::None` (a `Void` value) in contract storage**: the
  vault and factory formerly stored their `Option<Address>` policy fields
  verbatim, so a cleared policy left a `Void` in the ledger, which the host
  rejects/breaks on in several read paths (observed as `Error(Context,
  InvalidAction)` and abort traps in the wasm constructor path). Policy fields
  are now written only when `Some`, and setters `remove()` the key on `None`.
  Absent key ⇔ unconfigured, which is the correct on-chain semantic anyway
  (`Void` is not a legal contract-data value).

- **Per-user nonce replay protection** (issue #122): `refund`, `claim_batch`,
  and `process_batch` now each take a required `nonce` argument equal to the
  caller's (merchant's) current per-user counter, stored in persistent storage
  under `DataKey::UserNonce(Address)`. The counter starts at `0`, increments
  once per **successful** claim call, and a mismatch — replaying a consumed
  nonce or skipping ahead — reverts the call with `Error::StaleState`
  (mirroring the state channel's nonce semantics). Failed claims revert
  atomically and do not consume a nonce; an empty `process_batch` returns
  early without consuming one. New `get_user_nonce(caller)` getter, and new
  tests covering sequential advance, replay rejection, per-caller
  independence, and batched entry points. This is a **breaking interface
  change** to the three claim entry points.

### Changed

- **`Governance` contract**: a proposal-based, weighted-vote governance wrapper
  that closes the single-admin-key SPOF on `ReceiptAnchor`'s merchant role. A
  fixed set of members (each with a weight, set at construction) can `propose`
  a call to any target contract, `vote` on it over a bounded voting window,
  and once "yes" weight clears a configured quorum (in basis points) and
  strictly exceeds "no" weight, anyone can `execute` it. No change to the
  governed contract is required: `initialize` `ReceiptAnchor` with a
  `Governance` instance's address as `merchant`, and its existing
  merchant-gated calls (`set_min_anchor_interval`, `anchor_batch`,
  `prune_batches`) become reachable only through a passed proposal — the
  host's own self-authorization rule (a contract's `require_auth()` on its
  own address auto-succeeds when it is the direct caller) is what carries the
  authority through, the same mechanism already used for a `MultisigAccount`
  admin. Storage is kept intentionally small: each member's weight is its own
  persistent entry, per-voter "already voted" markers live in **temporary**
  storage so they expire on their own with the voting window, and a resolved
  proposal's calldata can be reclaimed immediately via `prune_proposal`
  rather than waiting on archival. `ReceiptAnchor` has no upgrade entry point
  (see `docs/ADR-003-upgradeability.md`, which forbids adding one without
  reopening that ADR), so this wrapper does not gate or introduce one — it
  covers the admin surface that actually exists today. See
  `contracts/governance/src/lib.rs` and
  `contracts/governance/tests/receipt_anchor_admin.rs`.

- **Commit-reveal front-running protection for `RefundVault`** (issue #128):
  sensitive vault operations (`refund`, `claim_batch`, `propose_policy`) are
  visible in the mempool, so a validator or bot could observe a merchant's
  transaction and submit a competing one with a higher fee, reordering the
  pool. To close that window, `RefundVault` now implements a commit-reveal
  scheme: the merchant first calls `commit(operation, commitment_hash)` to
  bind a SHA-256 hash of the intended action (with no plaintext visible), then
  calls `reveal(operation, commitment_hash, plaintext)` no earlier than
  `COMMIT_MIN_DELAY_LEDGERS` (7) ledgers later to surface and consume the
  action. `reveal` re-derives the hash from the plaintext and rejects a
  mismatch (`CommitMismatch`), a reveal before the delay (`CommitDelayNot
  Elapsed`), a reveal with no pending commit (`NoCommit`), a reveal under a
  different operation than the one committed (`CommitOperationMismatch`), and
  a duplicate pending commit (`CommitAlreadyExists`). Commitments are
  merchant-only and single-use. New error codes 305–309 are appended without
  renumbering. `get_commit_min_delay()` and `get_commit(hash)` are added as
  read-only getters.

- **VDF-gated refunds for `RefundVault`** (issue #138): the refund policy now
  carries a Verifiable Delay Function requirement — `propose_policy(ledgers,
  deadline, vdf_delay)` configures a delay in squarings (subject to the same
  timelock) and `execute_policy` applies it. When the policy has a delay
  configured, `refund`, every claim in `claim_batch`, and every item in
  `process_batch` must supply a valid **Wesolowski VDF proof** that the delay
  has genuinely elapsed; claims without one fail with `VdfProofRequired`
  (302), with an invalid or premature one with `InvalidVdfProof` (303), and a
  proof supplied against a policy with no delay with `VdfNotConfigured` (304).
  The proof is bound to the payment (challenge = `sha256(payment_ref)`), so it
  cannot be replayed across payments, and the delay is *computational* — a
  validator that controls block timestamps or transaction ordering cannot
  shorten it without factoring the contract's fixed 1024-bit modulus. The
  verifier (`contracts/refund-vault/src/vdf.rs`) runs in pure WASM via
  `crypto-bigint` (already in the dependency tree, so no new transitive
  crates), is exposed publicly as read-only `verify_vdf(challenge, delay,
  proof)` for randomness-verification flows, and its cost is pinned by a
  budget test (a verification measures ≈51k CPU units — about a tenth of a
  refund call). The new `get_vdf_delay()` getter exposes the configured delay.
  This is a **breaking change** for clients: the `propose_policy` signature is
  extended and `refund`/`RefundClaim`/`RefundParam` gain a `vdf_proof`
  argument/field. `initialize` is unchanged and existing deployments default
  to no delay (`0`), keeping them behaviour- and storage-compatible. The
  contract's modulus is a fixed constant with its factors discarded after
  generation; a production deployment should replace it with a
  ceremony-chosen modulus (see `docs/SECURITY_MODEL.md` § "VDF Fairness").

- **ZK validity proof batch anchoring for `ReceiptAnchor`**: `anchor_batch_zk`
  allows merchants to anchor batch state roots on-chain by providing a Groth16
  zero-knowledge validity proof (`ZkProof`), verifying validity in $O(1)$ time
  and saving computational overhead on-chain. Added `verify_zk_proof` to verify
  Groth16 proofs against verifying keys and public inputs, and introduced
  `Error::InvalidProof` (code 203).


- **Best-effort batch refunds for `RefundVault`**: `process_batch(refunds)`
  processes up to 100 claims in one transaction (`Vec<RefundParam>`, same shape
  as `RefundClaim`) under a single merchant authorization, returning
  `Vec<bool>` with one entry per claim. A failing claim is recorded as `false`
  and processing continues, so valid claims in a mixed batch complete rather
  than the whole call aborting; a batch larger than 100 claims fails with
  `BatchTooLarge`. Every claim runs the identical per-claim logic as `refund`
  (deadline, ceiling, float, and the configured fee), publishing a
  `RefundEvent` per applied claim. Non-atomic by design — callers that require
  all-or-nothing semantics should use `claim_batch` instead.
- **Batch refunds for `RefundVault`**: `claim_batch(claims)` refunds multiple
  claims in a single transaction, each processed with exactly the same logic,
  checks, fees and events as `refund`, sharing one merchant authorization and
  one reentrancy-lock acquisition. The batch is **atomic** — the first failing
  claim returns its error and the Soroban transaction revert discards every
  transfer, storage write and event of the batch, so either all claims persist
  or none do. The float is read fresh from the token contract before every
  element (so a batch cannot overdraw the vault), and repeated `payment_ref`s
  accumulate against the same ceiling across elements. Each claim publishes its
  own `RefundEvent` in claim order; an empty batch succeeds as a no-op. Callers
  pass a `Vec<RefundClaim>`, a `#[contracttype]` struct mirroring the `refund`
  arguments (`payment_ref`, `recipient`, `amount`, `paid_at_ledger`,
  `payment_amount`). This is an **additive** change: `refund` and every existing
  endpoint are unchanged (the shared claim path is extracted verbatim), and gas
  is pinned by a budget test asserting a ten-claim batch stays well under the
  default CPU and memory limits and scales near-linearly with a single claim.
- **Refund fees for `RefundVault`**: the merchant can configure a fee deducted
  from every successful refund — `set_fee_bps(bps)` fixes the rate (basis
  points, up to 10_000) and `set_fee_recipient(recipient)` the collector
  address; both are admin-only and setting the recipient to the vault's own
  address is rejected (`SelfTransfer`). `refund` splits each claim into the
  buyer's payout and the fee, which always rounds **up** so the sub-unit
  remainder accrues to the protocol. If no recipient is configured the fee
  defaults to the merchant. The total outflow per claim is unchanged
  (`payout + fee == amount`), the `payment_amount` ceiling and the float check
  are untouched, and the fee is `0` unless configured, so `initialize` and
  existing deployments are unaffected. New `get_fee_bps()` /
  `get_fee_recipient()` getters expose the configuration, and the
  `RefundEvent` data map gains a `fee` field (append-only, see
  `docs/EVENTS.md`).
- **Refund expiration deadline for `RefundVault`**: the refund policy now
  carries a wall-clock deadline (Unix timestamp) alongside the ledger-based
  window — `propose_policy(ledgers, deadline)` configures it (subject to the
  same timelock) and `execute_policy` applies it. `refund` rejects claims whose
  current ledger timestamp is strictly past the deadline with a new
  `RefundExpired` error (code 23); a deadline of `0` disables expiry. The new
  `get_refund_deadline()` getter exposes the configured deadline. This is a
  contract-source change: the `propose_policy` signature is extended, so it is
  a **breaking change** for clients; `initialize` is unchanged and existing
  deployments default to no expiration (deadline `0`), keeping them behaviour-
  and storage-compatible. Deadline boundary semantics are pinned by unit tests
  that manipulate the mock ledger timestamp.

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
- **Oracle aggregator for dynamic refund policies** (`RefundVault`): a
  standard `Oracle` interface (`get_price` + `get_last_update_ledger`) that
  any price/data feed contract can implement, merchant-whitelisted via
  `add_oracle`/`remove_oracle`/`get_oracles`; a median aggregator
  (`get_median_price`) that queries every whitelisted oracle for a feed and
  returns the median of the fresh (non-stale) values, so no single provider
  is trusted; and an `OraclePolicy` (feed, threshold, staleness bound,
  `refund_when_below`) installed via `set_oracle_policy`/`clear_oracle_policy`
  that gates `refund` and `process_batch` — a refund is only paid out while
  the aggregated feed satisfies the condition, failing closed on a missing
  whitelist or all-stale data. New events `oracle_policy_set_event` /
  `oracle_policy_cleared_event` and error codes 302–307
  (`NoOraclesConfigured`, `OracleAlreadyAdded`, `OracleNotFound`,
  `StaleOracleData`, `NoOraclePolicy`, `OraclePolicyDenied`).

### Changed

- **CI fixes**: the `build-wasm` job now builds the deployable contract crates
  with `--workspace --exclude testutils` — the `testutils` workspace member
  activates `soroban-sdk`'s `testutils` feature, which is not supported on the
  `wasm32v1-none` target and made every wasm build fail at the SDK boundary.


  The `.wasm-budget.json` size budgets are updated to the current deterministic
  release builds (receipt-anchor 33,067 B, refund-vault 85,453 B) with ~5%
  headroom — the exact-pin approach kept breaking on toolchain drift, and the
  refund-vault budget had not caught up with the VDF crypto code.

  The `ReceiptAnchor` budget gate in `fuzz_test.rs` is re-baselined for
  `verify_receipt`: the pure-WASM SHA-256 folding merged in #250 moved hashing
  out of the host into WASM, raising the host CPU instruction count for that
  path (~569.9k → ~780.8k) while cutting WASM instructions; the gate's limits
  now reflect the current implementation (measured 2026-08-29) and still allow
  15% headroom for toolchain drift.

- **Lower-cost Merkle proof verification** (issue #125): `ReceiptShard` and
  `ReceiptAnchor` now fold sorted-pair proofs in a single iterative pure-WASM
  SHA-256 loop, avoiding redundant proof buffering and host crypto roundtrips.
  Batch-size instruction measurements were added to the ReceiptAnchor test suite
  and documented in `docs/BENCHMARKS.md`.


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

### Fixed

- **`main` was failing CI** (left red by the advanced-wasm-memory merge):
  restored the truncated `assert_eq!` in
  `test_process_batch_exceeds_max_size_fails` (the file would not parse),
  fixed the clippy 1.98 `needless_borrow` / `unnecessary_cast` violations,
  excluded the host-only `testutils` crate from the wasm artifact build (it
  enables soroban-sdk's `testutils` feature, which the SDK rejects on wasm),
  and re-baselined the cost-regression constants and wasm size budgets to the
  freshly measured values (`verify_receipt` CPU 569,906 → 780,985 after the
  pure-Wasm sha2 rewrite; `refund` CPU 397,721 → 477,714; `refund_vault.wasm`
  37,376 → 56,320 bytes; `receipt_anchor.wasm` 24,576 → 33,792 bytes on the
  current toolchain).

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

