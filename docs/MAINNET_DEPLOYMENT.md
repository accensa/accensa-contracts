# Mainnet Deployment Guide

Deploying `accensa-contracts` to Stellar Mainnet (pubnet) requires deliberate
preparation.  This document covers every decision and action required before,
during, and after a pubnet deployment.

> **No pubnet deployment has been performed yet.** The values in
> [`deployments/pubnet.env`](../deployments/pubnet.env) are placeholders.
> [`deploy.sh`](../deploy.sh) will not deploy to pubnet unless explicitly
> instructed with `--network pubnet` and confirmed interactively.

---

## Pre-Deployment Checklist

Every item in this section **must be completed before running `deploy.sh`**.
The deployment will fail with `set -euo pipefail` if any required configuration
is missing.

### 1. Upgradeability Decision (Issue #55)

Soroban contracts are **not upgradeable** by default.  Once deployed, a contract
ID is bound to its uploaded WASM.  A new deployment mints a new contract ID.

- [ ] Confirm whether the contracts will be deployed as immutable or whether a
      upgradeability mechanism (e.g., a router proxy, a WASM replacement via the
      Stellar `upgrade` facility, or a key rotation) is in scope.
- [ ] If upgradeability is desired, document the mechanism and ensure it is
      implemented and tested before deployment.
- [ ] If the contracts are immutable, acknowledge that any future bug fix or
      feature addition requires a new deployment and coordinated migration.

**Status:** Open — tracked in
[#55](https://github.com/accensa/accensa-contracts/issues/55).

### 2. Audit Position (Issue #60)

The smart contracts are currently **unaudited** (see [`SECURITY.md`](../SECURITY.md)).

- [ ] Determine whether an external audit is required before pubnet deployment.
- [ ] If an audit is commissioned, record the audit firm, report URL, and the
      commit SHA that was audited.
- [ ] If proceeding without an audit, document the risk acceptance and the
      rationale (e.g., limited blast radius, testnet validation period).

**Status:** Open — tracked in
[#60](https://github.com/accensa/accensa-contracts/issues/60).

### 3. Admin Key Custody and Multisig

The admin (merchant) key is the single point of trust for `ReceiptAnchor` and
`RefundVault`.  A compromised key allows an attacker to drain vault float,
pause operations, or prune receipt batches.

- [ ] Decide whether the admin key will be a single ed25519 keypair or a
      multisig / smart-account signer from day one.
- [ ] If using multisig: configure the signer set, thresholds, and recovery
      procedure.  Document the signer addresses.
- [ ] If using a single key: document the key custody procedure (HSM, air-gapped
      machine, key sharding, etc.).
- [ ] Ensure the deployer identity used by `deploy.sh` has the correct key
      material available on the deployment machine.
- [ ] Verify that the admin key can sign Soroban `invokeAuth` transactions by
      performing a dry-run on testnet with the production key.

### 4. USDC Stellar Asset Contract (SAC) Address

`RefundVault` settles refunds in USDC via the Stellar Asset Contract.  The SAC
address is network-specific and **must be verified** against the authoritative
source.

- [ ] Look up the current USDC SAC address on Stellar Mainnet from the
      [Stellar USDC issuer account](https://stellar.expert/explorer/public/asset/USDC-GA5ZSEJYB37JDE5B6L17IAZEMAZ2Z2KSS6Y72Y2E5M4NOBYPCU6U5AIN)
      or the [Circle / Stellar documentation](https://www.stellar.org/developers/guides/issuing-assets.html).
- [ ] Record the verified address here:

      **Mainnet USDC SAC address:** `______________________________`

- [ ] Pass this address as the `TOKEN` environment variable during deployment:

      ```bash
      NETWORK=pubnet TOKEN=<verified-usdc-sac-address> ./deploy.sh
      ```

- [ ] After deployment, verify the vault was initialized with the correct token
      by reading back the contract state.

> ⚠️ **Never guess or copy a token address from a testnet deployment.**
> Testnet and mainnet SAC addresses are different.  Deploying with the wrong
> token address means refunds will attempt cross-asset transfers and fail.

### 5. Refund Window Configuration

The `REFUND_WINDOW_LEDGERS` parameter controls how long after payment a refund
can be claimed.

- [ ] Decide the production refund window:
      - `17280` ledgers ≈ 24 hours (testnet default)
      - `34560` ledgers ≈ 48 hours
      - `0` disables the window entirely
- [ ] Document the chosen value and the rationale.
- [ ] Pass it via the environment variable during deployment:

      ```bash
      REFUND_WINDOW_LEDGERS=<value> NETWORK=pubnet TOKEN=<usdc-sac-id> ./deploy.sh
      ```

### 6. Rent Funding and Monitoring

Soroban persistent storage incurs rent.  See the
[Storage Audit](storage-audit.md) for per-record costs and projections.

- [ ] Fund the deployer account with sufficient XLM to cover:
      - Base reserves for contract instances (2 XLM per contract)
      - Transaction fees for deployment and initialization
      - Initial storage rent for `BatchRecord` and `RefundRecord` entries
- [ ] Set up monitoring for the deployer account balance and storage rent.
- [ ] Decide who is responsible for funding rent extensions
      (`extend_batch_ttl`, `extend_refund_ttl`) in production.
- [ ] Document the monitoring and rent-funding procedure.

---

## Deployment Commands

### 0. Deploying the `RefundVaultFactory` (issue #129)

Factory deployments are constructor-wired: they create the stateless policy
contracts, the factory, and then per-merchant vaults. Deploy the three wasm
artifacts in order and wire the factory defaults to the freshly deployed
policies so no vault is ever created with an unconfigured gate:

```bash
NET=pubnet; ID=<identity>

TIME_ID=$(stellar contract deploy \
  --wasm target/wasm32v1-none/release/refund_policy_time.wasm \
  --source "$ID" --network "$NET" | tail -n 1)
VDF_ID=$(stellar contract deploy \
  --wasm target/wasm32v1-none/release/refund_policy_vdf.wasm \
  --source "$ID" --network "$NET" | tail -n 1)

FACTORY_ID=$(stellar contract deploy \
  --wasm target/wasm32v1-none/release/refund_vault_factory.wasm \
  --source "$ID" --network "$NET" | tail -n 1)

stellar contract invoke --id "$FACTORY_ID" --source "$ID" --network "$NET" \
  -- __constructor \
  --admin "$ID" \
  --vault_wasm_hash "$(stellar contract hash --wasm target/wasm32v1-none/release/refund_vault.wasm)" \
  --time_policy "$TIME_ID" \
  --vdf_policy "$VDF_ID"
```

Vaults are then created per merchant (merchant must sign):

```bash
stellar contract invoke --id "$FACTORY_ID" --source <merchant> --network "$NET" \
  -- deploy_vault \
  --init '{"merchant":"<merchant>","token":"<sac>","time_policy":"<time>","vdf_policy":"<vdf>","fee_bps":0,"fee_recipient":null,"refund_window":100,"deadline":0,"vdf_delay":0}'
```

Record `FACTORY_ID`, `TIME_ID`, `VDF_ID` alongside the other IDs in
`deployments/pubnet.env`. A `None`/null policy on an active gate deploys but
fails claims closed (`PolicyContractsNotConfigured`, 317) — configure both
factory defaults before taking merchant deployments.

### 1. Verify the deployment target

```bash
# Confirm the USDC SAC address resolves on mainnet
stellar contract invoke \
  --id <verified-usdc-sac-address> \
  --network pubnet --source <identity> \
  -- balance --id <some-known-account>
```

### 2. Run deploy.sh

```bash
NETWORK=pubnet \
TOKEN=<verified-usdc-sac-address> \
REFUND_WINDOW_LEDGERS=<your-value> \
IDENTITY=<your-identity> \
  ./deploy.sh
```

The script will:
1. Verify a clean git working tree and the `main` branch.
2. Build the WASM artifacts.
3. Display the WASM hashes and require explicit `YES` confirmation.
4. Deploy and initialize both contracts.
5. Write `deployments/pubnet.env` with contract IDs and provenance.
6. Verify the deployed contract metadata by reading it back.

### 3. Record the deployment

```bash
git add deployments/pubnet.env DEPLOYMENTS.md
git commit -m "docs: record pubnet deployment"
```

---

## Post-Deployment Verification

After deployment, verify the contracts independently:

### Read contract metadata

```bash
stellar contract invoke \
  --id <ReceiptAnchor-contract-id> \
  --network pubnet --source <identity> \
  -- get_version

stellar contract invoke \
  --id <RefundVault-contract-id> \
  --network pubnet --source <identity> \
  -- get_version
```

### Verify the vault token address

Read the vault's stored token address and confirm it matches the intended USDC
SAC:

```bash
stellar contract invoke \
  --id <RefundVault-contract-id> \
  --network pubnet --source <identity> \
  -- get_token
```

### Verify on Stellar Explorer

Check both contracts on
[stellar.expert](https://stellar.expert/explorer/public/):

- `https://stellar.expert/explorer/public/contract/<ReceiptAnchor-id>`
- `https://stellar.expert/explorer/public/contract/<RefundVault-id>`

### Anchor and verify a test receipt

After the indexer is running against pubnet, anchor a small batch and verify
a receipt against it to confirm the full end-to-end flow.

---

## Fee and Rent Analysis

### Transaction Fees

Soroban transaction fees are highly predictable.  Based on testnet benchmarks:

| Operation | Estimated Fee (XLM) | Notes |
|---|---|---|
| `anchor_batch` | ~0.02 – 0.05 | Scales with persistent storage reads/writes |
| `refund` | ~0.015 – 0.03 | Includes cross-contract calls to the USDC SAC |
| `verify_receipt` | 0 | Read-only simulation |

### Rent Cost Projection

Soroban state archiving requires paying "rent" to keep data in `Persistent`
storage.  A single `BatchRecord` occupies ~100 bytes.

**Scenario:** 500-payment batches, 1 year retention, 10,000 payments/day:

- 20 batches/day × 365 days = 7,300 batches
- Storage: 7,300 × 100 bytes ≈ 730 KB
- Rent: ~0.5 XLM/KB/year ≈ **365 XLM/year**
- Per-payment cost: negligible fraction of a cent

This makes the on-chain verifiable receipt architecture highly economical at
any reasonable transaction volume.

---

## Downstream Integrations

Once pubnet contract IDs are known, the following downstream systems will
need their configuration updated:

| System | Repository | What to update |
|---|---|---|
| Dashboard | [`accensa-app`](https://github.com/accensa/accensa-app) | Network config, contract ID references |
| Indexer | [`accensa-app`](https://github.com/accensa/accensa-app) | Contract IDs, token address, network passphrase |
| SDK | [`accensa-app`](https://github.com/accensa/accensa-app) | Network configuration, contract addresses |
| README / docs | This repository | `DEPLOYMENTS.md`, badges, explorer links |

These updates should only be made after the actual deployment IDs are
available from `deployments/pubnet.env`.  Do not pre-emptively change
references to values that do not yet exist.
