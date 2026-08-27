# ADR 003: Upgradeability Policy for ReceiptAnchor and RefundVault

> **Status: ACCEPTED**

## Context

Neither `ReceiptAnchor` nor `RefundVault` exposes an upgrade path today. There is
no `upgrade()` function, no admin wasm-swap via `update_current_contract_wasm`, and
no ADR stating that immutability is a deliberate choice rather than something
nobody got to. This ADR records the position explicitly so that a reader — and an
auditor — can tell immutability is a security property, not an omission.

The decision is load-bearing because each contract holds a different kind of
irreplaceable state, and each is wrong in a different way:

**`RefundVault` holds merchant float.** A bug in refund accounting in an immutable
contract means the merchant's remedy is withdraw-and-redeploy, and every open
refund record (`Refund(BytesN<32>)` in persistent storage) is stranded in the old
contract if the merchant moves on. That is survivable if planned for in advance,
and messy if discovered during an incident. `RefundVault` also already carries two
governance mechanisms this ADR must interact with: a **pause/unpause** emergency
stop and a **two-step admin transfer** (`transfer_admin` / `accept_admin` /
`cancel_admin_transfer`). The `DataKey` enum further reserves `Admins` and
`Threshold`, intended for a future multi-sig or quorum admin (tracked separately as
issue #23), but **no multi-sig logic is implemented today** — those keys are inert.

**`ReceiptAnchor` holds the anchored Merkle roots the whole verifiability claim
rests on.** Migration means either re-anchoring history or accepting that
verification spans two contract IDs — which changes the story an agent is told
about how to verify a receipt, because the agent currently verifies against a
single well-known contract ID published in `deployments/<network>.env`.

Soroban makes upgradeability available: `update_current_contract_wasm` (invoked via
the deployer environment) can swap a contract's wasm in place, keeping the contract
ID. So a technical constraint is not the reason to stay immutable. The
reason is a trust-model trade-off, costed below for each option.

## Options

The four candidate designs, each honestly costed. In every upgradeable variant the
authorisation path is the operative question, because that is what determines what
the upgrade *means* to the counterparties who are not the admin.

### Option 1 — Immutable, with a documented migration procedure

The contracts ship with no upgrade function and never will. State is fixed at the
deployed address; the only way to ship new logic is a new contract ID.

**Costs**

- **Bugs are permanent at an address.** A refund-accounting bug in `RefundVault`
  cannot be patched in place. The remedy is: pause, withdraw all float,
  strand the refund tombstones, redeploy, resume. A privilege-escalation or
  fund-draining bug in `RefundVault` means trust in the specific deployment is
  forfeit before funds are recovered.
- **No in-place recovery from a compromised or lost admin key.** If the admin key is
  rotated away the two-step transfer covers admin handover functionally (migration
  for the *operator*), but any logic defect remains.
- **Migration always costs a contract ID change**, and a new contract ID must be
  propagated to every downstream consumer (indexer, dashboard, public verifier,
  agent-facing SDK) before state-bearing calls can move over.
- **The immutability promise must be proven, not assumed.** Soroban upgrades
  require an explicit function that calls `update_current_contract_wasm`; their
  absence is verifiable by anyone reading the wasm hash recorded in
  `DEPLOYMENTS.md`. Absence is the guarantee, and it holds for the life of the
  deployment as long as the operator never ships one.

**Benefits**

- **The strongest trust property available on a permissioned contract.** No admin —
  not the operator, not a compromised key — can change the rules under a
  merchant's float or under an agent's receipt. This is precisely the claim the
  project makes in the README: "the policy lives in the contract rather than in a
  support inbox." Immutability is what makes that sentence true instead of aspirational.
- **A clean, inexpensive audit story.** An auditor reasons about one fixed wasm per
  address, and can pin the deployed behaviour to a git commit via the embedded
  `GIT_SHA`. There is no "what could the admin have changed it to" ground state.
- **Simplest surface area.** No upgrade entry point means one less privilege to
  secure and one less interaction with the reserved multi-sig keys.

### Option 2 — Upgradeable behind the existing merchant admin

Add `upgrade(new_wasm)` callable by the merchant admin via `require_auth`, invoking
`update_current_contract_wasm`.

**Costs**

- **Directly contradicts the core product claim.** With a single merchant key able
  to swap wasm at will, "the policy lives in the contract" becomes "the policy lives
  wherever the merchant's key says it is." An auditor would flag that the
  `RefundVault` float — which the README pitches as *not* custodied by anyone who can
  change the rules — is in fact governed by a single hot admin key. This is the
  single worst-tradeoff option for `RefundVault`.
- **Misfit for `ReceiptAnchor`.** The value of a receipt anchor is that its roots are
  independently verifiable and *immovable*. A merchant-side upgrade that could
  rewrite how roots verify (or replace the tree) undermines verifiability of past
  and future receipts alike at one address.
- **Incident handling quality degrades**: a merchant who can patch away would
  reason "we'll fix it in an upgrade", deferring the runbook discipline that
  Option 1 forces.

**Benefits**

- **Fastest, in-place response to a bug or to new policy features** at the same
  contract ID, with no downstream propagation of a new address.
- **Cheapest operational path** for the merchant (no migrate-and-redeploy runbook).

### Option 3 — Upgradeable behind a timelock and/or the multi-sig admin (issue #23)

Require a timelock delay and/or a quorum of the multi-sig admin before an upgrade
commits, so no single key can change behaviour instantly.

**Costs**

- **The multi-sig admin does not exist yet.** `Admins` / `Threshold` are reserved
  `DataKey` variants with no enforcing logic; issue #23 is open. Choosing this
  option makes the upgrade path *depend on* work that is not shipped, and would
  ship an upgrade function before the guardrail that is supposed to make it safe.
- **A timelock changes the emergency calculus, not the trust model.** A delay makes a
  *compromised* key's upgrade recoverable (there is time to respond), but it does
  not change *who can* upgrade — it is still the merchant/operator collectively.
  The "admin can change the rules under a merchant's float" objection survives the
  timelock, just tooled slower.
- **Two more moving parts to secure and test** (timer state, quorum counting), and
  the timelock only has force if the deployed authority set is genuinely distributed
  and the keys held by distinct parties.

**Benefits**

- **Best safety if upgradeability is unavoidable.** A timelock gives incident
  responders a window to veto a silent rogue upgrade; multi-sig gives an adversary a
  harder target than a single key. This is the honest baseline for any organisation
  that concludes it needs in-place upgrades.
- **Keeps the contract ID stable** across upgrades (same benefit as Option 2).

### Option 4 — Split: immutable `ReceiptAnchor`, upgradeable `RefundVault`

Give the vault a (governed) upgrade path while keeping the anchor fixed.

**Costs**

- **Costs the worst of both worlds on the wrong axis.** The candidate for
  upgradeability is exactly the contract holding merchant float — the one where an
  admin can change policy under a custodian-like relationship the project denies
  having. And the candidate for immutability is the contract whose roots move to a
  new ID anyway on any future change, re-deriving the two-contract-ID verification
  problem on the more consequential side.
- **Two divergent policies to document and defend.** The security story becomes "the
  anchor is immutable, the float is mutable," which read together is weaker than
  either alone: the agent's receipt story is stable only until an anchor change, and
  the merchant's float story is mutable exactly where it hurts.
- **Marginal benefit is low**: most worthwhile fixes span correctness/policy in
  the vault, but the vault is also the most dangerous thing to patch in place.

**Benefits**

- **Middle ground for organisations that want to accelerate low-risk vault
  features** (new policy knobs) at the same ID while keeping receipt verifiability
  pinned.
- **Confines upgradeable surface area** to the vault rather than both contracts.

## Decision

**Status: ACCEPTED — both contracts are immutable, with a written migration runbook
(as required for this choice).**

We select **Option 1**. Immutability is deliberate for both contracts. `RefundVault`
and `ReceiptAnchor` are released without an upgrade entry point, and the CI/build
policy forbids adding one in the future.

**Reasons**

- The project's differentiating claim is that policy and verifiability live on-chain
  rather than in a support inbox you can talk a merchant into changing. That claim is
  only trustworthy if *nobody* — including the operator — can mutate the rules.
  Option 1 is the only option that makes the README's pitch true rather than
  decorated.
- The deployed addresses are live infrastructure with downstream consumers keyed to
  a single well-known ID (the public verifier reads `ReceiptAnchor` by address). A
  *stable* ID is not worth renting: stability is only valuable when behaviour is also
  stable. An upgraded ID is a promise to re-read; `DEPLOYMENTS.md` already
  catalogues the real cost of that (v0.1.0 vs v0.2.0 at the same addresses).
- Option 2 is rejected because a single-hot-key upgrade entry point is the exact
  upgrade "power" the project must not grant, and because the merchant admin is
  already the strongest actor in the system — it does not need the additional
  authority to rewrite wasm.
- Option 3 is rejected *today* as a dependency: the multi-sig it would hang on
  (issue #23) is not implemented, and choosing it would couple a security
  decision to unshipped guardrails. If the project later ships multi-sig and
  concludes in-place upgrades are necessary, re-open this ADR rather than patch
  it incrementally.
- Option 4 is rejected because it mutates the contract that should be least
  mutable (float) and pins the contract whose future changes are most disruptive
  (roots), inverting the sensible split.

**Auditor-facing security implications, stated explicitly**

- **Immutability as a control:** the absence of `update_current_contract_wasm`
  inside both wasm artifacts is itself the anti-tampering control. There is no admin
  action — at rest or compromised — that can alter behaviour. The residual risk is
  confined to (a) a bug in a *deployed* wasm, and (b) operator mistakes in
  *deploying* a new instance. Both are addressed by the migration runbook, not by
  in-place repair.
- **Compromised keys cannot change policy.** A stolen merchant key can drain float
  or pause/unpause within the limits the contract already encodes, but it cannot
  install new rules. This bounds the blast radius of key compromise to the existing
  authorisation surface.
- **Bug disclosure has a defined, repeatable response:** pause, withdraw,
  redeploy, resume. Because immutability is now documented rather than assumed,
  incident responders are not improvising a migration mid-outage; they are executing
  a rehearsed runbook whose edge cases (§Migration runbook) are already costed.

## Consequences

### Security properties preserved

- **`RefundVault` float is drift-free:** the refund policy (window, no-double-refund,
  float-bound, merchant-only auth, pause) is fixed at the address. No admin can
  re-open a refund window retroactively or waive the double-refund guard.
- **`ReceiptAnchor` roots are immovable:** `verify_receipt` resolves only against the
  roots genuinely anchored at the deployment address. Past and future receipts share
  one verification story.
- **The attack surface is reduced by one entire privilege class**: there is no
  upgrade entry point to defend, audit, or falsely trust.

### Operational consequences (what changed)

- **This ADR converts the immutability from an omission into a choice**, and
  `docs/SECURITY_MODEL.md` and `README.md` are updated to state the position
  explicitly (see the final section).
- Any future logic change is a **new contract ID**, and the migration runbook below
  is the enforced procedure.
- `DEPLOYMENTS.md` and `deployments/<network>.env` remain the single source of truth
  for contract IDs; a migration changes both plus everything downstream.

## Migration Runbook (required by the immutable decision)

Both migrations share one trigger and one invariant: **float/roots move only after
the new contract ID is live, verifiable, and announced; nothing state-bearing is
left behind knowingly.**

### Common to both contracts

1. **Freeze writes first.** For `RefundVault`, call `pause()` and confirm `Paused`
   applies to deposit/refund/withdraw/yield. For `ReceiptAnchor`, stop the indexer
   from calling `anchor_batch` (the anchor has no pause; writes are blocked
   operationally).
2. **Build and record the new wasm.** Build from a tagged commit, capture the
   `sha256sum` and `GIT_SHA`, and deploy the *new instance*. Record the new contract
   ID in `deployments/<network>.env` alongside the retained `NEXT_PUBLIC_*_ID` of
   the old instance.
3. **Copy across any needed policy state by *reinitialising*, not by reading
   storage.** Instance policy (token, refund window, reserve ratios) is re-supplied
   at `initialize`; it is not copied as persistent records.
4. **Propagate the new ID.** Update the indexer, dashboard, public verifier, and
   agent-facing SDK to the new ID. Because the old and new instances coexist
   during cutover, downstream readers must key reads by the ID that emitted the
   event they are replaying.
5. **Verify before resuming.** Run the read-only verification from `DEPLOYMENTS.md`
   against the new instance for a sample of prior anchors/refunds before lifting the
   freeze.
6. **Retain the old instance** at least until its event horizon passes (unplayed
   events, in-flight refunds, outstanding verification requests). Then retire it —
   for the vault this is where the old refund tombstones become unreachable-through-
   the-new-volume, and for the anchor where old roots stop being served. Do **not**
   delete the old instance's wasm-hash record; `DEPLOYMENTS.md` history is the audit
   trail.

### `RefundVault` — moving merchant float

- **Settle or strand refund records before moving.** A refund record keyed by
  `payment_ref` cannot be carried onto the new instance (persistent storage is
  instance-bound). Decide one of two paths *before* redeploying:
  - **Path A (chosen):** wait out or reject every open window, so no refund is still
    outstanding; withdraw the float; treat the old refund tombstones as exactly
    equivalent — refunds already executed stay refunded (no replay on the new
    instance because the merchant simply does not re-refund).
  - **Path B (plain replay) — rejected:** if any outstanding refunds remain, the old
    tombstones do *not* move, so a re-refund after cutover would technically be
    possible on the new instance. This is why the runbook freezes before withdrawing:
    do not cut over with open windows.
- **Merchant float:** withdraw (`withdraw(amount, to)`) to the merchant before the
  old instance is retired, then `deposit(from, amount)` into the new instance.
  Never attempt to transfer float directly between instances.
- **No replay risk** once Path A is followed: every already-executed refund is
  settled on the old instance and never re-run on the new one, and new refunds are
  keyed to payments `paid_at_ledger` inside the new window.

### `ReceiptAnchor` — anchored history

- **Re-anchor, do not copy.** If the anchor is upgraded, prior batches are not
  carried over. Either (a) re-anchor the historical roots into the new instance as a
  new batch (preserving the verifiable chain at the *new* ID), or (b) accept that
  verification spans two contract IDs and state that explicitly.
- **Choose and document one verification story.** Under (a), agents always verify
  against the new ID once read-through is complete; under (b), an agent must know
  *which* ID holds a given `batch_id`. The dashboard and verifier must announce which
  story applies so a verifying agent is not told the wrong contract ID.
- **Pruning/archival interplay:** `prune_batches` and TTL archival already mean some
  old roots are only restorable, not live. A migration does not make this worse; it
  merely changes *which* instance the restore targets.

## Interaction with existing governance

- **Two-step admin transfer** (`transfer_admin`/`accept_admin`/`cancel_admin_transfer`)
  in `RefundVault` continues to cover *operator* handover. It is a functional
  migration for who holds the keys; it is **not** an upgrade and does not change
  code. The two must never be conflated in public documentation.
- **Pause** remains the primary emergency mitigation for the immutable vault:
  `pause()` halts deposit/refund/withdraw/yield operations and is the first step of
  the runbook. `ReceiptAnchor` has no pause; its write-freeze is operational
  (stop the indexer).
- **Multi-sig (issue #23):** the reserved `Admins`/`Threshold` keys are inert today.
  This ADR does not depend on them. If multi-sig ships, it governs the *authorised
  actor set* (who may move/withdraw/initialize) — not code updates, since there are
  none. If the project later wants in-place upgrades, multi-sig plus a timelock
  (Option 3) is the acceptable shape and should reopen this ADR, not extend it.

## Reconciliation with `SECURITY_MODEL.md` and `README.md`

- `docs/SECURITY_MODEL.md` gains an explicit **Immutability** subsection: both
  contracts are immutable; there is no upgrade entry point; admin key compromise
  cannot change behaviour; bug disclosure follows the migration runbook.
- `README.md` is updated so the "policy lives in the contract rather than in a
  support inbox" claim names the mechanism behind it (immutability, no
  `update_current_contract_wasm`), turning an aspiration into a stated property.

## References

- `docs/ADR-001-merkle-structure.md`, `docs/ADR-002-upto-scheme.md` — prior ADR format
- `docs/SECURITY_MODEL.md` — threat model this ADR amends
- `docs/storage-audit.md` — persistent-state classification (float, refund tombstones,
  batch roots) that drives the runbook
- `docs/MAINNET_DEPLOYMENT.md`, `docs/RELEASING.md`, `DEPLOYMENTS.md`,
  `deployments/testnet.env` — deployment, ID distribution, and the wasm/GIT_SHA record
- Soroban `update_current_contract_wasm` — the mechanism deliberately not shipped