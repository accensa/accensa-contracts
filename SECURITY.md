# Security Policy

## Threat Model & Security Properties

For a detailed analysis of our trust assumptions, attack vectors, and protocol invariants, please review our [Security Model](docs/SECURITY_MODEL.md).

## Supported Versions

Only the latest `main` branch and active releases are supported with security updates.

| Version | Supported |
| ------- | --------- |
| `main` (`0.2.x`) | :white_check_mark: |
| `< 0.2.0` | :x: |

## Reporting a Vulnerability

If you discover a security vulnerability, please **do not report it publicly**.

### Preferred Reporting Channel
Use **[GitHub Private Vulnerability Reporting](https://github.com/accensa/accensa-contracts/security/advisories/new)** to submit private reports directly to the maintainers. This channel has been tested end-to-end and ensures confidential triage.

### Secondary Contact
If GitHub Private Vulnerability Reporting is unavailable, send an email to **`security@accensa.dev`** or contact maintainers via direct message in the Stellar Developer Discord.

## Scope

### In-Scope
- `ReceiptAnchor` Soroban smart contract (`contracts/receipt-anchor`).
- `RefundVault` Soroban smart contract (`contracts/refund-vault`).
- On-chain Soroban state transitions, access control, double-refund protections, and Merkle root verification.
- Live testnet deployments documented in [`DEPLOYMENTS.md`](DEPLOYMENTS.md).

### Out-of-Scope
- Merchant private key management and custody.
- Off-chain indexer, dashboard, and SDK repositories (covered in [`accensa-app`](https://github.com/accensa/accensa-app)).
- Stellar network RPC nodes and third-party RPC providers.
- Protocol-level Soroban host environment or Stellar consensus vulnerabilities.

## Response SLA & Disclosure Policy

- **Initial Triage:** Maintainers will acknowledge and perform initial triage within **48 hours**.
- **Progress Updates:** Status updates will be provided every **5 business days** until a fix or patch is ready.
- **Coordinated Disclosure:** We adhere to a standard **90-day coordinated disclosure window** (or mutually agreed release date) before publishing advisory details.

> [!NOTE]
> The smart contracts in this repository are currently **UNAUDITED**. Exercise caution when deploying to mainnet environments.
