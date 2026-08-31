# ADR 004: Design of the Optional On-Chain Soroban Registry for Bazaar Discovery

> **Status: PROPOSED / DO NOT SHIP (Design complete, recommendation against on-chain deployment)**

## Context

The Stellar Community Fund (SCF) RFP §3.2 specifies that the x402 Bazaar discovery catalog should remain off-chain by default, while treating an on-chain Soroban discovery registry as an optional stretch deliverable: *"Keep index off-chain by default; onchain Soroban registry is optional stretch."* §3.5 further mandates: *"Address TTL and rent extension strategy if onchain registry included."*

In the current off-chain implementation (`x402-facilitator-stellar`), the discovery catalog is maintained as an in-memory or cached index of available MCP/REST endpoints and services. While operationally agile, an off-chain registry presents two main structural challenges:
1. **Catalog Volatility**: In-memory indexes can empty on server restart or drift between nodes.
2. **Spoofing / Ownership Binding**: Without cryptographic verification or decentralized binding, malicious or unauthorized actors could claim ownership or override metadata for endpoints they do not own.

This ADR explores whether deploying an on-chain Soroban discovery registry within `accensa-contracts` provides sufficient architectural value to justify the on-chain rent and invocation overheads.

---

## 1. What Belongs On-Chain: Full Record vs. Commitment Hash

Storing full discovery records (including URLs, human-readable descriptions, OpenAPI/MCP JSON schemas, and pricing tiers) directly in Soroban state is cost-prohibitive:
- Full resource records typically range between 500 bytes and 5 KB each.
- In Soroban, persistent entries are costed per byte and per ledger for rent.

### Chosen Data Architecture: Cryptographic Commitment Model

Instead of full record storage, the contract stores a concise **Listing Commitment**:

```rust
#[contracttype]
pub struct ListingKey {
    pub provider: Address,
    pub resource_hash: BytesN<32>, // SHA-256 hash of canonical endpoint URL + tool name
}

#[contracttype]
pub struct ListingRecord {
    pub manifest_hash: BytesN<32>, // SHA-256 hash of full off-chain JSON schema/manifest
    pub registered_at: u64,
    pub version: u32,
}
```

- **Key Size**: `Address` (32 bytes) + `BytesN<32>` (32 bytes) + key prefix overhead ≈ 80 bytes.
- **Value Size**: `BytesN<32>` (32 bytes) + `u64` (8 bytes) + `u32` (4 bytes) + struct overhead ≈ 56 bytes.
- **Total Footprint per Entry**: ~140 bytes (well within Soroban limits).

---

## 2. Storage Class and Rent Cost Analysis

Following the methodology established in [`docs/storage-audit.md`](storage-audit.md):
- On Stellar mainnet, persistent storage rent is approximately **0.5 XLM per KB per year** (~0.000488 XLM/byte/year).
- Base entry overhead adds ~100 bytes per persistent ledger entry.
- **Effective entry storage footprint**: 140 bytes data + 100 bytes entry overhead = **240 bytes / listing**.

### Cost Calculations at Realistic Catalog Scales

| Catalog Scale | Total Entries | Storage Footprint | Annual Rent Cost | Cost per Listing / Year |
| :--- | :--- | :--- | :--- | :--- |
| **Small Pilot** | 100 listings | 24 KB | 12 XLM (~$1.44) | 0.12 XLM (~$0.014) |
| **Medium Ecosystem** | 5,000 listings | 1.20 MB | 600 XLM (~$72.00) | 0.12 XLM (~$0.014) |
| **Large-Scale Bazaar** | 50,000 listings | 12.0 MB | 6,000 XLM (~$720.00) | 0.12 XLM (~$0.014) |
| **Global Enterprise Catalog** | 500,000 listings | 120.0 MB | 60,000 XLM (~$7,200.00) | 0.12 XLM (~$0.014) |

While 0.12 XLM/year per listing appears modest in isolation, maintaining hundreds of thousands of dynamic tool listings incurs ongoing economic drag and rent replenishment overhead.

---

## 3. TTL and Rent-Extension Strategy (§3.5 Compliance)

To prevent persistent entry archival and state bloat:

1. **Storage Class**: `Persistent` (must not use `Temporary`, as deletion allows spoofing and unauthorized recreation).
2. **Payer Model**: The service provider (`provider.require_auth()`) pays the initial creation rent and TTL bump at registration.
3. **Public Extension Helper**: Mirroring `ReceiptAnchor::extend_batch_ttl` and `RefundVault::extend_refund_ttl`, the registry exposes a public `extend_listing_ttl(provider: Address, resource_hash: BytesN<32>)` function. Anyone (facilitators, indexers, providers) can bump the listing's TTL.
4. **TTL Policy**:
   - `TTL_EXTEND`: 518,400 ledgers (~30 days).
   - `TTL_THRESHOLD`: 100 ledgers (or dynamic threshold for active listings).
   - **Lapse & Archival**: When a listing's TTL lapses, it becomes `Archived`. It cannot be overwritten or hijacked by a third party. To reactivate, the legitimate owner or any indexer submits a `RestoreFootprint` operation.

---

## 4. Ownership Model and Reconciling Facilitator Spoofing

- **Authentication Invariant**: Registration and updates require `provider.require_auth()`. A listing is permanently bound to the `provider` address.
- **Resolution**:
  1. Off-chain discovery clients fetch the full JSON schema from off-chain peer nodes or IPFS.
  2. The client checks the on-chain registry: `registry.get_listing(provider, resource_hash)`.
  3. The client verifies that `SHA256(offchain_manifest) == onchain_manifest_hash`.
  4. If a malicious facilitator attempts to spoof an endpoint or alter pricing/routing, the hash verification fails immediately.

---

## 5. Soroban Resource Limits (CPU & Memory Footprint)

- **Registry Write (`register_listing` / `update_listing`)**:
  - Requires 1 persistent write, 1 instance read, and SHA-256 verification.
  - CPU Instructions: ~35,000 (Limit: 100,000,000).
  - Memory: ~25 KB (Limit: 40 MB).
  - Well within Soroban limits.
- **Registry Read (`get_listing`)**:
  - Requires 1 persistent read.
  - CPU Instructions: ~12,000.
  - Memory: ~10 KB.

---

## 6. Architectural Recommendation: DO NOT SHIP (Keep Off-Chain with Cryptographic Signatures)

### **Recommendation: DO NOT SHIP on-chain registry.**

### Rationale:
1. **Unnecessary Operational Complexity**: Off-chain discovery with cryptographic provider signatures (e.g. Ed25519 or SEP-41/ERC-191 signed manifests published via standard HTTP headers or IPFS) solves the spoofing problem completely without incurring on-chain rent or latency.
2. **Latency Impact on Real-Time Agent Interactions**: AI agents discovering MCP tools dynamically require sub-50ms catalog lookups. Querying on-chain Soroban RPC endpoints adds 100ms–1500ms of latency per tool discovery query.
3. **Economic Overhead**: Maintaining active rent across thousands of transient or updated tools requires complex rent-payer custody or automated refill bots.
4. **Alignment with RFP**: SCF RFP §3.2 explicitly recommends keeping discovery off-chain by default.

### Alternative Path Forward:
- Use signed manifests anchored via the existing `ReceiptAnchor` Merkle trees if batch notarization of catalog versions is ever required.
- Retain this ADR as conclusive evidence of discovery cost modeling and architectural analysis for SCF evaluation.

---

## References
- `docs/storage-audit.md` — Storage class classification and rent calculation methodology
- `docs/ADR-003-upgradeability.md` — Contract immutability and governance model
- [SCF RFP: x402 Facilitator with Bazaar Discovery Support](https://stellar.gitbook.io/scf-handbook/scf-awards/build-award/rfp-track#x402-facilitator-with-bazaar-discovery-support-1)