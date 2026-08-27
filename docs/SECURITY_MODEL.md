# Security Model

This document outlines the threat model, trust assumptions, and attack mitigations for the Accensa smart contracts.

> **Audit readiness:** for the audit scope, enumerated invariants, known
> accepted risks, and engagement logistics, see [AUDIT.md](AUDIT.md). This
> document is the canonical threat model; the two are kept reconciled.

## Trust Assumptions

### 0. Immutability of Deployed Contracts

Both `ReceiptAnchor` and `RefundVault` are **immutable**: they contain no upgrade
entry point and no call to `update_current_contract_wasm`. This is a deliberate
security property, not an omission — see
[ADR 003](ADR-003-upgradeability.md).

Consequences for the threat model:
- **No admin — including the operator — can change contract behaviour after
deployment.** A merchant can withdraw float, pause/unpause, and (via the two-step
transfer) hand over admin, but cannot install new rules or alter the refund policy
or anchoring behaviour.
- **A compromised admin key cannot rewrite wasm.** Its blast radius is bounded by
the authorisation surface the contract already encodes (drain float, pause, refund
within policy).
- **Bugs are permanent at a deployed address.** The response to a logic defect is
withdraw-and-redeploy per the migration runbook in ADR 003, not an in-place patch.

### 1. The Admin (Merchant)
The admin is assumed to be a trusted entity in the context of configuring the contract. They are responsible for:
- Initializing the `RefundVault` with the correct token address and parameters.
- Maintaining the float balance required to process refunds.
- Authorizing deposits and legitimate configuration changes.
If the admin's private key is compromised, the attacker could drain the vault's float or block refunds.

**The admin may be any `Address`, including a contract account.** On Soroban an
`Address` is not required to be a keypair — it can be a contract that implements
`__check_auth`, and `require_auth()` invokes that implementation. Both contracts
store a single `Address` as `Admin` and put no constraint on what it is, so a
`RefundVault` or `ReceiptAnchor` initialised with a multisig contract account as
its merchant already requires that account's threshold of signers for `pause`,
`unpause`, `set_refund_window`, `refund`, `withdraw`, `anchor_batch` and
`prune_batches` — with no threshold logic in this repository. This is covered by
tests in `contracts/refund-vault/tests/multisig_admin_vault.rs` and
`contracts/receipt-anchor/tests/multisig_admin_anchor.rs` (see
[`DEPLOYMENTS.md`](../DEPLOYMENTS.md#using-a-multisig-contract-account-as-admin)).

Implications for key-compromise risk when the admin is a contract account:
- **There is no single key that controls the contracts.** If the admin is a
  multisig with threshold *n*, draining the float or pausing the vault requires
  compromising *n* distinct signers, not one. The blast radius of any single
  compromised key collapses to nothing (one signer alone cannot even initiate an
  admin transfer).
- **The compromise surface moves to the account contract and its signer
  management.** The same guarantees that apply to the merchant keypair now apply
  to whatever governs the account's signer set (e.g. the account contract's own
  upgrade or signer-rotation policy). Read the account's documentation before
  relying on it as the vault's admin.
- **Signer rotation is a separate operation.** The vault's `transfer_admin`
  two-step handover is one way to move admin to a new account; the alternative is
  to rotate signers *inside* the account contract, which does not touch the vault
  at all.

### 2. The Indexer (Off-chain)
The off-chain indexer service is responsible for aggregating receipts and computing the Merkle root of the batches. 
- It is trusted to correctly hash the valid receipts and anchor the legitimate root on-chain.
- However, since users cryptographically verify their specific receipt against the on-chain root, a compromised indexer cannot forge a valid proof for a fake receipt that passes the on-chain check without brute-forcing a hash collision.

### 3. The User (Buyer)
Users are untrusted. The contracts must assume any data submitted by users could be malicious and must validate all inputs (e.g., verifying amounts are greater than zero, verifying proofs).

### 4. Refund float ownership
The float can only be funded by the merchant. `deposit` requires the depositor to be the merchant address (`from == merchant`, plus merchant auth); any other address is rejected with `Unauthorized`. This is a **deliberate guarantee**, not an implementation detail: the only funds ever at stake in the vault are the merchant's own, and a third party cannot contribute float — dust, unsolicited funding, or a top-up from a treasury contract — that the merchant has not authorised. `withdraw` is merchant-only for the same reason (it may send funds to any address the merchant chooses). A merchant who wants a finance key, treasury contract, or automated top-up to fund refunds must route those deposits through an address the merchant controls; the contract does not accept third-party funding by design.

### 5. The Yield Strategy (optional, `main` only)
The `RefundVault` yield integration (added after this document was first
written; present on `main` but not on the `0.1.0` testnet deployment) introduces
one additional trust assumption: funds deployed to a registered strategy are
trusted to that strategy's contract. The vault enforces reserve and deployment
ratios but cannot enforce strategy solvency, and the strategy is a potential
re-entrancy surface. See [AUDIT.md](AUDIT.md) §2 and §5 for the full treatment.

## Attack Vectors and Mitigations

### Replay Attacks
- **Threat:** An attacker attempts to submit the same refund request repeatedly to drain the vault.
- **Mitigation:** Refunds are cumulative: each `refund` call for a `payment_ref`
  adds to a stored running total, and cumulative refunds may never exceed the
  original `payment_amount` supplied on the call. A call that would push the
  total past the ceiling is rejected with an `ExceedsPayment` error, and a
  record written by the legacy single-refund rule is treated the same way. There
  is no code path that pays the same amount twice for one payment.
- **Guard persistence:** This ceiling only holds while the `RefundV2` record it
  depends on is actually live. The record's TTL is now sized to the
  merchant's configured `refund_window_ledgers` (extended to the network's
  maximum TTL when the window is `0`, i.e. "no time bound") rather than a
  flat interval, so the guard cannot expire while `refund` would still accept
  a call against that payment on policy grounds — see "TTL Strategy" in
  [Storage Audit](storage-audit.md) for the mechanism and why the naive flat
  extension was silently a no-op. What remains unverified against a live
  network (only checked against this SDK's test host) is whether an entry
  that *does* go archived — i.e. outlives even the window-sized TTL, or is
  simply never restored — causes `refund` to fail closed (a host trap on
  accessing an archived entry) or fail open (a stale read). The test host
  auto-heals expired entries rather than modeling archival, so it cannot
  distinguish the two. Treat this as an open item for anyone auditing against
  testnet/mainnet directly, and see #122 for a nonce-based alternative that
  would not depend on the answer either way.

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

## Operational Considerations

### Trustline Failures
For Classic Stellar assets wrapped in a Stellar Asset Contract (like USDC), the recipient must establish a trustline before receiving tokens. If a buyer's account lacks a trustline for the token, a `refund` will revert with a token-level `HostError`. The `RefundVault` does not pre-check trustlines because doing so would consume excess computation budget for successful refunds. Instead, this token-level panic is bubbled up and treated as an expected operational failure. Merchants issuing manual `withdraw` transactions face the same trustline requirement for the destination address.

## Vulnerability Reporting

If you discover a vulnerability that breaks any of the security properties or
mitigations described in this document, please follow our private disclosure
guidelines in [SECURITY.md](../SECURITY.md).

