# Accensa Benchmarks

This document records the Soroban budget methodology used for contract resource
regressions. Receipt-anchor Merkle verification is measured independently from
setup and anchoring, across representative batch sizes.

## ReceiptAnchor Merkle verification (#125)

`contracts/receipt-anchor/src/test.rs::test_verify_receipt_batch_size_instruction_benchmark`
measures CPU instruction usage for batch sizes 1, 10, 25, 50, and 100. The test
builds a valid sorted-pair proof for each depth, resets the Soroban budget before
verification, and prints the measured value as a CI artifact. The implementation
uses one iterative pure-WASM SHA-256 fold: proof elements are not copied into a
second buffer and no host crypto call is made per level.

The test is intentionally a measurement suite rather than a fragile absolute
threshold: Soroban SDK/toolchain revisions can legitimately change budget values.
The existing budget-regression test continues to protect the checked-in baseline;
run the benchmark before and after changes and compare the printed
`cpu_instructions` values for the same batch size.


## Methodology

- **Test Environment:** (To be defined upon measurement)
- **Soroban SDK Version:** `27.0.4`
- **Measurement Infrastructure:** Tests using `soroban_sdk::Env::budget()` to capture CPU instructions and memory bytes.
- **Contract Version:** (To be defined)

### Benchmark Scenarios
- **A. Fresh Authorization:** The payer begins without relevant pre-existing state. We measure `authorize`, `settle`, and their pair total.
- **B. Existing Payer Token State:** The payer already has relevant token/account state. We measure if this affects ledger reads, writes, CPU, memory, or fees.
- **C. Settlement Amount Variation:** We measure settlement at 1%, 50%, and 100% of the authorization cap.
- **D. Recipient Trustline Variation:** We measure with and without an existing recipient trustline, if applicable.
- **E. Concurrent Authorizations:** We test realistic contention/burst behavior for multiple authorizations from the same payer (within simulation capabilities).

## Resource Results

*Measurements are pending `#65`.*

| Scenario | Operation | CPU Instructions | Memory (bytes) | Ledger Reads | Ledger Writes | Fee (Stroops) | Fee (USD) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| A. Fresh | `authorize` | TBD | TBD | TBD | TBD | TBD | TBD |
| A. Fresh | `settle` | TBD | TBD | TBD | TBD | TBD | TBD |
| A. Fresh | **Pair Total** | TBD | TBD | TBD | TBD | TBD | TBD |
| B. Existing State | `authorize` | TBD | TBD | TBD | TBD | TBD | TBD |
| B. Existing State | `settle` | TBD | TBD | TBD | TBD | TBD | TBD |
| B. Existing State | **Pair Total** | TBD | TBD | TBD | TBD | TBD | TBD |
| C. Variation (1%) | `settle` | TBD | TBD | TBD | TBD | TBD | TBD |
| C. Variation (50%) | `settle` | TBD | TBD | TBD | TBD | TBD | TBD |
| C. Variation (100%)| `settle` | TBD | TBD | TBD | TBD | TBD | TBD |
| D. No Trustline | `authorize` | TBD | TBD | TBD | TBD | TBD | TBD |
| Exact Baseline | `exact` | TBD | TBD | TBD | TBD | TBD | TBD |

## Network Limit Comparison

| Operation | Metric | Network Limit | Measured Value | Percentage Used | Percentage Headroom |
| --- | --- | --- | --- | --- | --- |
| `authorize` | CPU | TBD | TBD | TBD% | TBD% |
| `authorize` | Memory | TBD | TBD | TBD% | TBD% |
| `settle` | CPU | TBD | TBD | TBD% | TBD% |
| `settle` | Memory | TBD | TBD | TBD% | TBD% |
| **Pair Total** | (Aggregate info across operations, for cost reference only) | - | - | - | - |

> *Important:* The Pair Total represents the aggregate cost for a metered payment. It must not be directly compared to a single per-transaction network limit since `authorize` and `settle` are executed in separate transactions.

## Economic Comparison

- **Current XLM/USD Price:** TBD (Source: TBD, Timestamp: TBD)
- **Metered Request Price Assumptions:**
  - $0.001
  - $0.005
  - $0.01

### Viability Analysis

| Scenario | Request Price (USD) | Pair Cost (USD) | Cost Percentage | Economically Viable? |
| --- | --- | --- | --- | --- |
| Fresh | $0.001 | TBD | TBD% | TBD |
| Fresh | $0.005 | TBD | TBD% | TBD |
| Fresh | $0.010 | TBD | TBD% | TBD |

## Conclusion

*(To be written when measurements are complete. Example: "The two-invocation design remains economically viable above the measured threshold because the pair costs X% or less of the representative request price.")*
