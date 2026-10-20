#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

WASM_DIR="target/wasm32v1-none/release"
BUDGET_FILE=".wasm-budget.json"
FAILED=0

echo "### WASM Binary Size & Budget Check"
echo ""

if [ ! -f "$BUDGET_FILE" ]; then
  echo "Error: $BUDGET_FILE not found." >&2
  exit 1
fi

# Resolving budget entries requires jq. A missing or broken jq used to silently
# disable the per-contract budget comparison (the `|| echo null` fallback
# swallowed its failure and the `[ -gt ]` on "null" errored silently), so it is
# now a hard failure: the gate runs, but every contract fails its budget check.
JQ_OK=1
if ! command -v jq >/dev/null 2>&1; then
  echo "Error: jq is required to resolve budget entries in $BUDGET_FILE." >&2
  JQ_OK=0
  FAILED=1
elif ! jq -e . "$BUDGET_FILE" >/dev/null 2>&1; then
  echo "Error: $BUDGET_FILE is not valid JSON." >&2
  JQ_OK=0
  FAILED=1
fi

CONTRACTS=("receipt_anchor" "refund_vault" "refund_policy_time" "refund_policy_vdf" "refund_vault_factory" "governance" "multisig_account" "receipt_shard" "state_channel" "stream_vault" "upto_authorization")

# The checked set is the union of the hardcoded contract list above and every
# key present in .wasm-budget.json, so a budget entry with no hardcoded entry
# is reported (and fails) instead of silently ignored. A rename or a new
# contract can no longer remove itself from this gate.
if [ "$JQ_OK" -eq 1 ]; then
  CHECKED_CONTRACTS=()
  while IFS= read -r contract; do
    CHECKED_CONTRACTS+=("$contract")
  done < <(printf '%s\n' "${CONTRACTS[@]}" "$(jq -r 'keys[]' "$BUDGET_FILE")" | sort -u)
else
  CHECKED_CONTRACTS=("${CONTRACTS[@]}")
fi

# soroban-env-host refuses to upload a contract whose code entry does not fit
# one ledger entry: "contract code entry with key 'ContractCode(..)' size: N >
# 131072". The entry wraps the wasm in a fixed envelope the host measured at
# 101 bytes (a 138615-byte refund_vault blob produced an entry size of
# 138716), so the wasm itself has to stay under the difference. This gate is
# independent of .wasm-budget.json: exceeding it means the contract cannot be
# deployed at all, not merely that it grew past its baseline.
MAX_LEDGER_ENTRY_BYTES=131072
CODE_ENTRY_ENVELOPE_BYTES=101
MAX_WASM_BYTES=$(( MAX_LEDGER_ENTRY_BYTES - CODE_ENTRY_ENVELOPE_BYTES ))

echo "#### Binary Sizes vs Budget"
echo ""
echo "| Contract | Size (bytes) | Budget (bytes) | Status |"
echo "|---|---|---|---|"

for CONTRACT in "${CHECKED_CONTRACTS[@]}"; do
  WASM_FILE="${WASM_DIR}/${CONTRACT}.wasm"
  if [ ! -f "$WASM_FILE" ]; then
    echo "| \`${CONTRACT}\` | N/A | - | ❌ Not Built |" >&2
    FAILED=1
    continue
  fi

  SIZE=$(stat -c%s "$WASM_FILE" 2>/dev/null || stat -f%z "$WASM_FILE" 2>/dev/null || echo "0")
  if [ "$JQ_OK" -eq 1 ]; then
    BUDGET=$(jq -r --arg key "$CONTRACT" '.[$key] // empty' "$BUDGET_FILE")
  else
    BUDGET=""
  fi

  if [ "$SIZE" -gt "$MAX_WASM_BYTES" ]; then
    echo "| \`${CONTRACT}\` | $SIZE | ${BUDGET:-N/A} | ❌ Over deploy limit (> $MAX_WASM_BYTES) |" >&2
    echo "Error: ${CONTRACT}.wasm is $SIZE bytes; its code entry would be $(( SIZE + CODE_ENTRY_ENVELOPE_BYTES )) > $MAX_LEDGER_ENTRY_BYTES and the network will reject the upload." >&2
    FAILED=1
  elif [ -z "$BUDGET" ] || ! [[ "$BUDGET" =~ ^[0-9]+$ ]]; then
    echo "| \`${CONTRACT}\` | $SIZE | N/A | ❌ No valid budget |" >&2
    FAILED=1
  elif [ "$SIZE" -gt "$BUDGET" ]; then
    echo "| \`${CONTRACT}\` | $SIZE | $BUDGET | ❌ Exceeded |"
    FAILED=1
  else
    echo "| \`${CONTRACT}\` | $SIZE | $BUDGET | ✅ Within Budget |"
  fi
done

echo ""

if command -v soroban &>/dev/null; then
  echo "#### soroban contract inspect --wasm Budget Analysis"
  echo ""

  for CONTRACT in "${CHECKED_CONTRACTS[@]}"; do
    WASM_FILE="${WASM_DIR}/${CONTRACT}.wasm"
    if [ ! -f "$WASM_FILE" ]; then
      continue
    fi

    echo "--- $CONTRACT ---"
    soroban contract inspect --wasm "$WASM_FILE" 2>&1 || true
    echo ""
  done
else
  echo "⚠ soroban CLI not found; skipping \`soroban contract inspect --wasm\` analysis."
  echo "   Install stellar-sdk or stellar-cli to enable CPU/memory instruction limit checks."
fi

if [ "$FAILED" -ne 0 ]; then
  echo "Error: WASM budget gate failed: one or more contracts are missing, unmapped, or exceeded their budget." >&2
  exit 1
fi

echo "All WASM binaries within budget."
