# Security Model

This document outlines the threat model, trust assumptions, and attack mitigations for the Accensa smart contracts.

## Trust Assumptions

### 1. The Admin (Merchant)
The admin is assumed to be a trusted entity in the context of configuring the contract. They are responsible for:
- Initializing the `RefundVault` with the correct token address and parameters.
- Maintaining the float balance required to process refunds.
- Authorizing deposits and legitimate configuration changes.
If the admin's private key is compromised, the attacker could drain the vault's float or block refunds.

### 2. The Indexer (Off-chain)
The off-chain indexer service is responsible for aggregating receipts and computing the Merkle root of the batches. 
- It is trusted to correctly hash the valid receipts and anchor the legitimate root on-chain.
- However, since users cryptographically verify their specific receipt against the on-chain root, a compromised indexer cannot forge a valid proof for a fake receipt that passes the on-chain check without brute-forcing a hash collision.

### 3. The User (Buyer)
Users are untrusted. The contracts must assume any data submitted by users could be malicious and must validate all inputs (e.g., verifying amounts are greater than zero, verifying proofs).

## Attack Vectors and Mitigations

### Replay Attacks
- **Threat:** An attacker attempts to submit a valid refund proof multiple times to drain the vault.
- **Mitigation:** Once a refund is processed, the `payment_ref` is recorded in persistent storage. The contract checks this state (`AlreadyRefunded` error) to strictly enforce that each receipt can only be refunded once.

### Proof Forgery
- **Threat:** An attacker tries to claim a refund for a non-existent or altered receipt.
- **Mitigation:** The contract utilizes a sorted-pair Merkle tree. Every refund request requires a cryptographic inclusion proof that must perfectly resolve to the anchored root hash. Modifying the receipt or the proof will result in a mismatched root, causing the transaction to revert.

### Window Expiry Evasion
- **Threat:** An attacker attempts to process a refund after the designated refund window has expired.
- **Mitigation:** The contract enforces the refund window by strictly comparing the current ledger sequence against the `paid_at_ledger` plus the `RefundWindow`. If the threshold is crossed, the transaction is rejected with a `WindowExpired` error.

### Float Draining (Negative/Zero Amounts)
- **Threat:** An attacker tries to refund a negative amount to cause an underflow or steal funds.
- **Mitigation:** Explicit validation ensures that the `amount` is strictly greater than zero (`InvalidAmount` error) before executing token transfers, preventing unintended arithmetic behaviors or logical exploits.

## Storage Security

For details on how storage archival and persistence affect the security model (such as preventing replay attacks via persistent tombstoning), see the [Storage Audit](storage-audit.md).
