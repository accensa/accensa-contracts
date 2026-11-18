# Upgrade Decision Log

Single place to read **what was decided** about upgradeability, **what it costs**,
and **what would reopen it**. The reasoning and options live in
[`ADR-003`](ADR-003-upgradeability.md); this log is the status record and
operational summary.

## Status

| Item | Status |
|---|---|
| Decision | **Accepted** (ADR-003, Option 4: differentiated posture) |
| `ReceiptAnchor` | Immutable: no upgrade entry point, no wasm replacement |
| `RefundVault` | Admin-gated two-phase upgrade (`migrate_state` then `upgrade_wasm`), see [`REFUND_VAULT_MIGRATION.md`](REFUND_VAULT_MIGRATION.md) |
| Every other contract (`Governance`, `MultisigAccount`, `UptoAuthorization`, `CrossChain`, `StreamVault`, `Treasury`, ...) | Immutable: no upgrade entry point; a logic change is a new deployment |
| Pre-deployment checklist item "Upgradeability Decision (Issue #55)" | Closed; see [`MAINNET_DEPLOYMENT.md`](MAINNET_DEPLOYMENT.md#1-upgradeability-decision-issue-55) |

The checklist item in `MAINNET_DEPLOYMENT.md` is a resolved record, not an open
question: the decision above is final until a trigger below is met.

## Why immutable by default

- **Trust minimisation.** No admin, including a compromised key, can change the
  rules that govern user funds or an agent's receipt.
- **Audit story.** One fixed wasm per contract address, pinned to a git commit
  through the embedded `GIT_SHA` and the hashes in `DEPLOYMENTS.md`.
- **Smallest privileged surface.** No upgrade entry point to secure.
- **Why `RefundVault` is the exception.** It holds live merchant float and open
  refund records; forcing a drain-and-redeploy for every fix is itself risky.

## Consequences

- A bug in an immutable contract is permanent at that address. The remedy is to
  freeze (where the contract allows it), deploy a new instance, migrate state and
  redirect consumers. The procedure is in
  [`UPGRADE_AND_MIGRATION.md`](UPGRADE_AND_MIGRATION.md).
- A new deployment mints a new contract ID. Every consumer (indexer, dashboard,
  verifier, SDK, facilitator) must be repointed;
  [`DEPLOYMENTS.md`](../DEPLOYMENTS.md) and `deployments/<network>.env` remain the
  source of truth for IDs.
- Historical data stays on the old instance. For `ReceiptAnchor`, receipts remain
  verifiable against the predecessor forever; verification may span two IDs.
- `RefundVault` admin-key compromise can authorize a wasm upgrade in addition to
  other admin powers, so key custody (ideally the `MultisigAccount`) matters more.
- Admin transfer (`transfer_admin`/`accept_admin`) is a change of authority, not
  an upgrade.

## Triggers for reopening the decision

Reopen ADR-003 (new ADR superseding it) only if at least one holds:

1. **Recurrent cost.** Two or more redeploy-and-migrate cycles for the same
   immutable contract within a release cycle, or one that stranded user funds or
   state that could not be migrated.
2. **Multi-sig / timelock available.** A timelock or quorum-gated admin (ADR-003
   Option 3) is implemented, tested and audited, removing the single-key risk
   that justified immutability.
3. **Audit finding.** An external audit concludes the migration path for a
   contract is riskier than a gated in-place upgrade.
4. **Ecosystem change.** A Soroban platform change materially alters the
   trade-offs (for example, native upgrade governance).

## Process to reopen

1. Open an issue citing the trigger and the evidence (incident, cost, audit text).
2. Draft a superseding ADR that restates the options and names the contracts in scope.
3. Update this log's status table and the pre-deployment checklist in the same change.
4. Require maintainer and security review before any code adds an upgrade entry point.
