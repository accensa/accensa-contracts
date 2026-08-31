#!/bin/bash
# Deploys the Accensa contracts to a Stellar network and records the resulting
# contract IDs to deployments/<network>.env, so every deployment leaves a
# committed, independently verifiable trail instead of scrolling past in stdout.
set -euo pipefail

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
NETWORK="${NETWORK:-testnet}"
IDENTITY="${IDENTITY:-deployer}"
# Testnet native XLM SAC. Override with TOKEN=... to use USDC or another asset.
TOKEN="${TOKEN:-CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC}"
# 17280 ledgers is approximately 1 day at ~5s per ledger.
REFUND_WINDOW_LEDGERS="${REFUND_WINDOW_LEDGERS:-17280}"

OUT_DIR="deployments"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Compute SHA-256 of a file in a portable way (Linux sha256sum / macOS shasum).
sha256_of() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    echo "unknown"
  fi
}

# Parse command-line arguments.  Safe to call from tests.
parse_args() {
  while [ $# -gt 0 ]; do
    case "$1" in
      --network)
        if [ -z "${2:-}" ]; then
          echo "Error: --network requires a value" >&2
          exit 1
        fi
        NETWORK="$2"
        shift 2
        ;;
      *)
        echo "Error: unknown argument '$1'" >&2
        echo "Usage: $0 [--network testnet|futurenet|pubnet]" >&2
        exit 1
        ;;
    esac
  done
}

# Validate pubnet deployment prerequisites.
# Exits with a non-zero status if any check fails.
validate_pubnet() {
  local errors=0

  echo ""
  echo "🔒 Pubnet pre-flight checks"
  echo "----------------------------------------------------------"

  # 1. Clean working tree
  if ! git diff --quiet 2>/dev/null; then
    echo "❌ FAIL: Working tree has uncommitted changes." >&2
    echo "   Commit or stash all changes before deploying to pubnet." >&2
    errors=$((errors + 1))
  fi
  if ! git diff --cached --quiet 2>/dev/null; then
    echo "❌ FAIL: Staged changes in working tree." >&2
    echo "   Commit or unstage all changes before deploying to pubnet." >&2
    errors=$((errors + 1))
  fi

  # 2. Current branch is main
  local branch
  branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "unknown")
  if [ "$branch" != "main" ]; then
    echo "❌ FAIL: Current branch is '$branch', not 'main'." >&2
    echo "   Pubnet deployments must originate from the main branch." >&2
    errors=$((errors + 1))
  fi

  # 3. Commit SHA
  local commit_sha
  commit_sha=$(git rev-parse HEAD 2>/dev/null || echo "unknown")
  if [ "$commit_sha" = "unknown" ]; then
    echo "❌ FAIL: Cannot determine commit SHA." >&2
    errors=$((errors + 1))
  fi

  if [ "$errors" -gt 0 ]; then
    echo "" >&2
    echo "❌ $errors pre-flight check(s) failed. Aborting." >&2
    exit 1
  fi

  echo "✅ Working tree is clean"
  echo "✅ Branch is main"
  echo "✅ Commit SHA: $commit_sha"
  echo ""
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
parse_args "$@"

# ---------------------------------------------------------------------------
# Pubnet safety gate: all validation and confirmation happens BEFORE any
# deploy command.  This is the single control point for pubnet access.
# ---------------------------------------------------------------------------
if [ "$NETWORK" = "pubnet" ]; then
  validate_pubnet
fi

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
echo "🚀 Building contracts..."
stellar contract build

# ---------------------------------------------------------------------------
# Compute WASM hashes (must be after build, before deploy)
# ---------------------------------------------------------------------------
ANCHOR_WASM="target/wasm32v1-none/release/receipt_anchor.wasm"
VAULT_WASM="target/wasm32v1-none/release/refund_vault.wasm"

ANCHOR_HASH=$(sha256_of "$ANCHOR_WASM")
VAULT_HASH=$(sha256_of "$VAULT_WASM")

ANCHOR_VERSION=$(grep -m 1 "^version" contracts/receipt-anchor/Cargo.toml | cut -d '"' -f 2)
VAULT_VERSION=$(grep -m 1 "^version" contracts/refund-vault/Cargo.toml | cut -d '"' -f 2)
COMMIT_SHA=$(git rev-parse HEAD 2>/dev/null || echo "unknown")

# ---------------------------------------------------------------------------
# Pubnet: display artifacts and require explicit WASM hash confirmation
# ---------------------------------------------------------------------------
if [ "$NETWORK" = "pubnet" ]; then
  echo ""
  echo "==========================================================="
  echo "⚠️  PUBNET DEPLOYMENT — ARTIFACT VERIFICATION"
  echo "==========================================================="
  echo ""
  echo "Contract versions: ReceiptAnchor $ANCHOR_VERSION, RefundVault $VAULT_VERSION"
  echo "Commit SHA:        $COMMIT_SHA"
  echo ""
  echo "WASM artifacts to deploy:"
  echo "  ReceiptAnchor  $ANCHOR_HASH  ($ANCHOR_WASM)"
  echo "  RefundVault    $VAULT_HASH   ($VAULT_WASM)"
  echo ""
  echo "Token:   $TOKEN"
  echo "Refund window: $REFUND_WINDOW_LEDGERS ledgers"
  echo ""
  read -r -p "Type YES to confirm these are the correct artifacts for pubnet: " confirm
  if [ "$confirm" != "YES" ]; then
    echo "Aborted: confirmation not received." >&2
    exit 1
  fi
  echo ""
fi

# ---------------------------------------------------------------------------
# Identity setup
# ---------------------------------------------------------------------------
echo "🔑 Using identity '$IDENTITY' on network '$NETWORK'..."
if ! stellar keys address "$IDENTITY" >/dev/null 2>&1; then
  echo "   Identity not found, generating..."
  stellar keys generate "$IDENTITY" --network "$NETWORK"
fi
DEPLOYER=$(stellar keys address "$IDENTITY")
echo "   Deployer address: $DEPLOYER"

if [ "$NETWORK" = "testnet" ] || [ "$NETWORK" = "futurenet" ]; then
  echo "💎 Ensuring deployer is funded..."
  stellar keys fund "$IDENTITY" --network "$NETWORK" || true
fi

# ---------------------------------------------------------------------------
# Deploy
# ---------------------------------------------------------------------------
echo "🚢 Deploying ReceiptAnchor..."
ANCHOR_ID=$(stellar contract deploy \
  --wasm "$ANCHOR_WASM" \
  --source "$IDENTITY" --network "$NETWORK" 2>/dev/null | tail -n 1)
echo "   ReceiptAnchor: $ANCHOR_ID"

echo "🚢 Deploying RefundVault..."
VAULT_ID=$(stellar contract deploy \
  --wasm "$VAULT_WASM" \
  --source "$IDENTITY" --network "$NETWORK" 2>/dev/null | tail -n 1)
echo "   RefundVault: $VAULT_ID"

echo "⚙️  Initializing ReceiptAnchor..."
stellar contract invoke --id "$ANCHOR_ID" --source "$IDENTITY" --network "$NETWORK" \
  -- initialize --merchant "$DEPLOYER"

echo "⚙️  Initializing RefundVault..."
stellar contract invoke --id "$VAULT_ID" --source "$IDENTITY" --network "$NETWORK" \
  -- initialize --merchant "$DEPLOYER" --token "$TOKEN" \
  --refund_window_ledgers "$REFUND_WINDOW_LEDGERS"

# ---------------------------------------------------------------------------
# Record deployment metadata
# ---------------------------------------------------------------------------
mkdir -p "$OUT_DIR"
OUT_FILE="$OUT_DIR/$NETWORK.env"

cat > "$OUT_FILE" <<EOF
# Generated by deploy.sh on $(date -u +"%Y-%m-%dT%H:%M:%SZ")
# Network: $NETWORK
# Commit: $COMMIT_SHA
NEXT_PUBLIC_RECEIPT_ANCHOR_ID=$ANCHOR_ID
NEXT_PUBLIC_REFUND_VAULT_ID=$VAULT_ID
MERCHANT_ADDRESS=$DEPLOYER
TOKEN_ADDRESS=$TOKEN
REFUND_WINDOW_LEDGERS=$REFUND_WINDOW_LEDGERS
RECEIPT_ANCHOR_VERSION=$ANCHOR_VERSION
RECEIPT_ANCHOR_WASM_HASH=$ANCHOR_HASH
REFUND_VAULT_VERSION=$VAULT_VERSION
REFUND_VAULT_WASM_HASH=$VAULT_HASH
EOF

echo ""
echo "==========================================================="
echo "🎉 DEPLOYMENT COMPLETE"
echo "==========================================================="
cat "$OUT_FILE"
echo "==========================================================="
echo "Recorded to $OUT_FILE — commit this file."
echo "Explorer: https://stellar.expert/explorer/$NETWORK/contract/$ANCHOR_ID"
echo "          https://stellar.expert/explorer/$NETWORK/contract/$VAULT_ID"

# ---------------------------------------------------------------------------
# Post-deployment verification (pubnet only)
# ---------------------------------------------------------------------------
if [ "$NETWORK" = "pubnet" ]; then
  echo ""
  echo "🔍 Verifying deployed contract metadata..."
  verify_ok=true

  echo -n "   ReceiptAnchor version: "
  anchor_meta_version=$(stellar contract invoke --id "$ANCHOR_ID" --network "$NETWORK" \
    --source "$IDENTITY" -- get_version 2>/dev/null || echo "UNKNOWN")
  echo "$anchor_meta_version"
  if [ "$anchor_meta_version" != "$ANCHOR_VERSION" ]; then
    echo "   ⚠️  Version mismatch! Expected $ANCHOR_VERSION, got $anchor_meta_version" >&2
    verify_ok=false
  fi

  echo -n "   RefundVault version:   "
  vault_meta_version=$(stellar contract invoke --id "$VAULT_ID" --network "$NETWORK" \
    --source "$IDENTITY" -- get_version 2>/dev/null || echo "UNKNOWN")
  echo "$vault_meta_version"
  if [ "$vault_meta_version" != "$VAULT_VERSION" ]; then
    echo "   ⚠️  Version mismatch! Expected $VAULT_VERSION, got $vault_meta_version" >&2
    verify_ok=false
  fi

  if [ "$verify_ok" = false ]; then
    echo "" >&2
    echo "❌ Post-deployment verification FAILED. Review the output above." >&2
    echo "   Deployment metadata has been written to $OUT_FILE." >&2
    exit 1
  fi

  echo "✅ Deployed contract versions match source"
  echo ""
  echo "==========================================================="
  echo "✅ PUBNET DEPLOYMENT VERIFIED"
  echo "==========================================================="
fi
