# ADR 003: Upgradeability and Immutability Posture

> **Status: ACCEPTED**

## Context

The initial design intended `ReceiptAnchor` and `RefundVault` to be strictly
immutable, with no upgrade entry point and no call to
`update_current_contract_wasm`. That position was recorded so a reader and an
auditor could tell immutability was a deliberate security property rather than an
omission.

The current implementation has evolved to a **differentiated posture**:

- `ReceiptAnchor` remains **strictly immutable**: it exposes no upgrade entry
  points and makes no calls to `env.deployer().update_current_contract_wasm()`.
- `RefundVault` implements an **admin-gated upgrade path**: it exposes a two-phase
  `migrate_state(target_version)` and `upgrade_wasm(wasm_hash)` mechanism (calling
  `env.deployer().update_current_contract_wasm(wasm_hash)`), gated by
  `admin.require_auth()`, as documented in
  [`docs/REFUND_VAULT_MIGRATION.md`](REFUND_VAULT_MIGRATION.md).

The change is load-bearing because each contract holds a different kind of
irreplaceable state, and each fails in a different way:

- **`RefundVault` holds merchant float.** Requiring a full redeployment and manual
  capital transfer for every logic fix strands open refund records
  (`Refund(BytesN<32>)` in persistent storage) on the old instance and carries real
  operational risk during an incident. The vault also already carries a
  **pause/unpause** emergency stop and a **two-step admin transfer**
  (`transfer_admin` / `accept_admin` / `cancel_admin_transfer`).
- **`ReceiptAnchor` holds the anchored Merkle roots the verifiability claim rests
  on.** Migration means either re-anchoring history or accepting that verification
  spans two contract IDs, which changes the story an agent is told about how to
  verify a receipt against the single well-known ID published in
  `deployments/<network>.env`.

Soroban makes in-place upgrades available through
`update_current_contract_wasm`, so the question is not technical feasibility but
the trust-model trade-off for each contract.

## Options Considered

### Option 1 - Fully immutable, with a documented migration procedure

Both contracts ship with no upgrade function; the only way to ship new logic is a
new contract ID.

**Benefits**

- Strongest trust property: no admin, including a compromised key, can change the
  rules under merchant float or an agent's receipt.
- A clean audit story: one fixed wasm per address, pinnable to a git commit via the
  embedded `GIT_SHA`.
- Smallest privileged surface: no upgrade entry point to secure.

**Costs**

- Bugs are permanent at an address. The only remedy is pause, withdraw, redeploy,
  and re-anchor, and open refund tombstones are stranded.
- Every logic change costs a contract ID change that must be propagated to every
  downstream consumer (indexer, dashboard, verifier, agent SDK).
- An immutable vault forgoes low-risk in-place fixes for live merchant float,
  pushing operators toward risky ad-hoc migrations during an incident.

### Option 2 - Upgradeable behind the merchant admin for both contracts

Add `upgrade(new_wasm)` callable by the merchant admin via `require_auth`.

**Benefits**

- Fastest in-place response to a bug or new policy feature at the same ID.
- Cheapest operational path for the merchant.

**Costs**

- For `ReceiptAnchor`, an admin who can swap wasm could rewrite how roots verify,
  undermining verifiability of past and future receipts at one address.
- For `RefundVault`, a single hot key able to change policy weakens the claim that
  the policy lives in the contract rather than wherever the admin key points.
- Incident discipline degrades if "we'll fix it in an upgrade" replaces a rehearsed
  runbook.

### Option 3 - Upgradeable behind a timelock and/or multi-sig admin

Require a timelock delay and/or a quorum before an upgrade commits.

**Benefits**

- Gives responders a veto window against a silent rogue upgrade and makes the
  authority a harder target than a single key.
- Keeps the contract ID stable.

**Costs**

- A timelock changes the emergency calculus, not who can upgrade.
- Adds timer and quorum state to secure and test.
- Depends on multi-sig, which is not implemented today (the reserved
  `Admins`/`Threshold` keys are inert; tracked separately as issue #23).

### Option 4 - Differentiated posture: immutable anchor, admin-upgradeable vault

Keep `ReceiptAnchor` fixed and give `RefundVault` an explicitly gated upgrade path.

**Benefits**

- Preserves a single, permanent verification story for receipts.
- Allows operational continuity for live merchant float without an ad-hoc
  redeploy-and-transfer during an incident.
- Confines the upgradeable surface to the contract whose bugs are otherwise most
  expensive to recover from.

**Costs**

- Two postures to document, defend, and audit.
- The upgradeable contract is the one holding merchant float, so admin key custody
  becomes even more important. This is mitigated by requiring `admin.require_auth()`
  for both phases and by the migration runbook.

## Decision

**We adopt Option 4: a differentiated posture.**

1. **`ReceiptAnchor` remains immutable.** Anchored Merkle roots and payment receipts
   serve as permanent, verifiable cryptographic proofs. Immutability ensures receipt
   verification semantics cannot be altered post-deployment, and verification stays
   pinned to one well-known contract ID.
2. **`RefundVault` supports admin-controlled in-place upgrades.** Because the vault
   manages live merchant float and complex refund policies, it exposes a two-phase
   `migrate_state` followed by `upgrade_wasm` flow, documented in
   [`docs/REFUND_VAULT_MIGRATION.md`](REFUND_VAULT_MIGRATION.md).

### Reasoning

1. **Trust minimisation for receipts:** keeping `ReceiptAnchor` immutable guarantees
   to users, indexers, and auditors that anchored proofs cannot be modified or
   reinterpreted by code replacement.
2. **Operational continuity for vault float:** a full redeployment for every logic
   fix or policy adjustment carries operational risk and capital disruption. The
   two-phase flow decouples storage-layout migration from wasm replacement while
   gating both steps behind `admin.require_auth()`.
3. **Auditability and safety controls:** `RefundVault` enforces monotonic storage
   versioning in `migrate_state` and admin authentication on-chain, while the
   migration runbook prescribes operator-level safety steps (pause, snapshots,
   invariant checks before `upgrade_wasm`) to prevent unexpected transitions.
4. **Option 2 was rejected** because a single hot key able to rewrite anchor wasm
   would directly undermine receipt verifiability, and because it grants the vault
   admin more power than the existing authorisation surface requires.
5. **Option 3 remains the acceptable shape if in-place upgrades ever need a
   timelock or quorum.** It is deferred until multi-sig ships (issue #23); the
   current two-phase admin flow does not depend on it.

## Migration Strategy

- **For `ReceiptAnchor`:** the migration path is **redeployment and re-anchoring**.
  A new contract is deployed, integrators update their contract address, and
  historical anchored batches remain permanently verifiable on the immutable
  predecessor.
- **For `RefundVault`:** upgrades proceed via the two-phase in-place runbook in
  [`docs/REFUND_VAULT_MIGRATION.md`](REFUND_VAULT_MIGRATION.md):
  1. Upload the new wasm to the network.
  2. Pause vault activity and snapshot state.
  3. Execute `migrate_state(target_version)` with admin authorization.
  4. Validate state invariants, policy, and float conservation.
  5. Execute `upgrade_wasm(wasm_hash)` with admin authorization.
  6. Verify post-upgrade state and unpause.

  If an upgrade is unsuitable or a catastrophic breach occurs, the fallback is a
  scheduled drain: pause activity, unpause only long enough for the admin to
  withdraw the float (`withdraw` is itself blocked while paused), then redeploy.
  The old instance stays readable so its refund records remain auditable, and a
  refund already paid from it must never be paid again from the replacement.

## Consequences

### Security properties preserved

- **`ReceiptAnchor` roots are immovable:** `verify_receipt` resolves only against
  the roots genuinely anchored at the deployment address. Past and future receipts
  share one verification story, and a logic change requires a new anchor instance.
  The claim is checkable, not asserted: the wasm artifact pinned in
  `DEPLOYMENTS.md` and the source commit embedded as `GIT_SHA` must contain no
  `update_current_contract_wasm` call and no upgrade entry point.
- **`RefundVault` upgrades are explicit and authenticated:** both `migrate_state`
  and `upgrade_wasm` strictly require `admin.require_auth()`. There is no
  unauthenticated upgrade path.
- **Compromised admin keys have a bounded but larger blast radius on the vault:**
  a stolen admin key can authorize a wasm upgrade in addition to standard admin
  powers (withdrawing float, pausing, configuring policy and yield). This amplifies
  the importance of admin key custody, multi-sig administration, and cold storage
  governance.

### Operational consequences

- **`ReceiptAnchor` bugs are permanent at a deployed address:** the response is
  redeploy and re-anchor, not an in-place patch.
- **`RefundVault` in-place upgrades must preserve integrity:** operators must follow
  the two-phase runbook, pause activity, advance storage versioning monotonically,
  validate invariants, and verify balances before unpausing.
- **Downstream consumers must track the anchor ID:** because the anchor cannot be
  upgraded in place, `DEPLOYMENTS.md` and `deployments/<network>.env` remain the
  single source of truth for contract IDs.

## Interaction with Existing Governance

- **Two-step admin transfer** (`transfer_admin` / `accept_admin` /
  `cancel_admin_transfer`) in `RefundVault` covers operator handover. It is a
  transfer of authority, **not** an upgrade, and does not change code. The two must
  never be conflated in public documentation.
- **Pause** remains the primary emergency mitigation for the vault: `pause()` halts
  deposit/refund/withdraw/yield operations and is the first step of the migration
  runbook. `ReceiptAnchor` has no pause; its write-freeze is operational (stop the
  indexer).
- **Multi-sig (issue #23):** the reserved `Admins`/`Threshold` keys are inert today.
  Neither the current posture nor this ADR depends on them. If multi-sig ships, it
  governs the authorised actor set (who may move/withdraw/initialize/upgrade); if a
  timelock is then required, Option 3 is the shape to adopt.

## Reconciliation with `SECURITY_MODEL.md` and `README.md`

- `docs/SECURITY_MODEL.md` gains an explicit **Immutability and Upgradeability
  Posture** subsection: the anchor is immutable, the vault is admin-upgradeable via
  the two-phase flow, admin key compromise can authorize upgrades, and bug
  disclosure follows the documented migration runbook.
- `README.md` is updated so the "policy lives in the contract rather than in a
  support inbox" claim names the mechanism behind it: `ReceiptAnchor` has no
  `update_current_contract_wasm`, while `RefundVault` upgrades are explicitly
  admin-gated and runbook-driven.

## References

- `docs/ADR-001-merkle-structure.md`, `docs/ADR-002-upto-scheme.md` - prior ADR format
- `docs/SECURITY_MODEL.md` - threat model this ADR amends
- [`docs/REFUND_VAULT_MIGRATION.md`](REFUND_VAULT_MIGRATION.md) - the vault upgrade runbook
- `docs/storage-audit.md` - persistent-state classification (float, refund tombstones,
  batch roots) that drives the runbook
- `docs/MAINNET_DEPLOYMENT.md`, `docs/RELEASING.md`, `DEPLOYMENTS.md`,
  `deployments/testnet.env` - deployment, ID distribution, and wasm/GIT_SHA record
- Soroban `update_current_contract_wasm` - the mechanism used by `RefundVault` and
  deliberately absent from `ReceiptAnchor`
