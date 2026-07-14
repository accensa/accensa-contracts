# Accensa Contracts

![Stellar](https://img.shields.io/badge/stellar-x402-blue.svg)

**[Live Dashboard](https://accensa-dashboard.vercel.app)** • **[Documentation](https://accensa-docs.vercel.app)**

Accensa is the merchant back-office for x402 sellers on Stellar. This repository contains the Soroban smart contracts for verifiable receipts and policy-bounded refunds.

## Contracts

*   **ReceiptAnchor:** Stores batched Merkle roots of payment receipts. Allows agents to independently verify they were charged correctly without needing a trusted API.
*   **RefundVault:** Holds merchant USDC float and executes policy-bounded refunds on-chain.

## Getting Started

To build locally:

1.  Ensure you have Rust and the `wasm32v1-none` target installed.
2.  Install the `soroban-cli`.
3.  Run `cargo build --target wasm32v1-none --release`.
4.  Run tests: `cargo test`.

## Contributing
See [CONTRIBUTING.md](CONTRIBUTING.md) for how to pick up open issues.

## Contributors

<a href="https://github.com/accensa/accensa-contracts/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=accensa/accensa-contracts" />
</a>

## License
MIT