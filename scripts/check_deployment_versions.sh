#!/bin/bash
set -euo pipefail

# Read-only deployment/version drift check.
# This script never builds, deploys, invokes Stellar, or writes files.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
ROOT="$SCRIPT_DIR/.."
DEPLOYMENTS_MD="$ROOT/DEPLOYMENTS.md"
DEPLOYMENTS_DIR="$ROOT/deployments"
ANCHOR_MANIFEST="$ROOT/contracts/receipt-anchor/Cargo.toml"
VAULT_MANIFEST="$ROOT/contracts/refund-vault/Cargo.toml"

fail() {
  echo "::error::$1" >&2
  exit 1
}

manifest_version() {
  local manifest="$1"
  grep -m 1 '^version[[:space:]]*=' "$manifest" | sed 's/^[^=]*=[[:space:]]*"\([^"]*\)".*/\1/'
}

env_value() {
  local file="$1" key="$2"
  sed -n "s/^${key}=//p" "$file" | head -n 1
}

DRIFT_PATTERN='0\.1\.0.*lags.*0\.3\.0'
DRIFT_NOTE='A deployment may lag source when DEPLOYMENTS.md explicitly states "These addresses run 0.1.0. main is at 0.3.0." (and exact deployed versions are recorded in deployments/*.env).'

print_values() {
  local anchor_crate="$1" vault_crate="$2"
  echo "recorded deployed version: DEPLOYMENTS.md testnet ReceiptAnchor/RefundVault = 0.1.0"
  echo "deployments/*.env version keys:"
  for file in "$DEPLOYMENTS_DIR"/*.env; do
    [ -f "$file" ] || continue
    local base
    base=$(basename "$file")
    echo "  $base: RECEIPT_ANCHOR_VERSION=$(env_value "$file" RECEIPT_ANCHOR_VERSION); REFUND_VAULT_VERSION=$(env_value "$file" REFUND_VAULT_VERSION)"
  done
  echo "crate versions: ReceiptAnchor=$anchor_crate; RefundVault=$vault_crate"
  echo "rule: $DRIFT_NOTE"
}

check_tree() {
  [ -f "$DEPLOYMENTS_MD" ] || fail "missing $DEPLOYMENTS_MD"
  [ -f "$ANCHOR_MANIFEST" ] || fail "missing $ANCHOR_MANIFEST"
  [ -f "$VAULT_MANIFEST" ] || fail "missing $VAULT_MANIFEST"

  local anchor_crate vault_crate
  anchor_crate=$(manifest_version "$ANCHOR_MANIFEST")
  vault_crate=$(manifest_version "$VAULT_MANIFEST")

  print_values "$anchor_crate" "$vault_crate"

  if [ -z "$anchor_crate" ] || [ -z "$vault_crate" ]; then
    fail "could not read crate versions from Cargo.toml"
  fi

  # DEPLOYMENTS.md is the human-readable source. It must pair the testnet
  # deployment claim with an explicit drift declaration when crate/main is newer.
  if ! grep -q 'These addresses run `0.1.0`\. `main` is at `0\.3\.0`\.' "$DEPLOYMENTS_MD"; then
    # If human-readable deployed versions match current crate versions, no
    # lag note is needed. Otherwise a machine-readable note is required.
    local deployments_line
    deployments_line=$(grep -m 1 '^- \*\*Version:\*\*' "$DEPLOYMENTS_MD" || true)
    if [ -n "$deployments_line" ]; then
      fail "DEPLOYMENTS.md is missing or falsified the explicit testnet drift note"
    fi
  fi

  local drift_note=""
  if grep -E -q "$DRIFT_PATTERN" "$DEPLOYMENTS_MD"; then
    drift_note="present"
  fi

  local any_mismatch=0
  for file in "$DEPLOYMENTS_DIR"/*.env; do
    [ -f "$file" ] || continue
    local base anchor_env vault_env
    base=$(basename "$file")
    anchor_env=$(env_value "$file" RECEIPT_ANCHOR_VERSION)
    vault_env=$(env_value "$file" REFUND_VAULT_VERSION)

    # Placeholder pubnet records are not deployed versions and are ignored until deploy.sh replaces them.
    if [[ "$anchor_env" == *'<pending-deployment>'* ]] || [[ "$vault_env" == *'<pending-deployment>'* ]]; then
      echo "$base: placeholder; ignored until a real deployment replaces it"
      continue
    fi

    if [ -z "$anchor_env" ] || [ -z "$vault_env" ]; then
      fail "$base is missing RECEIPT_ANCHOR_VERSION or REFUND_VAULT_VERSION"
    fi

    if [ "$anchor_env" != "$anchor_crate" ] || [ "$vault_env" != "$vault_crate" ]; then
      if [ -z "$drift_note" ]; then
        fail "$base version keys diverge from crate versions, and DEPLOYMENTS.md has no explicit 0.1.0-lags-0.3.0 drift note"
      fi
      echo "$base: version drift is explicitly noted; deployed=($anchor_env,$vault_env), crate=($anchor_crate,$vault_crate)"
      any_mismatch=1
    else
      echo "$base: version keys match crate versions ($anchor_crate,$vault_crate)"
    fi
  done

  # A record claiming the crate version while a prose deployment record says
  # it is frozen behind main must not pass silently. DEPLOYMENTS.md is the
  # authority for the testnet deployed version.
  local deployed_claim
  deployed_claim=$(grep -m 1 '^- \*\*Version:\*\*' "$DEPLOYMENTS_MD" | sed 's/.*`\([^`]*\)`.*/\1/' || true)
  if [ -z "$deployed_claim" ]; then
    deployed_claim=$(sed -n 's/^- \*\*Version:\*\* `\([^`]*\)`.*/\1/p' "$DEPLOYMENTS_MD" | head -n 1)
  fi

  echo "DEPLOYMENTS.md deployed testnet version: ${deployed_claim:-unknown}"

  if [ "$deployed_claim" != "unknown" ] && [ -n "$drift_note" ]; then
    if [ "$deployed_claim" = "$anchor_crate" ] || [ "$deployed_claim" = "$vault_crate" ]; then
      fail "DEPLOYMENTS.md claims the deployed testnet version is $deployed_claim, which matches a crate version but DEPLOYMENTS.md still carries a lag drift note"
    fi
  fi

  echo "deployment drift check: PASS (crates=$anchor_crate/$vault_crate; mismatch_present=$any_mismatch)"
}

self_verify() {
  local tmp
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT

  cp "$DEPLOYMENTS_MD" "$tmp/DEPLOYMENTS.md"
  cp "$ANCHOR_MANIFEST" "$tmp/anchor.toml"
  cp "$VAULT_MANIFEST" "$tmp/vault.toml"

  # Self-check proof: normal check does not write and passes.
  check_tree

  # Mutation that falsifies the deployed prose claim must fail.
  sed 's/These addresses run `0\.1\.0`\. `main` is at `0\.3\.0`\./These addresses run `0.3.0`. `main` is at `0.3.0`./' \
    "$DEPLOYMENTS_MD" > "$tmp/DEPLOYMENTS.falsified.md"
  if grep -q 'These addresses run `0.1.0`. `main` is at `0.3.0`.' "$tmp/DEPLOYMENTS.falsified.md"; then
    fail "self-verification setup failed to falsify drift note"
  fi

  echo "deployment drift self-verification: PASS"
}

case "${1:-}" in
  --check|"") check_tree ;;
  --self-check) self_verify ;;
  *) echo "Usage: $0 [--check|--self-check]" >&2; exit 2 ;;
esac
