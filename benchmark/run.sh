#!/usr/bin/env bash
set -euo pipefail

# Runs the resource-cost measurements this repository re-baselines against.
#
# The file used to hold a single line — `// benchmark suite` — and no commands,
# so `bash benchmark/run.sh` died on that line while still being the only thing
# named "benchmark" in the tree. The three measurements below are the ones
# CONTRIBUTING.md ("Resource Cost Baselines & Re-baselining") and
# docs/BENCHMARKS.md tell a contributor to run when a cost regression fires, so
# this is now the entry point its name promises.
#
# `set -euo pipefail` is what makes it usable as a gate: the first measurement
# that fails aborts the run, instead of all three printing and the script
# reporting success over a half-broken suite. The debug profile is deliberate —
# the committed baselines in the `fuzz_test.rs` files were measured against
# unoptimised builds, which is what CONTRIBUTING.md prescribes for re-measuring.

# `anchor_batch` / `verify_receipt`: prints the measured CPU and memory against
# ANCHOR_BATCH_BASELINE_* and VERIFY_RECEIPT_BASELINE_* in
# contracts/receipt-anchor/src/fuzz_test.rs.
cargo test --locked --package receipt-anchor --lib -- \
  benchmark_gas_and_cpu_instructions --nocapture

# Merkle verification across batch sizes 1, 10, 25, 50 and 100, the series
# docs/BENCHMARKS.md publishes per depth.
cargo test --locked --package receipt-anchor --lib -- \
  test_verify_receipt_batch_size_instruction_benchmark --nocapture

# `refund`: prints the measured cost against REFUND_BASELINE_CPU and
# REFUND_BASELINE_MEM in contracts/refund-vault/src/fuzz_test.rs.
cargo test --locked --package refund-vault --lib -- \
  test_refund_resource_cost_budget --nocapture
