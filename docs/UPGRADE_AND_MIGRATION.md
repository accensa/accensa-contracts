# Upgrade and Migration Runbook (redeploy and migrate)

Soroban binds a contract ID to its uploaded wasm. Most contracts in this
repository have no upgrade entry point (see
[`UPGRADE_DECISION_LOG.md`](UPGRADE_DECISION_LOG.md)), so shipping new logic means
deploying a **new instance** and moving state to it. `RefundVault` additionally
has an in-place path ([`REFUND_VAULT_MIGRATION.md`](REFUND_VAULT_MIGRATION.md));
use this runbook when that path is unsuitable (for example after a breach, or a
storage layout the migration cannot express).

Terms: **old** = the instance being retired; **new** = its replacement.
Examples use `stellar contract invoke`; substitute `--network testnet|pubnet`,
`--source <identity>`, and IDs from [`deployments/`](../deployments)
(`testnet.env`, `pubnet.env`). Rehearse every step on testnet first.

## What each contract lets you do

| Contract | Freeze the old instance | State to carry over | Notes |
|---|---|---|---|
| `RefundVault` | `pause()` (admin); `withdraw` is blocked while paused, so unpause only long enough to withdraw | Float balance, `Policy`, refund records `Refund(payment_ref)` (tombstones), storage version | Never pay a `payment_ref` again from new if old already refunded it |
| `StreamVault` | No pause or admin. Stop creating streams off-chain; the buyer (`cancel_stream`) or anyone (`claim_stream`) settles each open stream | Open streams: `get_stream(id)`, deposit, rate, start/stop ledgers, claimed totals | Contract balance equals unclaimed principal of open streams |
| `Treasury` | No pause. Stop admin actions operationally; `claim_vested`, `claim_yield`, `unstake` stay callable by users | Vesting schedules (`get_schedule`), strategy whitelist/allocations/reserve, buyback/AMM/oracle config, distribution accumulator and stakes, token balances | Recall all strategy positions before cutover |

## Phase 1: Preparation

1. Announce the maintenance window and the new contract ID to indexer operators,
   facilitators and merchants ahead of time. Publish the old and new commit SHAs.
2. Build the new wasm from a tagged commit (`bash scripts/optimize_wasm.sh`),
   record its hash and compare against the release artifact
   ([`RELEASING.md`](RELEASING.md)).
3. Confirm the admin key (or `MultisigAccount` quorum) is available for every
   step below, including a break-glass signer.
4. Write down the rollback criteria (Phase 4) and the cutoff after which rollback
   is no longer possible (Phase 5).
5. Rehearse on testnet end to end, including the verification commands.

## Phase 2: State export (freeze, then snapshot)

1. **Freeze** the old instance as described in the table above. Record the ledger
   number of the freeze.
2. **Snapshot** read-only state to a file, once per contract:

```bash
OLD=<old-contract-id>
# Token balance held by the old instance (the number that must be conserved)
stellar contract invoke --id <token-id> --network "$NET" -- balance --id "$OLD" \
  > snapshot/old_balance.json
```

   - `RefundVault`: also `get_policy`/`get_refund_window`, `get_admin`,
     `get_storage_version`, and `get_refund <payment_ref>` for every open record
     (enumerate `payment_ref`s from `RefundEvent` / `DepositEvent` history in your
     indexer).
   - `StreamVault`: `get_merchant`, `get_token`, then `get_stream <id>` and
     `get_stream_claimable <id>` for every id from `1..=N` (ids come from
     `StreamCreatedEvent`).
   - `Treasury`: `get_admin`, `get_token`, `get_allocations`, `get_reserve_bps`,
     `get_deployed_total`, `get_yield_earned`, `get_buyback_config`, `get_amm`,
     `get_stable_token`, `get_price_feed`, `get_distribution_state`, and
     `get_schedule <beneficiary>` for each beneficiary.
3. Hash the snapshot directory (`sha256sum snapshot/*`) and have a second
   operator reproduce it from the same ledger.

## Phase 3: New deployment

1. Deploy with the project script, which verifies a clean tree and records
   provenance:

```bash
TOKEN=<token> REFUND_WINDOW_LEDGERS=<n> IDENTITY=<id> ./deploy.sh   # see MAINNET_DEPLOYMENT.md
```

   For `StreamVault` and `Treasury`, deploy with the same constructor arguments as
   the old instance (`merchant`, `token` / `admin`, `token`).
2. Re-create state through the new contract's public entry points. Nothing is
   copied storage-to-storage:
   - `RefundVault`: `deposit` the float; re-apply policy; do **not** recreate
     refund records that are already paid.
   - `StreamVault`: for each still-open stream, have the buyer `cancel_stream` on
     old (merchant receives accrued, buyer receives the remainder) and `create_stream`
     on new with the remaining parameters.
   - `Treasury`: `recall_strategy` all positions on old, move the balance, then
     `add_schedule` for each beneficiary with the unvested remainder (reduce
     `total` by what was already claimed), `whitelist_strategy`, `set_allocations`,
     `set_reserve_bps`, buyback/AMM/oracle configuration, and
     `initialize_distribution`.
3. Commit the new IDs to `deployments/<network>.env` and
   [`DEPLOYMENTS.md`](../DEPLOYMENTS.md) in a docs-only commit; keep the old IDs
   listed as retired.

## Phase 4: Verification

Run these against **new** and compare to the Phase 2 snapshot. All must pass
before redirecting traffic.

```bash
NEW=<new-contract-id>
# 1. Balance conservation: new balance == old balance at freeze (minus anything paid out in Phase 2/3)
stellar contract invoke --id <token-id> --network "$NET" -- balance --id "$NEW"
# 2. Config parity: compare getters with the snapshot
diff <(stellar contract invoke --id "$NEW" --network "$NET" -- get_admin) snapshot/admin.json
# 3. Spot-check records / streams / schedules against the snapshot
```

Pass criteria per contract:

- `RefundVault`: token balance == old float, `get_refund_window` equal,
  `get_storage_version` as expected, a sampled paid `payment_ref` is **not**
  refundable from new.
- `StreamVault`: new balance == sum of recreated deposits; `get_stream_claimable`
  for sampled ids matches the snapshot.
- `Treasury`: `get_deployed_total` is 0 or matches re-deployed allocations; for
  each beneficiary `claimable` + already claimed == original total.

**Rollback** (until the cutoff in Phase 5): leave consumers on old, unpause old if
frozen, and withdraw anything moved to new. Because old is only frozen, never
destroyed, this is a pointer change plus returning funds.

## Phase 5: Redirect and wind down

1. **Redirect facilitators and indexers.** Update the contract IDs in their
   configuration (and `accensa-app`), restart them, and confirm they subscribe to
   the documented topics ([`EVENTS.md`](EVENTS.md),
   [`EVENTS_GOVERNANCE.md`](EVENTS_GOVERNANCE.md)) on the new ID. Keep indexing the
   old ID for historical queries and late claims.
2. **Open the new instance** to production traffic. Watch events and balances for
   one full refund window / stream period.
3. **Wind down old.** Confirm zero token balance and no open obligations
   (every refund record settled or migrated; streams closed; vesting migrated).
   Leave the old contract deployed and readable so receipts, refund records and
   history stay auditable. Keep paying rent on anything you still need to read
   ([`storage-audit.md`](storage-audit.md)).
4. **Close out.** Mark the old ID retired in `DEPLOYMENTS.md`, post the completion
   notice, and archive the snapshot with its checksum.
   Rollback is no longer supported after this step.

## Checklist

- [ ] Communications sent; new commit and wasm hash published
- [ ] Old instance frozen; freeze ledger recorded
- [ ] Snapshot exported, hashed, independently reproduced
- [ ] New instance deployed; IDs recorded in `deployments/`
- [ ] State re-created; balance and config parity verified
- [ ] Facilitators and indexers redirected and confirmed
- [ ] Old instance at zero balance, retired in `DEPLOYMENTS.md`
