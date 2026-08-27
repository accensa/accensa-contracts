# ADR 003: Bazaar Discovery Registry

## Context and Problem Statement

As the Accensa ecosystem grows, merchants, invoice factors, and third-party developers need a reliable way to discover trusted vaults, smart contracts, and associated metadata dynamically. Hardcoding contract addresses in off-chain applications scales poorly and leads to coordination bottlenecks during upgrades. We need an optional, decentralized, on-chain registry for Bazaar discovery.

## Considered Options

1. **Centralized off-chain API**: Easy to build, but creates a single point of failure and centralization in a decentralized protocol.
2. **Factory-emitted events**: Relying entirely on indexers to track `VaultCreated` events. While good for analytics, it is cumbersome for lightweight clients requiring real-time resolution.
3. **On-chain Soroban Registry Contract**: A dedicated smart contract mapping canonical names/identifiers to their current active addresses and metadata, fully readable on-chain.

## Decision Outcome

Chosen option: **On-chain Soroban Registry Contract**.

### Architecture Design

- **Contract Name**: `bazaar-registry`
- **Data Model**:
  - Key: `(Namespace, Identifier)` - typically strings or `Bytes`.
  - Value: `(ContractAddress, Metadata)` - the currently active contract address and optional descriptive information.
- **Access Control**: Only the global protocol admin (or a designated DAO multi-sig) can register or update the canonical entries to prevent spoofing and squatting.
- **Discovery Flow**:
  1. Off-chain client queries the registry with `(Namespace, "invoice-vault-v1")`.
  2. Registry returns the contract `Address`.
  3. Client interacts with the resolved address.

### Advantages

- Provides a single source of truth that is fully transparent and immutable.
- Allows clients to dynamically resolve addresses, streamlining upgrades.
- Maintains the decentralized nature of the Accensa protocol.

### Implementation Next Steps

- Draft the `bazaar-registry` contract in Rust.
- Define the `Namespace` standard for categorization (e.g., `vaults`, `oracles`, `routers`).
- Add comprehensive integration tests and deploy to testnet alongside the refund vault.
