# ADR 003: Upgradeability and Immutability Posture

## Context
There has been discussion regarding the upgrade path for `ReceiptAnchor` and `RefundVault`. In the initial design, both contracts were intended to be strictly immutable without exposing any upgrade functionality.

In the current implementation:
- `ReceiptAnchor` remains **strictly immutable**: it exposes no upgrade entry points and makes no calls to `env.deployer().update_current_contract_wasm()`.
- `RefundVault` implements an **admin-gated upgrade path**: it exposes a two-phase `migrate_state(target_version)` and `upgrade_wasm(wasm_hash)` mechanism (calling `env.deployer().update_current_contract_wasm(wasm_hash)`), gated by `admin.require_auth()`, as documented in [`docs/REFUND_VAULT_MIGRATION.md`](REFUND_VAULT_MIGRATION.md).

## Decision
We maintain a differentiated posture tailored to each contract's role:
1. **`ReceiptAnchor` remains immutable:** Anchored Merkle roots and payment receipts serve as permanent, verifiable cryptographic proofs. Immutability ensures that receipt verification semantics cannot be altered post-deployment.
2. **`RefundVault` supports admin-controlled in-place upgrades:** Because `RefundVault` manages live merchant float and complex refund policies, it incorporates an intentional, admin-authorized two-phase migration and WASM upgrade path (`migrate_state` followed by `upgrade_wasm`, documented in [`docs/REFUND_VAULT_MIGRATION.md`](REFUND_VAULT_MIGRATION.md)).

### Reasoning
1. **Trust Minimization for Receipts:** For `ReceiptAnchor`, remaining immutable provides a guarantee to users, indexers, and auditors that anchored payment proofs cannot be modified or reinterpreted by code replacement.
2. **Operational Continuity for Vault Float:** For `RefundVault`, requiring a full redeployment and manual capital transfer for every logic fix or policy adjustment carries operational risks and capital disruption. The two-phase upgrade flow decouples storage layout migration from WASM replacement, while strictly gating both steps behind `admin.require_auth()`.
3. **Auditability and Safety Controls:** The `RefundVault` contract enforces monotonic storage versioning (`migrate_state`) and admin authentication on-chain, while the migration runbook prescribes operator-level safety steps (pausing vault activity, taking state snapshots, and validating invariants before invoking `upgrade_wasm`) to prevent reentrancy and unexpected execution transitions during upgrade.

## Migration Strategy
- **For `ReceiptAnchor`:** In the event of required logic changes or critical defects, the migration path remains **Redeployment and Re-anchoring**: a new contract is deployed, integrators update their contract address, and historical anchored batches remain permanently verifiable on the immutable predecessor.
- **For `RefundVault`:** Upgrades proceed via the two-phase in-place migration runbook detailed in [`docs/REFUND_VAULT_MIGRATION.md`](REFUND_VAULT_MIGRATION.md):
  1. Upload new WASM to the network.
  2. Pause vault activity and snapshot state.
  3. Execute `migrate_state(target_version)` with admin authorization.
  4. Validate state invariants, policy, and float conservation.
  5. Execute `upgrade_wasm(wasm_hash)` with admin authorization.
  6. Verify post-upgrade state and unpause.
  If an upgrade is unsuitable or a catastrophic breach occurs, the fallback is pause, float withdrawal, and redeployment.