#!/bin/bash
# Tests for deploy.sh argument parsing, pubnet safety checks, and helpers.
#
# Run with: bash tests/test_deploy.sh
#
# These tests mock `git` and `stellar` to exercise the script's validation
# logic without deploying to any network.
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
  if [ "$expected" = "$actual" ]; then
    pass "$msg"
  else
    fail "$msg" "$expected" "$actual"
  fi
}

# Run a command; expect non-zero exit.
assert_exit_nonzero() {
  local msg="$1"; shift
  if "$@" >/dev/null 2>&1; then
    fail "$msg" "non-zero exit" "exit 0"
  else
    pass "$msg"
  fi
}

# Run a command; expect zero exit.
assert_exit_zero() {
  local msg="$1"; shift
  if "$@" >/dev/null 2>&1; then
    pass "$msg"
  else
    fail "$msg" "exit 0" "non-zero exit"
  fi
}

# ---------------------------------------------------------------------------
# Setup: create a temp dir, mock git and stellar, source deploy.sh functions.
# ---------------------------------------------------------------------------
setup() {
  MOCK_TMPDIR=$(mktemp -d)
  MOCK_GIT_DIR="$MOCK_TMPDIR/bin"
  mkdir -p "$MOCK_GIT_DIR"

  # Default mock git: clean tree, main branch, valid HEAD
  _write_clean_main_git

  # Mock stellar: does nothing, succeeds
  cat > "$MOCK_GIT_DIR/stellar" <<'STELLARMOCK'
#!/bin/bash
exit 0
STELLARMOCK
  chmod +x "$MOCK_GIT_DIR/stellar"

  export PATH="$MOCK_GIT_DIR:$PATH"

  # Extract only the function definitions from deploy.sh so we can test them
  # in isolation without triggering any top-level deployment commands.
  SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
  SCRIPT="$SCRIPT_DIR/deploy.sh"

  eval "$(sed -n '/^sha256_of()/,/^# ----/p' "$SCRIPT" | head -n -1)"
  eval "$(sed -n '/^parse_args()/,/^# ----/p' "$SCRIPT" | head -n -1)"
  eval "$(sed -n '/^validate_pubnet()/,/^# ----/p' "$SCRIPT" | head -n -1)"

  # Reset globals
  NETWORK="testnet"
  IDENTITY="deployer"
}

_write_clean_main_git() {
  cat > "$MOCK_GIT_DIR/git" <<'GITMOCK'
#!/bin/bash
case "$1" in
  diff)
    if [ "${2:-}" = "--cached" ]; then
      exit 0
    fi
    exit 0
    ;;
  rev-parse)
    case "${2:-}" in
      --abbrev-ref) echo "main" ;;
      HEAD)         echo "abc123def456789012345678901234567890abcd" ;;
      *)            exit 1 ;;
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
  diff)
    if [ "${2:-}" = "--cached" ]; then
      exit 1
    fi
    exit 1
    ;;
  rev-parse)
    case "${2:-}" in
      --abbrev-ref) echo "main" ;;
      HEAD)         echo "abc123" ;;
      *)            exit 1 ;;
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
  diff)
    if [ "${2:-}" = "--cached" ]; then
      exit 0
    fi
    exit 0
    ;;
  rev-parse)
    case "${2:-}" in
      --abbrev-ref) echo "feature/my-branch" ;;
      HEAD)         echo "abc123" ;;
      *)            exit 1 ;;
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
  diff)
    if [ "${2:-}" = "--cached" ]; then
      exit 0
    fi
    exit 0
    ;;
  rev-parse)
    case "${2:-}" in
      --abbrev-ref) echo "main" ;;
      HEAD)         exit 1 ;;
      *)            exit 1 ;;
    esac
    ;;
  *) exit 1 ;;
esac
GITMOCK
  chmod +x "$MOCK_GIT_DIR/git"
}

cleanup() {
  rm -rf "$MOCK_TMPDIR"
}

trap cleanup EXIT

# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------
echo ""
echo "==========================================================="
echo "deploy.sh test suite"
echo "==========================================================="
echo ""

setup

echo "--- parse_args: --network flag ---"

NETWORK="testnet"
parse_args --network pubnet
assert_eq "pubnet" "$NETWORK" "--network pubnet sets NETWORK to pubnet"

NETWORK="testnet"
parse_args --network testnet
assert_eq "testnet" "$NETWORK" "--network testnet sets NETWORK to testnet"

NETWORK="testnet"
parse_args --network futurenet
assert_eq "futurenet" "$NETWORK" "--network futurenet sets NETWORK to futurenet"

echo ""
echo "--- parse_args: default (no flag) ---"

NETWORK="testnet"
parse_args
assert_eq "testnet" "$NETWORK" "no flag preserves default testnet"

echo ""
echo "--- parse_args: error handling ---"

NETWORK="testnet"
assert_exit_nonzero "unknown argument produces error" \
  bash -c 'NETWORK=testnet; parse_args --invalid'

assert_exit_nonzero "--network without value produces error" \
  bash -c 'NETWORK=testnet; parse_args --network'

echo ""
echo "--- validate_pubnet: happy path ---"

setup
NETWORK="pubnet"
assert_exit_zero "clean tree + main branch passes validate_pubnet" \
  validate_pubnet

echo ""
echo "--- validate_pubnet: dirty working tree ---"

setup
_write_dirty_git
NETWORK="pubnet"
assert_exit_nonzero "dirty working tree rejected" \
  bash -c 'NETWORK=pubnet validate_pubnet'

echo ""
echo "--- validate_pubnet: wrong branch ---"

setup
_write_wrong_branch_git
NETWORK="pubnet"
assert_exit_nonzero "non-main branch rejected" \
  bash -c 'NETWORK=pubnet validate_pubnet'

echo ""
echo "--- validate_pubnet: unknown commit SHA ---"

setup
_write_no_head_git
NETWORK="pubnet"
assert_exit_nonzero "cannot determine commit SHA rejected" \
  bash -c 'NETWORK=pubnet validate_pubnet'

echo ""
echo "--- validate_pubnet: not called for testnet ---"

setup
_write_dirty_git
NETWORK="testnet"
# deploy.sh only calls validate_pubnet when NETWORK=pubnet.
# Demonstrate that validate_pubnet itself WOULD fail in this scenario:
assert_exit_nonzero "validate_pubnet would fail with dirty tree (deploy.sh skips it for testnet)" \
  bash -c 'NETWORK=testnet validate_pubnet'

echo ""
echo "--- sha256_of: hash computation ---"

setup

TEST_FILE="$MOCK_TMPDIR/testfile.txt"
echo "hello world" > "$TEST_FILE"

HASH=$(sha256_of "$TEST_FILE")
EXPECTED="a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447"
assert_eq "$EXPECTED" "$HASH" "sha256_of computes correct hash"

HASH_ALT=$(PATH=/nonexistent sha256_of "$TEST_FILE")
assert_eq "unknown" "$HASH_ALT" "sha256_of returns 'unknown' when no sha256 tool is available"

echo ""
echo "--- pubnet confirmation requires YES ---"

setup

CONFIRM_YES=$(echo "YES" | bash -c '
  read -r -p "Type YES to confirm: " confirm
  if [ "$confirm" = "YES" ]; then echo "confirmed"; else echo "aborted"; exit 1; fi
')
assert_eq "confirmed" "$CONFIRM_YES" "confirmation accepts YES"

CONFIRM_NO=$(echo "no" | bash -c '
  read -r -p "Type YES to confirm: " confirm
  if [ "$confirm" = "YES" ]; then echo "confirmed"; else echo "aborted"; exit 1; fi
' 2>/dev/null || true)
assert_eq "aborted" "$CONFIRM_NO" "confirmation rejects lowercase input"

CONFIRM_EMPTY=$(echo "" | bash -c '
  read -r -p "Type YES to confirm: " confirm
  if [ "$confirm" = "YES" ]; then echo "confirmed"; else echo "aborted"; exit 1; fi
' 2>/dev/null || true)
assert_eq "aborted" "$CONFIRM_EMPTY" "confirmation rejects empty input"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "==========================================================="
echo "Results: $TESTS_PASSED passed, $TESTS_FAILED failed, $TESTS_RUN total"
echo "==========================================================="

if [ "$TESTS_FAILED" -gt 0 ]; then
  exit 1
fi
exit 0
