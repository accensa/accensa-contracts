<div align="center">
  <h1>accensa-contracts</h1>
  <p><strong>Verifiable receipts and policy-bounded refunds for x402 payments on Stellar</strong></p>
  <p>
    <img src="https://img.shields.io/github/actions/workflow/status/accensa/accensa-contracts/ci.yml?branch=main" alt="CI Status" />
    <img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="License" />
    <img src="https://img.shields.io/badge/soroban--sdk-27.0.4-orange.svg" alt="soroban--sdk 27" />
    <img src="https://img.shields.io/badge/testnet-deployed-success.svg" alt="Deployed on testnet" />
  </p>
  <p>
    <a href="DEPLOYMENTS.md"><strong>Live on Testnet</strong></a> ·
    <a href="https://accensa.github.io/accensa-app/docs/contracts/overview"><strong>Documentation</strong></a> ·
    <a href="https://accensa-dashboard.vercel.app"><strong>Dashboard</strong></a> ·
    <a href="https://github.com/accensa/accensa-app"><strong>accensa-app</strong></a>
  </p>
</div>

> Part of the **[Accensa](https://github.com/accensa)** merchant back-office for
> x402 sellers on Stellar. This repo holds the on-chain half; the indexer,
> dashboard, and SDK live in [`accensa-app`](https://github.com/accensa/accensa-app).

## The Problem

x402 turns any HTTP endpoint into a paid resource: an AI agent hits your API, gets a
`402 Payment Required`, pays, and retries. That works — but it leaves both sides
without recourse.

**The agent cannot prove it was charged correctly.** Its receipt comes from the
seller's own API, attesting to payment without ledger backing. If the seller goes
offline, ghosts a refund, or double-bills, the agent has no recourse.

**The merchant has no liability cap.** Holding user float directly invites hacks
and disputes.

## The Solution

Accensa bridges x402 to Stellar via two Soroban smart contracts:

1. **ReceiptAnchor** — Merchants batch and anchor payment receipt roots on-chain using Merkle trees, giving agents verifiable proof of payment that survives server loss.
2. **RefundVault** — A policy-bounded vault holding merchant float for automated refunds, restricted by time windows, balance limits, and merchant authorization.

## Enforced Invariants & Test Coverage Mapping

Enforced invariants, each covered by a test:

- **No double refunds** — a `payment_ref` can only be refunded once (`AlreadyRefunded`).
  *Mapped Test:* `contracts/refund-vault/src/test.rs` -> `test_double_refund_same_payment_ref_fails`
- **Time-bounded** — refunds past `refund_window_ledgers` are rejected (`WindowExpired`).
  *Mapped Test:* `contracts/refund-vault/src/test.rs` -> `test_refund_outside_window_fails`, `test_refund_at_window_boundary_succeeds`
- **Float-bounded** — a refund can never exceed vault balance (`InsufficientFloat`).
  *Mapped Test:* `contracts/refund-vault/src/test.rs` -> `test_refund_exceeding_float_fails`, `test_withdraw_exceeding_float_fails`
- **Merchant-only** — every state-changing call requires merchant/admin auth (`Unauthorized`), with the explicit exception of `initialize` (which initializes the contract instance and does not require prior auth, see #145).
  *Mapped Tests:* `contracts/refund-vault/src/test.rs` -> `test_refund_requires_merchant_auth`, `test_deposit_from_non_merchant_fails`, `test_pause_requires_merchant_auth`, `test_unpause_requires_merchant_auth`, `test_transfer_admin_requires_auth`, `test_cancel_admin_transfer_requires_auth`, `test_accept_admin_requires_pending_auth`; `contracts/receipt-anchor/src/test.rs` -> `test_anchor_batch_requires_merchant_auth`, `test_prune_batches_requires_admin_auth`
- **Pausable** — operations are halted if the vault is paused (`Paused`).
  *Mapped Test:* `contracts/refund-vault/src/test.rs` -> `test_refund_when_paused_fails`, `test_deposit_when_paused_fails`, `test_withdraw_when_paused_fails`

## Documentation

- [Architecture Overview](docs/ARCHITECTURE.md)
- [Security Model](docs/SECURITY_MODEL.md)
- [Merkle Tree Structure](docs/ADR-001-merkle-structure.md)
- [Deployments](DEPLOYMENTS.md)

## License

MIT License. See [LICENSE](LICENSE) for details.
