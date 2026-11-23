#!/bin/bash
# Tests for deploy.sh argument parsing, pubnet safety checks, and helpers.
set -euo pipefail

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_PASSED=$((TESTS_PASSED + 1))
  echo "  ✓ $1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  echo "  ✗ $1"
  [ -n "${2:-}" ] && echo "    expected: $2" && echo "    got:      $3"
}

assert_eq() {
  local expected="$1" actual="$2" msg="$3"
  if [ "$expected" = "$actual" ]; then pass "$msg"; else fail "$msg" "$expected" "$actual"; fi
}

assert_exit_nonzero() {
  local msg="$1"; shift
  if "$@" >/dev/null 2>&1; then fail "$msg" "non-zero exit" "exit 0"; else pass "$msg"; fi
}

assert_exit_zero() {
  local msg="$1"; shift
  if "$@" >/dev/null 2>&1; then pass "$msg"; else fail "$msg" "exit 0" "non-zero exit"; fi
}

assert_output_contains() {
  local needle="$1" msg="$2"; shift 2
  local output
  if output=$("$@" 2>&1); then
    fail "$msg" "non-zero exit containing '$needle'" "exit 0"
  elif [[ "$output" == *"$needle"* ]]; then
    pass "$msg"
  else
    fail "$msg" "output containing '$needle'" "$output"
  fi
}

setup() {
  MOCK_TMPDIR=$(mktemp -d)
  MOCK_GIT_DIR="$MOCK_TMPDIR/bin"
  mkdir -p "$MOCK_GIT_DIR"
  mkdir -p "$MOCK_TMPDIR/deployments"
  OUT_DIR="$MOCK_TMPDIR/deployments"

  _write_clean_main_git

  cat > "$MOCK_GIT_DIR/stellar" <<'STELLARMOCK'
#!/bin/bash
exit 0
STELLARMOCK
  chmod +x "$MOCK_GIT_DIR/stellar"

  cat > "$MOCK_GIT_DIR/curl" <<'CURLMOCK'
#!/bin/bash
exit 0
CURLMOCK
  chmod +x "$MOCK_GIT_DIR/curl"

  export PATH="$MOCK_GIT_DIR:$PATH"

  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
  SCRIPT="$SCRIPT_DIR/deploy.sh"

  eval "$(sed -n '/^sha256_of()/,/^# ----/p' "$SCRIPT" | head -n -1)"
  eval "$(sed -n '/^is_checksum_valid_contract_id()/,/^}/p' "$SCRIPT")"
  eval "$(sed -n '/^validate_pubnet_config()/,/^}/p' "$SCRIPT")"
  eval "$(sed -n '/^probe_pubnet_network()/,/^}/p' "$SCRIPT")"
  eval "$(sed -n '/^validate_pubnet_network()/,/^}/p' "$SCRIPT")"
  eval "$(sed -n '/^validate_pubnet_artifacts()/,/^}/p' "$SCRIPT")"
  eval "$(sed -n '/^parse_args()/,/^# ----/p' "$SCRIPT" | head -n -1)"
  eval "$(sed -n '/^validate_pubnet()/,/^# ----/p' "$SCRIPT" | head -n -1)"

  NETWORK="testnet"
  IDENTITY="deployer"
  REFUND_WINDOW_LEDGERS="17280"
  TOKEN=$(python3 - <<'PY'
import sys
alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
body=bytes([16])+bytes(range(32))
crc=0
for byte in body:
    crc ^= byte << 8
    for _ in range(8):
        if crc & 0x8000:
            crc=((crc << 1)^0x1021)&0xffff
        else:
            crc=(crc << 1)&0xffff
raw=body+crc.to_bytes(2,"little")
bits=int.from_bytes(raw,"big")
print("".join(alphabet[(bits>>(5*(55-index)))&31] for index in range(56)))
PY
)

  ANCHOR_WASM="$MOCK_TMPDIR/receipt_anchor.wasm"
  VAULT_WASM="$MOCK_TMPDIR/refund_vault.wasm"
  printf "anchor" > "$ANCHOR_WASM"
  printf "vault" > "$VAULT_WASM"
  ANCHOR_HASH=$(printf "%.064d" 0)
  VAULT_HASH=$(printf "%.064d" 0)

  cat > "$OUT_DIR/pubnet.env" <<'ENV'
NEXT_PUBLIC_RECEIPT_ANCHOR_ID=CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
NEXT_PUBLIC_REFUND_VAULT_ID=CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
MERCHANT_ADDRESS=GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
TOKEN_ADDRESS=CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
REFUND_WINDOW_LEDGERS=17280
RECEIPT_ANCHOR_VERSION=0.3.0
RECEIPT_ANCHOR_WASM_HASH=0000000000000000000000000000000000000000000000000000000000000000
REFUND_VAULT_VERSION=0.3.0
REFUND_VAULT_WASM_HASH=0000000000000000000000000000000000000000000000000000000000000000
ENV
}

_write_clean_main_git() {
  cat > "$MOCK_GIT_DIR/git" <<'GITMOCK'
#!/bin/bash
case "$1" in
  diff)
    exit 0
    ;;
  rev-parse)
    case "${2:-}" in
      --abbrev-ref) echo "main" ;;
      HEAD) echo "abc123def456789012345678901234567890abcd" ;;
      *) exit 1 ;;
    esac
    ;;
  *) exit 1 ;;
esac
GITMOCK
  chmod +x "$MOCK_GIT_DIR/git"
}

_write_dirty_git() {
  cat > "$MOCK_GIT_DIR/git" <<'GITMOCK'
#!/bin/bash
case "$1" in
  diff) exit 1 ;;
  rev-parse)
    case "${2:-}" in
      --abbrev-ref) echo "main" ;;
      HEAD) echo "abc123" ;;
      *) exit 1 ;;
    esac
    ;;
  *) exit 1 ;;
esac
GITMOCK
  chmod +x "$MOCK_GIT_DIR/git"
}

_write_wrong_branch_git() {
  cat > "$MOCK_GIT_DIR/git" <<'GITMOCK'
#!/bin/bash
case "$1" in
  diff) exit 0 ;;
  rev-parse)
    case "${2:-}" in
      --abbrev-ref) echo "feature/my-branch" ;;
      HEAD) echo "abc123" ;;
      *) exit 1 ;;
    esac
    ;;
  *) exit 1 ;;
esac
GITMOCK
  chmod +x "$MOCK_GIT_DIR/git"
}

_write_no_head_git() {
  cat > "$MOCK_GIT_DIR/git" <<'GITMOCK'
#!/bin/bash
case "$1" in
  diff) exit 0 ;;
  rev-parse)
    case "${2:-}" in
      --abbrev-ref) echo "main" ;;
      HEAD) exit 1 ;;
      *) exit 1 ;;
    esac
    ;;
  *) exit 1 ;;
esac
GITMOCK
  chmod +x "$MOCK_GIT_DIR/git"
}

_write_unreachable_pubnet() {
  cat > "$MOCK_GIT_DIR/curl" <<'CURLMOCK'
#!/bin/bash
exit 1
CURLMOCK
  chmod +x "$MOCK_GIT_DIR/curl"
}

cleanup() { rm -rf "$MOCK_TMPDIR"; }
trap cleanup EXIT

echo ""
echo "==========================================================="
echo "deploy.sh test suite"
echo "==========================================================="
echo ""

setup

echo "--- parse_args: --network flag ---"
NETWORK="testnet"; parse_args --network pubnet
assert_eq "pubnet" "$NETWORK" "--network pubnet sets NETWORK"
NETWORK="testnet"; parse_args --network testnet
assert_eq "testnet" "$NETWORK" "--network testnet sets NETWORK"
NETWORK="testnet"; parse_args --network futurenet
assert_eq "futurenet" "$NETWORK" "--network futurenet sets NETWORK"

setup
NETWORK="testnet"; parse_args
assert_eq "testnet" "$NETWORK" "no flag preserves default testnet"

setup
NETWORK="testnet"; assert_exit_nonzero "unknown argument produces error" bash -c 'NETWORK=testnet; parse_args --invalid'
setup
NETWORK="testnet"; assert_exit_nonzero "--network without value produces error" bash -c 'NETWORK=testnet; parse_args --network'

echo ""
echo "--- validate_pubnet: happy path ---"
setup
NETWORK="pubnet"
assert_exit_zero "clean tree + main branch + valid pubnet config passes" validate_pubnet

setup
_write_dirty_git
NETWORK="pubnet"
assert_exit_nonzero "dirty working tree rejected" bash -c 'NETWORK=pubnet validate_pubnet'

setup
_write_wrong_branch_git
NETWORK="pubnet"
assert_exit_nonzero "non-main branch rejected" bash -c 'NETWORK=pubnet validate_pubnet'

setup
_write_no_head_git
NETWORK="pubnet"
assert_exit_nonzero "cannot determine commit SHA rejected" bash -c 'NETWORK=pubnet validate_pubnet'

echo ""
echo "--- pubnet placeholders ---"
setup
sed -i 's/^TOKEN_ADDRESS=.*/TOKEN_ADDRESS=<pending-deployment>/' "$OUT_DIR/pubnet.env"
NETWORK="pubnet"
assert_output_contains "TOKEN_ADDRESS is still <pending-deployment>" "placeholder key named in failure" bash -c 'NETWORK=pubnet OUT_DIR="$1" validate_pubnet' _ "$OUT_DIR"

echo ""
echo "--- pubnet token validation ---"
setup
TOKEN="not-a-contract"
NETWORK="pubnet"
assert_output_contains "TOKEN is not a checksum-valid" "malformed token rejected" bash -c 'NETWORK=pubnet TOKEN="$1" OUT_DIR="$2" validate_pubnet' _ "$TOKEN" "$OUT_DIR"

setup
TOKEN="${TOKEN%?}X"
NETWORK="pubnet"
assert_output_contains "TOKEN is not a checksum-valid" "checksum-invalid token rejected" bash -c 'NETWORK=pubnet TOKEN="$1" OUT_DIR="$2" validate_pubnet' _ "$TOKEN" "$OUT_DIR"

setup
TOKEN="CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC"
NETWORK="pubnet"
assert_output_contains "testnet token contract" "testnet token rejected" bash -c 'NETWORK=pubnet TOKEN="$1" OUT_DIR="$2" validate_pubnet' _ "$TOKEN" "$OUT_DIR"

echo ""
echo "--- pubnet refund window validation ---"
for window in 0 -1 abc; do
  setup
  REFUND_WINDOW_LEDGERS="$window"
  assert_output_contains "REFUND_WINDOW_LEDGERS must be a positive integer" "invalid refund window $window rejected" bash -c 'NETWORK=pubnet REFUND_WINDOW_LEDGERS="$1" OUT_DIR="$2" validate_pubnet' _ "$window" "$OUT_DIR"
done

echo ""
echo "--- pubnet network reachability ---"
setup
_write_unreachable_pubnet
NETWORK="pubnet"
assert_output_contains "Pubnet network is unreachable" "unreachable pubnet rejected" bash -c 'NETWORK=pubnet OUT_DIR="$1" validate_pubnet' _ "$OUT_DIR"

echo ""
echo "--- pubnet artifacts ---"
setup
rm -f "$ANCHOR_WASM"
assert_output_contains "Missing WASM artifact" "missing artifact rejected" bash -c 'ANCHOR_WASM="$1" VAULT_WASM="$2" ANCHOR_HASH="$3" VAULT_HASH="$4" validate_pubnet_artifacts' _ "$ANCHOR_WASM" "$VAULT_WASM" "$ANCHOR_HASH" "$VAULT_HASH"

setup
ANCHOR_HASH="unknown"
assert_output_contains "artifact hash is unknown or invalid" "unknown anchor hash rejected" bash -c 'ANCHOR_WASM="$1" VAULT_WASM="$2" ANCHOR_HASH="$3" VAULT_HASH="$4" validate_pubnet_artifacts' _ "$ANCHOR_WASM" "$VAULT_WASM" "$ANCHOR_HASH" "$VAULT_HASH"

setup
VAULT_HASH="unknown"
assert_output_contains "artifact hash is unknown or invalid" "unknown vault hash rejected" bash -c 'ANCHOR_WASM="$1" VAULT_WASM="$2" ANCHOR_HASH="$3" VAULT_HASH="$4" validate_pubnet_artifacts' _ "$ANCHOR_WASM" "$VAULT_WASM" "$ANCHOR_HASH" "$VAULT_HASH"

echo ""
echo "--- sha256_of: hash computation ---"
setup
TEST_FILE="$MOCK_TMPDIR/testfile.txt"
echo "hello world" > "$TEST_FILE"
HASH=$(sha256_of "$TEST_FILE")
EXPECTED="a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447"
assert_eq "$EXPECTED" "$HASH" "sha256_of computes correct hash"
HASH_ALT=$(PATH=/nonexistent sha256_of "$TEST_FILE")
assert_eq "unknown" "$HASH_ALT" "sha256_of returns unknown without sha256 tool"

echo ""
echo "==========================================================="
echo "Results: $TESTS_PASSED passed, $TESTS_FAILED failed, $TESTS_RUN total"
echo "==========================================================="

if [ "$TESTS_FAILED" -gt 0 ]; then exit 1; fi
exit 0
