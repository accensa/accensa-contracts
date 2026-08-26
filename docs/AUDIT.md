# Audit Readiness

This document is the preparation package for an external audit of the
`accensa-contracts` on-chain contracts. Its purpose is to let an auditor spend
their budget on the contracts themselves rather than on reconstructing what the
invariants are supposed to be — that work is done here, and the known risks are
stated up front so they can be verified rather than discovered.

Status: **ready to commission**, subject to the checklist in
[§8 Logistics](#8-logistics). The two blockers that previously stood in the way
are cleared:

- **#47 — placeholder integration test.** The placeholder was replaced with
  real cross-contract coverage (`contracts/refund-vault/tests/integration_test.rs`,
  five tests covering the payment-ref↔leaf correspondence, refund-of-pruned-batch,
  double-refund with a valid proof, pause interaction, and TTL archival across
  both contracts).
- **#55 — undecided upgradeability.** Resolved by
  [ADR-003](ADR-003-upgradeability.md) (**ACCEPTED**): both contracts are
  deliberately immutable, with a written migration runbook. The threat model
  this audit works against is fixed.

---

## 1. Scope

### 1.1 In scope

| Component | Location | Notes |
|---|---|---|
| `ReceiptAnchor` | `contracts/receipt-anchor` | Merkle batch anchoring + `verify_receipt`. |
| `RefundVault` | `contracts/refund-vault` | Policy-bounded refunds, float, pause, TTL, two-step admin transfer, yield-strategy integration. |
| Cross-contract behaviour | — | The contracts never call each other; the only external calls are to the token (SAC) and the optional yield strategy. |
| Live testnet deployment | `DEPLOYMENTS.md` | Read-only verification is possible against the deployed addresses; see §1.3 for the version caveat. |

**Functions in scope — `ReceiptAnchor`:** `initialize`, `anchor_batch`,
`get_batch`, `verify_receipt`, `get_batch_count`, `get_max_batch_size`,
`extend_batch_ttl`, `prune_batches`.

**Functions in scope — `RefundVault`:** `initialize`, `deposit`, `refund`,
`withdraw`, `set_refund_window`, `get_refund`, `set_yield_strategy`,
`set_reserve_ratio`, `set_max_deploy_ratio`, `deploy_to_yield`,
`withdraw_from_yield`, `harvest_yield`, `get_yield_info`, `pause`, `unpause`,
`extend_refund_ttl`, `transfer_admin`, `accept_admin`, `cancel_admin_transfer`.

Events (`AnchorEvent`, `PruneEvent`, `DepositEvent`, `RefundEvent`,
`WithdrawEvent`, `AdminTransferInitiatedEvent`, `AdminTransferAcceptedEvent`,
`YieldDeployedEvent`, `YieldWithdrawnEvent`, `YieldHarvestedEvent`) are in scope
only insofar as they must mirror stored state (see invariant [I-13](#i-13-events-mirror-state)).

### 1.2 Out of scope (and why)

- **The `soroban-sdk` / Soroban host environment and the Stellar protocol.**
  Trusted platform. The contracts depend on standard host behaviour (storage
  classes, TTL/archival, `require_auth`, footprint, atomicity of cross-contract
  calls within a transaction). Audit findings belong in the contracts, not the
  platform; we are not paying for a review of Stellar itself.
- **The Stellar Asset Contract (SAC) token implementation.** Trusted, SEP-41
  behaviour assumed (balance queries, transfer semantics, trustline
  enforcement). In scope only for how the vault *uses* it.
- **Off-chain components in `accensa-app`** (indexer, dashboard, SDK,
  middleware). Out of scope as code, but the Merkle convention they share with
  `ReceiptAnchor` is auditable via the pinned parity vectors — see
  [§6 Prior work](#6-prior-work).
- **Merchant key custody and private-key management.** An accepted operational
  risk, stated in [§5](#5-known-issues-and-accepted-risks).
- **The multi-sig design (issue #23).** The `Admins` / `Threshold` storage keys
  are reserved but inert — no logic reads them today. Auditing a control that
  does not exist is out of scope; verifying it stays inert is a one-line check
  (see [I-12](#i-12-reserved-keys-are-inert)).
- **x402 itself.** The HTTP payment protocol is out of scope; only the on-chain
  receipts/refunds machinery is audited.

### 1.3 Artifact to audit — pin the commit

The audit must be pinned to a specific artifact, not "the repo at some point":

- Both contracts embed `GIT_SHA` (via `build.rs` + `contractmeta!`) into the
  wasm, and `deploy.sh` records version, commit SHA, and wasm hashes into
  `deployments/<network>.env`. `DEPLOYMENTS.md` documents the live testnet
  record.
- **Testnet drift warning:** the addresses live on testnet run **`0.1.0`**;
  `main` is **`0.2.x`** (source-only so far). The `0.1.0` deployment has no
  `prune_batches`, `get_batch_count`, `extend_batch_ttl`, `pause`, `unpause`,
  `extend_refund_ttl`, yield, or two-step admin. An indexer pointed at those
  addresses uses old event topics (`("anchored", …)`, `("refunded", …)`). An
  audit that does not state which artifact it reviewed is worthless — see
  `DEPLOYMENTS.md` and the changelog.
- **Recommendation:** audit the exact commit destined for mainnet, and record
  that commit (plus its wasm hashes) in the engagement kickoff. The read-only
  `verify_receipt` walkthrough in `DEPLOYMENTS.md` can be re-run against the
  audited artifact to confirm the auditor's build matches the deployed one.

---

## 2. Trust Boundaries

Drawn as of `main`. Who is trusted, and to do what:

| Actor | Trusted to | Not trusted to | Notes |
|---|---|---|---|
| **Merchant admin** (one `Address` per contract) | Everything the contracts allow: anchor/prune, deposit/refund/withdraw, pause/unpause, set policy (window, ratios, strategy), deploy/withdraw/harvest yield, two-step admin handover. Fully trusted *within the contract's rules*. | Change code (contracts are immutable), rewrite anchored history, refund the same `payment_ref` twice, move float outside the vault's accounting. | A compromised admin key = total float loss. Stated plainly in [§5](#5-known-issues-and-accepted-risks). |
| **Multi-sig set** (`Admins`/`Threshold` keys) | Nothing. The keys are reserved for issue #23 and are inert today. | — | Must not be treated as an existing control. |
| **Public TTL-extenders** (anyone) | Nothing. `extend_batch_ttl` / `extend_refund_ttl` are deliberately permissionless so anyone can keep records alive. | Mutating anything except TTL. Extension cannot create/delete records, cannot shorten TTL, and errors on unknown refs. | Griefing cost analysed in [I-11](#i-11-ttl-extension-is-public-and-cannot-grief). |
| **Token contract (SAC)** | Correct SEP-41 behaviour: honest `balance`, honest `transfer`, trustline enforcement. | — | A broken/malicious token breaks the vault's float accounting. Accepted platform trust. |
| **Indexer (off-chain)** | Batching receipts correctly and anchoring the right root. | Forging a receipt: `verify_receipt` is on-chain and permissionless, so a compromised indexer cannot fake a proof without a SHA-256 collision. | |
| **Anyone verifying receipts** | Nothing. `verify_receipt` is read-only and free. | — | |
| **Yield strategy contract** (optional, admin-registered) | Honest `deposit`/`withdraw`/`harvest`/`total_balance`/`accrued_yield` and returning deployed tokens on request. | Being assumed solvent. The vault enforces *ratios*, not strategy solvency; a malicious or insolvent strategy can strand deployed funds. | New trust boundary — see [§5](#5-known-issues-and-accepted-risks). |

Two consequences worth stating explicitly:

- **The `payment_ref` ↔ receipt-leaf correspondence is a convention, not an
  on-chain guarantee.** `RefundVault.refund` never reads `ReceiptAnchor`; the
  vault refunds whatever 32-byte `payment_ref` the merchant supplies. The 1:1
  mapping to an anchored leaf is maintained by the off-chain indexer/SDK and
  tested in the integration suite — an auditor should not assume the vault
  itself enforces it.
- **The refund window is a policy control, not a cryptographic boundary.**
  `refund` enforces the window against the *merchant-supplied* `paid_at_ledger`.
  A merchant can always pass `paid_at_ledger = now` and refund any payment up to
  float. The window protects the merchant's own policy discipline and the
  agent's expectations; it does not constrain a malicious merchant (who is
  already trusted with the float).

---

## 3. Invariants

This is the list an auditor should try to break. Each entry states the invariant,
why it matters, and the shape of attack that would violate it. Unless noted,
each is asserted by unit, fuzz, or integration tests (§6).

### `ReceiptAnchor`

#### I-1. Batch IDs are strictly monotonic and gap-free
`anchor_batch` assigns `batch_count + 1` and increments `BatchCount`; IDs are
never reused and no batch can be overwritten. Only the merchant can anchor.
*Attack:* forge an `anchor_batch` without merchant auth; or cause two batches to
share an ID. The only state-changing path is merchant-authenticated, and there
are no external calls during `anchor_batch`, so there is no interleaving surface.

#### I-2. Every batch has `count ≤ MAX_BATCH_SIZE` (1000)
Enforced before auth. `get_max_batch_size` is the discovery API; clients must
not hard-code the limit.
*Attack:* anchor a batch with `count > 1000` — must return `BatchTooLarge`.

#### I-3. `verify_receipt` returns `true` iff the leaf is genuinely in the anchored tree
For `verify_receipt(batch_id, leaf, proof)`, `true` ⟺ `leaf` is a member of the
tree whose root is anchored at `batch_id`, under the sorted-pair SHA-256
convention (siblings concatenated smaller-hash-first, no left/right flags —
[ADR-001](ADR-001-merkle-structure.md)). `false` (a value, **not** an error) for
a wrong leaf, wrong sibling, wrong proof length, or reversed level order;
`BatchNotFound` for an absent/pruned batch. Soundness rests on SHA-256 collision
resistance.
*Attack:* any proof shape or leaf mutation that resolves to the anchored root —
this is the security property of the whole receipt story.

#### I-4. Pruning removes only a contiguous prefix
`prune_batches(before_ledger)` walks forward from the `PrunedUpTo` cursor and
stops at the first batch whose `anchored_ledger ≥ before_ledger`. Only batches
`[PrunedUpTo, pruned_up_to)` are removed; the cursor is monotonic; a batch is
never deleted out of the middle while older batches remain.
*Attack:* prune a non-prefix, or delete a batch newer than the cursor — e.g. by
calling with a `before_ledger` that jumps the cursor past live batches.

#### I-5. Pruning is bounded per call
At most `MAX_PRUNE_BATCHES` (100) deletions per call; the cursor persists, so
large prunes resume across calls. Keeps per-transaction compute bounded.
*Attack:* make a single `prune_batches` call do unbounded work.

#### I-6. A pruned batch is permanently gone
`prune_batches` uses `persistent::remove`, not TTL expiry: once pruned, the
batch cannot be re-created (IDs are never reused) and `verify_receipt` /
`get_batch` on it return `BatchNotFound`. This is deliberate — refunds never
depend on batches (see [I-14](#i-14-refunds-do-not-depend-on-anchored-batches)) —
but it means an old receipt's verifiability ends at prune time unless the
merchant retained the root off-chain.
*Attack:* resurrect a pruned batch, or double-count pruned batches in the cursor.

### `RefundVault`

#### I-7. A `payment_ref` is refunded at most once, under any call interleaving
The executed refund is recorded as a persistent tombstone keyed by `payment_ref`;
a second `refund` for the same ref returns `AlreadyRefunded`. This is the
double-refund guarantee that makes the vault safe to hold float.
*Attack (the one spot worth scrutiny):* the tombstone is written **after** the
outbound token transfer completes. The auditor should verify whether the
outbound `transfer` to a *contract* recipient can re-enter `refund` before the
tombstone exists, and confirm that re-entry re-runs the float and window checks
in a way that cannot pay out twice. The fuzz suite asserts the invariant over
random interleavings, but a single adversarial recipient contract is exactly the
case generated sequences do not cover — call this out as a targeted test for the
engagement.

#### I-8. Refunds outside `refund_window_ledgers` are impossible
`refund` rejects when `current_ledger > paid_at_ledger + window`
(`WindowExpired`). A window of `0` disables expiry (documented policy choice,
covered by a dedicated test). The boundary is inclusive: `paid_at_ledger + window`
itself is still refundable.
*Attack:* refund after the window (including with a reentrant interleaving that
skips the check), or overflow the `paid_at_ledger + window` addition in a way
that widens the window.

#### I-9. Vault float can never go negative; total outflow never exceeds total inflow
`refund` and `withdraw` both check the vault's token balance before transferring
(`InsufficientFloat`); `deposit` requires a positive amount and merchant auth.
Consequently, cumulative outflows (refunds + withdrawals + deployed principal)
can never exceed cumulative inflows (deposits + harvested yield). The fuzz model
asserts `float == deposits − refunds − withdrawals` after every step, and
`test_refund_exceeding_liquid_after_deploy_fails` pins the yield-path case.
*Attack:* any sequence that drains more than was deposited — e.g. via token
balance races, yield double-counting, or an underflowing accounting state.

#### I-10. Only the merchant admin can perform privileged state changes
`anchor_batch`, `prune_batches` (anchor); `deposit`, `refund`, `withdraw`,
`set_refund_window`, `pause`, `unpause`, `set_yield_strategy`,
`set_reserve_ratio`, `set_max_deploy_ratio`, `deploy_to_yield`,
`withdraw_from_yield`, `harvest_yield`, `transfer_admin`, `cancel_admin_transfer`
(vault) all `require_auth` the admin. `accept_admin` requires auth of the
*pending* admin only. `initialize` is one-shot (`AlreadyInitialized`).
*Attack:* any of these without the right key — including via a strategy or token
contract that calls back into the vault.

#### I-11. TTL extension is public and cannot be used to grief
`extend_batch_ttl(batch_id)` / `extend_refund_ttl(payment_ref)` are
permissionless by design (anyone can keep records alive; this is a feature, not
a bug). They error on unknown refs (`BatchNotFound` / `RefundNotFound`), never
shorten a TTL, and can only extend toward `TTL_EXTEND` (~30 days) when the
remaining TTL is below `TTL_THRESHOLD` (100 ledgers). **They mutate no state
other than TTL.**
*The cost of extending every record forever (stated as requested):* each
extension is a normal Soroban transaction whose **caller** pays the transaction
fee and the rent for the extended lifetime. Rent is ~0.5 XLM/KB/year; a
`BatchRecord`/`RefundRecord` is ~100 bytes, so keeping one record alive is
~0.05 XLM/year. For a vault with *N* records, an attacker holding every record
alive forever pays on the order of `N × (fee + 0.05 XLM/yr)`, repeated per
30-day cycle. The attack cannot corrupt state — its only effect is preventing
archival, which is the merchant's own goal — and the attacker pays, not the
merchant. At any realistic record count this is uneconomical and pointless.
*Attack:* extend a non-existent record (must error), or shorten/extend past
policy limits.

#### I-12. Reserved keys are inert
`DataKey::Admins`, `Threshold`, `Metadata`, `RefundMax` exist in the enum but no
code path reads or writes them. They must stay inert until issue #23 (multi-sig)
lands. *Auditor check:* grep confirms no storage access to these keys outside the
enum definition.
*Attack:* use the reserved keys to bypass admin checks — should be impossible
today; the auditor verifies.

#### I-13. Events mirror state
`RefundEvent` data ≡ the stored `RefundRecord`; `AnchorEvent` data ≡ the stored
`BatchRecord`; prune, deposit, withdraw, admin-transfer and yield events carry
the values that were actually written. An indexer reconstructing state from
events must agree with on-chain reads. See `docs/EVENTS.md`.
*Attack:* emit an event inconsistent with stored state (malleable / fabricated
values).

#### I-14. Refunds do not depend on anchored batches
Archiving or pruning a batch in `ReceiptAnchor` has no effect on `RefundVault`;
a payment can be refunded even after its batch is pruned, provided it is inside
the window. Covered by `test_refund_of_payment_in_pruned_batch`.
*Attack:* make refund availability depend on anchor state (e.g. by coupling the
two contracts).

### Cross-cutting

#### I-15. A paused vault mutates no fund-moving state
While paused, `deposit`, `refund`, `withdraw`, `deploy_to_yield`,
`withdraw_from_yield`, and `harvest_yield` all return `Paused` **before** any
state change or transfer. Note the precise boundary: policy setters
(`set_refund_window`, `set_yield_strategy`, `set_reserve_ratio`,
`set_max_deploy_ratio`), `pause`/`unpause`, admin transfer, and public TTL
extension remain callable while paused. The pause freezes *money movement and
yield operations*, not configuration — this is deliberate (an admin must be able
to unpause, or to fix policy, from a paused state).
*Attack:* any paused-path call that transfers tokens or changes float/refund
state. The fuzz suite asserts paused operations never mutate state.

#### I-16. Admin handover is two-step
`transfer_admin` (current admin auth) sets a pending admin; only the pending
admin can `accept_admin`; the current admin can `cancel_admin_transfer`.
There is no single-step takeover and no path where two admins hold power
simultaneously. Float, policy, and pause state are untouched by handover.
*Attack:* take over admin without the pending-accept dance, or leave two live
admins.

#### I-17. Yield accounting is self-consistent
`deployed_principal` tracks only funds moved to the strategy; `harvested_yield`
tracks yield returned to the vault; `total_value = liquid balance + deployed −
harvested` (harvested yield is the operator's, not the principal pool's).
`deploy_to_yield` enforces (a) post-deploy liquid balance ≥
`reserve_ratio` × total value, and (b) total deployed ≤ `max_deploy_ratio` ×
total value, both in basis points (ratios capped at 10,000).
`withdraw_from_yield` never removes more principal than deployed.
*Attack:* make accounting drift — e.g. a strategy that lies about
`principal_returned` / `yield_returned`, a deploy that breaches the reserve, or
double-counting harvested yield as principal. Note the reserve is **not**
enforced on `refund`/`withdraw`: an admin can refund below reserve; that is
policy, not a bug.

---

## 4. The Threat Model in One Paragraph

A merchant anchors receipt roots and holds float for policy-bounded refunds on
Stellar, using immutable contracts. The merchant admin is trusted within the
contracts' rules and is the single most powerful actor; its compromise means
total float loss but not code change or history rewrite. Everyone else is
untrusted: agents verify receipts permissionlessly, anyone can extend TTLs, and
the token contract and (optionally) a registered yield strategy are trusted
platform/partner contracts. The contracts are immutable, so the audit's job is
to confirm the invariant list above holds for the pinned artifact — because
after deployment, a bug is permanent at that address.

See `docs/SECURITY_MODEL.md` for the fuller threat model; this document is
consistent with it and adds the audit-specific detail (yield-strategy trust,
TTL-griefing cost, window-as-policy nuance).

---

## 5. Known Issues and Accepted Risks

Stated plainly, including the uncomfortable ones. None of these is hidden from
an auditor; each is a deliberate position or a documented operational property.

1. **Admin key compromise means total float loss.** A stolen merchant key can
   `withdraw` the entire float, `refund` arbitrarily (within policy and balance),
   pause, change the window/ratios/strategy, deploy or withdraw yield, and hand
   over admin. Immutability bounds this: the key cannot change code, rewrite
   roots, or double-refund. The residual control is key custody, which is
   explicitly out of scope (§1.2) and is the operator's operational risk.
2. **An immutable contract with a bug requires migration, not a patch.** There
   is no `update_current_contract_wasm`; a logic defect at a deployed address is
   permanent there. The response is the ADR-003 runbook: pause → withdraw →
   redeploy → resume. Refund tombstones and anchored roots do **not** move to the
   new instance — open-window refunds must be settled or rejected before
   cutover. This is why the audit must pin the artifact that will actually be
   deployed (§1.3).
3. **Rent exhaustion archives records.** Without TTL extension, `BatchRecord`s
   and `RefundRecord`s archive after ~30 days and reads on them fail until a
   `RestoreFootprint` restores them. Archival does **not** weaken the
   double-refund guarantee: per `docs/storage-audit.md`, the Soroban environment
   fails (rather than reports "not found") when a contract touches an archived
   refund tombstone, so the tombstone's anti-replay effect survives archival.
   Operationally, a merchant that stops extending TTLs loses live access to old
   records — an availability risk, not a soundness one.
4. **Trustline failures revert refunds/withdrawals.** Per SEP-41, a recipient
   without a token trustline makes the token transfer fail, and the vault's
   `refund`/`withdraw` revert with the token-level error. The vault deliberately
   does not pre-check trustlines (budget); this is documented operational
   behaviour, not a contract bug.
5. **The refund window is merchant-settable and merchant-suppliable.** The
   window can be changed by the admin at any time (including to `0` = no expiry),
   and `paid_at_ledger` is supplied by the merchant. The window is a policy
   control for legitimate refund discipline, not a boundary against a malicious
   merchant (who is already fully trusted with the float). See §2.
6. **The `payment_ref` ↔ leaf correspondence is off-chain.** The vault does not
   verify that a refund's `payment_ref` was ever anchored. If the indexer/SDK
   ever diverged from the leaf convention, refunds could reference non-receipts
   (or receipts could be unrefundable). Parity is pinned by vectors (§6) and
   integration tests, but the correspondence itself is a convention.
7. **A registered yield strategy is a trusted partner.** Funds deployed to a
   strategy leave the vault's direct control. The vault enforces reserve and
   deployment ratios but cannot force a strategy to return funds; a malicious or
   insolvent strategy can strand deployed principal. The strategy is also a
   re-entrancy surface (its `deposit`/`withdraw`/`harvest` run after token
   transfers and can call back into the vault). Mitigations: admin-only
   registration, ratio bounds, tests for every ratio failure mode — but the
   counterparty risk is real and should be reflected in how much float is ever
   deployed.
8. **Testnet ≠ main.** The deployed testnet addresses run `0.1.0` (no prune,
   pause, TTL extension, yield, or two-step admin; old event topics). The
   audited artifact should be the mainnet-destined `main` build, and the drift
   between the two is documented in `DEPLOYMENTS.md` / `CHANGELOG.md`.
9. **Housekeeping:** an undeclared scratch file
   (`contracts/refund-vault/src/explore.rs`) still sits in the tree. It is not
   compiled (not declared as a module) and has no effect on the build, but it
   should be deleted before the engagement so the audited tree contains no dead
   code.

---

## 6. Prior Work the Auditor Can Build On

Everything below is in-repo or pinned to the repo, so the auditor starts from
tests and measurements rather than a blank tree.

- **Test suite.** ~104 unit/integration tests across both contracts: 24
  `ReceiptAnchor` unit tests, 41 `RefundVault` unit tests, 34 yield-strategy
  tests, and 5 cross-contract integration tests
  (`contracts/refund-vault/tests/integration_test.rs`). Every enforced invariant
  in §3 maps to at least one test, and `test_snapshots/` pins golden ledger
  snapshots for the deterministic suite.
- **Fuzz properties (#58).** `contracts/*/src/fuzz_test.rs` run seeded proptest
  sequences against a model oracle and assert invariants after *every* step:
  contiguous-prefix pruning with a monotonic cursor, Merkle rejection of every
  wrong proof shape, `float == deposits − refunds − withdrawals` (never
  negative), no double refunds, no state mutation while paused, and TTL
  extension that never shortens. Failures shrink to a minimal counterexample and
  are frozen as regression tests. CI runs a bounded budget; longer profiles:
  `FUZZ_CASES=2000 FUZZ_SEQ_LEN=256 cargo test -- --ignored`.
- **Budget measurements (#52).** `docs/MAINNET_DEPLOYMENT.md` contains measured
  fee projections (`anchor_batch` ~0.02–0.05 XLM, `refund` ~0.015–0.03 XLM,
  `verify_receipt` free) and a rent model (0.5 XLM/KB/year; ~100 bytes per
  record; ~365 XLM/year to keep a year of 7,300 batch records alive). Use these
  as the cost baseline for the TTL-griefing analysis and for gas/rent bounds.
- **Cross-implementation vector parity (#53).** `contracts/receipt-anchor/src/vectors.rs`
  is generated from the TypeScript SDK's Merkle implementation in `accensa-app`
  and tested byte-identically against it, including the live testnet batch #1
  vectors (valid membership proof and forged-leaf rejection). This pins the
  sorted-pair SHA-256 convention shared across implementations.
- **Design ADRs.** `ADR-001` (Merkle structure), `ADR-002` (pruning "upto"
  scheme), `ADR-003` (upgradeability / immutability + migration runbook),
  `docs/storage-audit.md` (storage classes, TTL, archival semantics),
  `docs/EVENTS.md` (event shapes), `docs/ARCHITECTURE.md` and `docs/mechanics.mdx`
  (flow diagrams).
- **Live on-chain verification.** `DEPLOYMENTS.md` documents a read-only
  `verify_receipt` walkthrough (valid proof → `true`, forged → `false`) that runs
  for free against the deployed testnet contract.

---

## 7. Reconciliation with `SECURITY_MODEL.md`

`docs/SECURITY_MODEL.md` remains the canonical threat model. This document is
consistent with it and extends it where the audit needs more precision:

- Trust assumptions: identical (admin trusted within rules; indexer cannot
  forge; users untrusted; immutability as a control).
- **Yield strategy:** `SECURITY_MODEL.md` predates the yield integration; the
  strategy's trust boundary is added in §2 and §5 of this document. This is an
  extension, not a contradiction.
- **Paused-vault precision:** `SECURITY_MODEL.md` does not enumerate pause
  semantics; the precise boundary (fund-moving ops halted, policy setters still
  callable) is stated in [I-15](#i-15-a-paused-vault-mutates-no-fund-moving-state).
- Both documents point to each other; discrepancies, if any are found during the
  audit, are bugs in this documentation and should be fixed in the same PR as the
  finding.

---

## 8. Logistics

| Item | Detail |
|---|---|
| **Technical contact** | `security@accensa.dev` (primary); maintainers via direct message in the Stellar Developer Discord (secondary). |
| **Reporting channel for findings** | [GitHub Private Vulnerability Reporting](https://github.com/accensa/accensa-contracts/security/advisories/new) — tested end-to-end and preferred. |
| **Response SLA** | Initial triage acknowledgment within **48 hours**; progress updates every **5 business days** until resolved (`SECURITY.md`). |
| **Public findings tracking** | Findings are tracked privately via GitHub Security Advisories, then published (advisory + changelog entry) after the standard **90-day coordinated disclosure** window or a mutually agreed date. Public issue tracker is used only for post-disclosure follow-up. |
| **Engagement artifacts** | Audit scope, pinned commit + wasm hashes, and the final report should be committed to this repo (or linked from it) so future audits and the public verifier can see what was reviewed. |

**Checklist before commissioning (from the acceptance criteria):**

- [ ] Audit target commit pinned and its wasm hashes recorded (§1.3).
- [ ] This document reviewed by someone who did **not** write the contracts
      (the invariants and risks here are the thing being reviewed; an author's
      blind spots are the point).
- [ ] No placeholder tests remain (verified: #47 replaced; grep for
      `unimplemented`/placeholder in `contracts/` is clean).
- [ ] Upgradeability position fixed (verified: ADR-003 ACCEPTED).
- [ ] `docs/SECURITY_MODEL.md` and this document reconciled (§7).
