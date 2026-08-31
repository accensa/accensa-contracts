# ADR 003: Immutability of Contracts

## Context
There has been discussion regarding the lack of an upgrade path for `ReceiptAnchor` and `RefundVault`. Currently, these contracts do not expose any `upgrade` functionality or calls to `env.deployer().update_current_contract_wasm()`.

## Decision
We have decided to maintain the immutability of these contracts as a deliberate design choice.

### Reasoning
1. **Trust Minimization:** By remaining immutable, we provide an absolute guarantee to users and auditors that the contract logic cannot be altered by the merchant or any other party post-deployment.
2. **Auditability:** Long-lived auditability of anchoring and refund logic is a primary goal. An immutable contract provides a stable target for verification that does not change over time.
3. **Complexity Reduction:** Implementing secure upgrade mechanisms (e.g., admin-gated, time-locked) introduces significant complexity and potential attack surfaces (e.g., admin key compromise leading to arbitrary code execution).

## Migration Strategy
In the event of a critical security vulnerability or required logic change, the migration path is as follows:
1. **Redeployment:** A new version of the contract is deployed to a new address.
2. **Migration:** Integrators and users are notified of the new contract address. For stateful contracts like `RefundVault`, a specific migration protocol will be defined for moving funds and state (if possible) or gracefully winding down the old contract.
3. **Legacy Support:** The old contract remains at its address, continuing to serve its original logic, ensuring that historic proofs remain valid and verifiable.

This approach shifts the burden of evolution from in-place patching to explicit versioning and migration, which aligns with our core value proposition of verifiable, trustless operation.