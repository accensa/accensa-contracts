# Accensa Contracts

![Stellar](https://img.shields.io/badge/stellar-x402-blue.svg)
![Drips Wave](https://img.shields.io/badge/wave-active-success.svg)

Accensa is the merchant back-office for x402 sellers on Stellar. This repository contains the Soroban smart contracts for verifiable receipts and policy-bounded refunds.

## Contracts

*   **ReceiptAnchor:** Stores batched Merkle roots of payment receipts. Allows agents to independently verify they were charged correctly without needing a trusted API.
*   **RefundVault:** Holds merchant USDC float and executes policy-bounded refunds on-chain.

## Getting Started

To build locally:

1.  Ensure you have Rust and the `wasm32-unknown-unknown` target installed.
2.  Install the `soroban-cli`.
3.  Run `cargo build --target wasm32-unknown-unknown --release`.
4.  Run tests: `cargo test`.

## Contributing
See [CONTRIBUTING.md](CONTRIBUTING.md) for how to pick up open issues.

## Maintainers
- Victor Adeleke

## License
MIT