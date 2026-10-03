#!/usr/bin/env bash
set -euo pipefail

# Runs the wasm optimizer over the built contract artifacts, in place.
#
# This is the step `stellar contract build` (which deploy.sh invokes) runs
# before uploading: cargo's raw `--release` output is 20-30% larger than the
# blob that actually goes on chain. Without it, CI builds, measures and tests
# an artifact nobody deploys — and refuses to upload it to the test host,
# because a contract code ledger entry is capped at 131072 bytes while the raw
# refund_vault.wasm measured 138631.
#
# Sizes afterwards are in deployed-artifact bytes, which is what
# .wasm-budget.json is meant to bound.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

WASM_DIR="target/wasm32v1-none/release"

# Pinned Binaryen release. The checksums are for this tag's .tar.gz assets;
# bump BINARYEN_VERSION and both hashes together.
BINARYEN_VERSION="${BINARYEN_VERSION:-version_133}"
BINARYEN_SHA256_X86_64_LINUX="2dc9c7813f5375db93d96ead4b78222fcc3e2677bbb832297af4797782a37489"
BINARYEN_SHA256_ARM64_MACOS="ad66da82ac13f163e424b1643f16c6dfcccc98b5966296b43e52d3cab04f84a8"

# The wasm feature set soroban-env-host enables when instantiating a contract;
# the optimizer must not assume anything beyond it.
WASM_OPT_FLAGS=(
  -Oz
  --enable-sign-ext
  --enable-mutable-globals
  --enable-bulk-memory
  --enable-reference-types
)

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Resolve an existing wasm-opt, or fetch the pinned release into a local cache
# directory. Prints the path to the binary.
find_wasm_opt() {
  if command -v wasm-opt >/dev/null 2>&1; then
    command -v wasm-opt
    return
  fi

  local arch os asset expected url dir
  arch="$(uname -m)"
  os="$(uname -s)"
  case "$os/$arch" in
    Linux/x86_64)
      asset="binaryen-${BINARYEN_VERSION}-x86_64-linux.tar.gz"
      expected="$BINARYEN_SHA256_X86_64_LINUX" ;;
    Darwin/arm64)
      asset="binaryen-${BINARYEN_VERSION}-arm64-macos.tar.gz"
      expected="$BINARYEN_SHA256_ARM64_MACOS" ;;
    *)
      echo "Error: no pinned Binaryen asset for $os/$arch; install wasm-opt." >&2
      return 1 ;;
  esac

  dir="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/binaryen/${BINARYEN_VERSION}-${os}-${arch}"
  if [ ! -x "$dir/bin/wasm-opt" ]; then
    url="https://github.com/WebAssembly/binaryen/releases/download/${BINARYEN_VERSION}/${asset}"
    mkdir -p "$dir"
    curl --fail --silent --show-error --location --output "$dir/$asset" "$url"
    if [ "$(sha256_of "$dir/$asset")" != "$expected" ]; then
      echo "Error: checksum mismatch for $asset" >&2
      return 1
    fi
    # The archive unpacks to bin/, lib/, include/ under one top-level dir.
    tar -xzf "$dir/$asset" -C "$dir" --strip-components=1
  fi
  echo "$dir/bin/wasm-opt"
}

WASM_OPT="$(find_wasm_opt)"

if ! compgen -G "$WASM_DIR/*.wasm" >/dev/null; then
  echo "Error: no wasm artifacts in $WASM_DIR; build them first with" >&2
  echo "  cargo build --locked --workspace --exclude testutils --target wasm32v1-none --release" >&2
  exit 1
fi

echo "### WASM optimize ($("$WASM_OPT" --version), ${WASM_OPT_FLAGS[*]})"
echo ""
echo "| Contract | Raw (bytes) | Optimized (bytes) | Saved |"
echo "|---|---|---|---|"

for wasm in "$WASM_DIR"/*.wasm; do
  name="$(basename "$wasm")"
  before=$(stat -c%s "$wasm" 2>/dev/null || stat -f%z "$wasm")
  # Optimize to a temp path and move it over the original: wasm-opt exits
  # non-zero partway on an unparseable input, and a half-written contract
  # artifact would otherwise be size-checked or imported by a test.
  "$WASM_OPT" "${WASM_OPT_FLAGS[@]}" -o "$wasm.optimized" "$wasm"
  mv "$wasm.optimized" "$wasm"
  after=$(stat -c%s "$wasm" 2>/dev/null || stat -f%z "$wasm")
  printf "| \`%s\` | %s | %s | -%s%% |\n" "$name" "$before" "$after" \
    "$(( (before - after) * 100 / before ))"
done
