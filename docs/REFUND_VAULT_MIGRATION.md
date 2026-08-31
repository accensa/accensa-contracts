# RefundVault state migration

RefundVault upgrades use a two-phase process so a storage migration is
auditable and survives the WASM handoff:

1. Deploy the new WASM and upload it to the network.
2. Pause refund activity and snapshot the vault state.
3. Call `migrate_state(target_version)` as the current admin. The target must
   be greater than the stored version; legacy deployments without a marker are
   treated as version 1.
4. Validate policy, token, refund records, and vault balance conservation.
5. Call `upgrade_wasm(wasm_hash)` as the admin. The hash must identify the
   previously uploaded WASM.
6. Verify `get_storage_version`, policy reads, and representative legacy
   refunds after the upgrade before resuming activity.

The version marker is monotonic and the migration call is idempotency-safe:
repeating the same target fails instead of silently claiming a second
migration. New `DataKey` variants must be appended, and changes to encoded
`Policy`/record fields require a version-aware decoder. Migration tooling must
be bounded and resumable for large persistent-record sets; never delete a
legacy refund record until its replacement has been validated.

Both entry points require the current admin address to authenticate. A
separate `upgrade_wasm` call is intentional: Soroban may transfer execution to
the new WASM during the deployer call, so post-upgrade bookkeeping must not be
relied on in the same invocation.
