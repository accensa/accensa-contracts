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

### `verify_receipt_leaf` (root-history path)

`contracts/receipt-anchor/src/budget_test.rs::measure_verify_receipt_leaf_by_depth`
(`contracts/receipt-anchor/src/budget_test.rs:245`; run with `--features
budget-assert` after building both contracts to WASM) measures the real
`receipt_anchor` WASM, budget reset immediately before the call. The proof is
checked against the shard's retained roots in the router itself, with no
cross-contract call to a storage shard.

Profile: Tier A, `wasm32v1-none` release WASM invoked by the Soroban test host.
The source does not commit a measurement date for this table, so these rows are
undated; re-run the test to produce a dated result.

| Proof depth | Batch size up to | CPU instructions | Memory (bytes) |
| --- | --- | --- | --- |
| 0 | 1 | 680,379 | 1,524,651 |
| 1 | 2 | 892,836 | 1,524,675 |
| 2 | 4 | 1,105,293 | 1,524,699 |
| 4 | 16 | 1,530,207 | 1,524,747 |
| 7 | 128 | 2,167,578 | 1,524,819 |
| 10 (`MAX_PROOF_LEN`) | 1,024 | 2,811,397 | 1,524,891 |

Source for every CPU and memory value in this table:
`contracts/receipt-anchor/src/budget_test.rs:245`
(`measure_verify_receipt_leaf_by_depth`).

CPU grows by ~212k instructions per level; memory is flat (~24 bytes per
level). The worst case uses 2.8% of the 100,000,000-instruction and 3.6% of
the 41,943,040-byte per-transaction limits; the measurement test fails if
either exceeds 10%. `budget_verify_receipt_leaf_depth_{1,10}`
(`contracts/receipt-anchor/src/budget_test.rs:210-233`) gate CPU at the
baseline × 1.15.

## Methodology

The figures in this document come from two local profiles. Neither is a Mainnet
or Testnet simulation.

- **Tier A (`budget_test.rs`):** the contract is built for `wasm32v1-none` in
  release mode and invoked by the Soroban test host. CPU is read from
  `env.cost_estimate().budget().cpu_instruction_cost()` immediately after the
  budget is reset, with setup outside the measured window. These gates assert
  CPU only (`#[budget_cpu_lt]`).
- **Test-host regressions (`fuzz_test.rs`):** the same budget accessors run
  against the test host. These baselines gate both CPU and memory with a 15%
  headroom (`HEADROOM_PERCENT = 15`).
- **Soroban SDK:** `27.0.6` (`Cargo.lock:2103-2104`).
- **Tolerance:** every gate is `baseline × 1.15`, the 15% tolerance described in
  `budget.toml:18-23` and `contracts/receipt-anchor/src/budget_test.rs:8-10`.
  The tables below report the committed **baseline**, not the 15%-raised failure
  threshold. For example, `authorize` is gated at
  `713,619 × 1.15 = 820,662`, rounded up to `825,000` at
  `contracts/upto-authorization/src/budget_test.rs:72-75`.
- **Fresh measurements:** `cargo test --package receipt-anchor
  benchmark_gas_and_cpu_instructions` and `cargo test --package refund-vault
  test_refund_resource_cost_budget` (`CONTRIBUTING.md:63-64`). The Tier A gates
  need their WASM built first; the build command is at the top of each
  `budget_test.rs` (for example
  `contracts/upto-authorization/src/budget_test.rs:10-11`).

### Benchmark Scenarios

These scenarios are inherited from the `upto` scheme
(`docs/ADR-002-upto-scheme.md`). Only scenarios with a committed baseline are
reported; the rest are explicitly unmeasured rather than guessed.

| Scenario | Entry point | Status |
| --- | --- | --- |
| A. Fresh Authorization | `authorize`, `settle` (`contracts/upto-authorization/src/lib.rs:147`, `:371`) | Measured: `budget_authorize` (`contracts/upto-authorization/src/budget_test.rs:76`) and `budget_settle_at_cap` (`:88`) |
| B. Existing Payer Token State | `authorize`, `settle` | Unmeasured: no differential run with pre-existing payer token/account state is committed |
| C. Settlement Amount Variation | `settle` | Partly measured: only the cap and the 1-unit minimum are gated (`budget_settle_at_cap`, `budget_settle_at_min_boundary`); 1% and 50% are unmeasured |
| D. Recipient Trustline Variation | `settle` | Unmeasured: no run with and without a recipient trustline is committed |
| E. Concurrent Authorizations | `authorize` | Unmeasured: no multi-payer or concurrency test is committed |

`exact` is not an entry point. It names the authorisation scheme that
`authorize` implements when `max_slippage_bps = 0`
(`contracts/upto-authorization/src/lib.rs:145-155`), so the old
`Exact Baseline | exact` row is removed.

## Resource Results

### Committed baselines

Every figure below is the value committed in the cited source; none is estimated.
The line numbers point at the baseline constant or measurement comment and at the
test that produces the value.

| Entry point | Contract | CPU instructions | Memory (bytes) | Baseline source | Profile | Measured on |
| --- | --- | --- | --- | --- | --- | --- |
| `authorize` | `upto-authorization` | 713,619 | Not measured (1) | `contracts/upto-authorization/src/budget_test.rs:72` (`budget_authorize`, `:76`) | Tier A WASM | 2026-10-16 |
| `settle` (at cap) | `upto-authorization` | 985,389 | Not measured (1) | `contracts/upto-authorization/src/budget_test.rs:84` (`budget_settle_at_cap`, `:88`) | Tier A WASM | 2026-10-16 |
| `settle` (1-unit minimum) | `upto-authorization` | 985,389 | Not measured (1) | `contracts/upto-authorization/src/budget_test.rs:97` (`budget_settle_at_min_boundary`, `:101`) | Tier A WASM | 2026-10-16 |
| `refund` | `refund-vault` | 704,621 | 197,217 | `contracts/refund-vault/src/fuzz_test.rs:390-391` (`test_refund_resource_cost_budget`, `:394`) | test-host regression | 2026-08-30 |
| `anchor_batch` (N=1000) | `receipt-anchor` | 1,881,937 | 3,981,062 | `contracts/receipt-anchor/src/fuzz_test.rs:1064-1065` (`benchmark_gas_and_cpu_instructions`, `:1076`) | test-host regression | 2026-09-24 |
| `verify_receipt` (4-leaf) | `receipt-anchor` | 780,762 | 1,378,946 | `contracts/receipt-anchor/src/fuzz_test.rs:1072-1073` (`benchmark_gas_and_cpu_instructions`, `:1076`) | test-host regression | 2026-08-29 |

The `verify_receipt_leaf` values are in the table above; that test is the only
source for them (`contracts/receipt-anchor/src/budget_test.rs:245`).

(1) No memory baseline is committed for `authorize` or `settle`: the Tier A
`#[budget_cpu_lt]` gates assert CPU only. Memory baselines exist only for the
test-host regressions listed above.

### Unmeasured dimensions

| Dimension | Applies to | Reason |
| --- | --- | --- |
| Memory (bytes) | `authorize`, `settle` | No committed memory baseline; Tier A gates CPU only (see note 1) |
| Ledger reads and writes | all scenarios | Require a Tier B network simulation. `budget.toml:40-45` notes these entry points need pre-funded fixture state, so no read/write limits are committed |
| Fees (stroops) | all scenarios | No committed fee measurement exists |
| Fees (USD) | all scenarios | Requires a dated XLM/USD quote (see Economic Comparison) |
| Scenarios B, D and E | `authorize`, `settle` | No committed differential, trustline or concurrency test |
| Scenario C at 1% and 50% | `settle` | Only the cap and the 1-unit minimum are gated by `budget_settle_at_cap` and `budget_settle_at_min_boundary` |

## Network Limit Comparison

CPU limit is 100,000,000 instructions (`budget.toml:27`,
`contracts/receipt-anchor/src/budget_test.rs:237`). Memory limit is 40 MiB =
41,943,040 bytes (`contracts/receipt-anchor/src/budget_test.rs:238`;
`budget.toml:28` rounds this to 40,000,000).

Measured values below are the committed baselines from the Resource Results
section; percentages are `baseline / network limit`. The 15% gate ceiling is
still below 1.2% of the CPU limit for either entry point.

| Operation | Metric | Network Limit | Measured Value (baseline) | Percentage Used | Percentage Headroom |
| --- | --- | --- | --- | --- | --- |
| `authorize` | CPU | 100,000,000 | 713,619 | 0.71% | 99.29% |
| `authorize` | Memory | 41,943,040 | Not measured (1) | Not measured | Not measured |
| `settle` | CPU | 100,000,000 | 985,389 | 0.99% | 99.01% |
| `settle` | Memory | 41,943,040 | Not measured (1) | Not measured | Not measured |
| **Pair Total** | CPU (aggregate) | - | 1,699,008 | 1.70% | 98.30% |
| **Pair Total** | Memory (aggregate) | - | Not measured (1) | Not measured | Not measured |

The pair total is the sum of the two committed CPU baselines. The Pair Total is
information only: it must not be compared directly to a single per-transaction
limit because `authorize` and `settle` execute in separate transactions.

## Economic Comparison

- **Current XLM/USD Price:** Not measured. No dated price source or timestamp is
  committed, so the CPU/memory baselines cannot be converted to USD here.
- **Metered Request Price Assumptions (inputs, not measurements):**
  - $0.001
  - $0.005
  - $0.01

### Viability Analysis

| Scenario | Request Price (USD) | Pair Cost (USD) | Cost Percentage | Economically Viable? | Blocking reason |
| --- | --- | --- | --- | --- | --- |
| Fresh | $0.001 | Not measured | Not measured | Not determined | No dated XLM/USD quote |
| Fresh | $0.005 | Not measured | Not measured | Not determined | No dated XLM/USD quote |
| Fresh | $0.010 | Not measured | Not measured | Not determined | No dated XLM/USD quote |

## Conclusion

The only fully measured `upto` path is CPU: `authorize` at 713,619 and `settle`
at 985,389 instructions, 1,699,008 combined (1.70% of the 100,000,000-instruction
per-transaction limit). Memory, ledger I/O and fees are unmeasured for
`authorize`/`settle`, and the USD economic comparison is blocked on a dated
XLM/USD quote. The baselines that do exist for `refund`, `anchor_batch`,
`verify_receipt` and `verify_receipt_leaf` are listed above with their file,
line, profile and measurement date. No figure in this document is inferred or
aspirational.
