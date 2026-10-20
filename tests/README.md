# Tests & CI Test Harnesses

This directory contains test suites and verification harnesses that validate deployments and protocol invariants.

## Deploy Smoke Test (`tests/test_deploy.sh`)

Exercises the argument parsing, environment validation, pubnet safety protections, and SHA-256 hash helpers of `deploy.sh` in dry-run mode using mocked `git` and `stellar` binaries. It ensures the deployment pipeline validates configurations without communicating with real networks or spending funds.

### Running Locally

```bash
bash tests/test_deploy.sh
```

## Differential Invariant Fuzz Harness (`tests/differential/bolero_test.rs`)

Executes a bounded differential invariant fuzz campaign across protocol float conservation and Merkle sorted-pair ordering invariants.

- **Default iterations in CI:** 1,000 iterations (runtime < 0.1s, well within CI budget).
- **Custom campaign iterations:** Configurable via the `BOLERO_ITERATIONS` environment variable.

### Running Locally

```bash
rustc --test tests/differential/bolero_test.rs -o target/bolero_test && ./target/bolero_test --nocapture
```

To run a longer campaign (e.g., 50,000 iterations):

```bash
BOLERO_ITERATIONS=50000 ./target/bolero_test --nocapture
```
