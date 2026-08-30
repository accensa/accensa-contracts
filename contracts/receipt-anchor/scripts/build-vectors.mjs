// Regenerate `src/vectors.rs` from the canonical `merkle-vectors.json`.
//
// `merkle-vectors.json` is the single source of truth shared (via a vendored
// copy) with the accensa-app TypeScript SDK. This script turns it into the Rust
// fixture the contract test suite consumes. Running it with `--check` verifies
// that the committed `vectors.rs` is up to date AND that the committed
// `merkle-vectors.json.sha256` matches the current file — both guards must pass
// in CI so a diverged or stale fixture fails the build, not a warning.
//
// Usage:
//   node build-vectors.mjs            # (re)write src/vectors.rs
//   node build-vectors.mjs --check    # verify vectors.rs + hash, exit 1 on drift

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

const HERE = import.meta.dirname;
const ROOT = path.join(HERE, "..");
const JSON_PATH = path.join(ROOT, "merkle-vectors.json");
const RS_PATH = path.join(ROOT, "src", "vectors.rs");
const HASH_PATH = path.join(ROOT, "merkle-vectors.json.sha256");

function sha256File(p) {
  return crypto.createHash("sha256").update(fs.readFileSync(p)).digest("hex");
}

function bytesLiteral(hexStr) {
  // hexStr is "0x...." (64 hex chars)
  const h = hexStr.slice(2);
  const parts = [];
  for (let i = 0; i < h.length; i += 2) parts.push("0x" + h.slice(i, i + 2));
  return "[" + parts.join(", ") + "]";
}

function render(doc) {
  const header = `// GENERATED FILE — do not edit by hand.
//
// Emitted by contracts/receipt-anchor/scripts/build-vectors.mjs from the
// canonical contracts/receipt-anchor/merkle-vectors.json. That JSON file is the
// single source of truth shared with the accensa-app TypeScript SDK; both
// implementations are tested against byte-identical vectors, so any divergence
// between the two fails one of the suites.
//
// Parity is enforced three ways (see docs/CONFORMANCE.md):
//   1. This file is regenerated from merkle-vectors.json in CI; a drift fails.
//   2. merkle-vectors.json.sha256 is the committed hash of the JSON; a stale
//      hash fails.
//   3. A cross-repo CI job fails when this file's source hash differs from the
//      SDK's vendored copy.
//
// To regenerate locally:
//   node contracts/receipt-anchor/scripts/build-vectors.mjs

pub struct Vector {
    pub name: &'static str,
    pub leaf: [u8; 32],
    pub proof: &'static [[u8; 32]],
    pub root: [u8; 32],
    pub expected: bool,
}

// Generated layout is intentionally dense; rustfmt would reflow every hash
// literal and make regeneration produce spurious diffs.
#[rustfmt::skip]
pub const VECTORS: &[Vector] = &[
`;

  const body = doc.vectors
    .map((v) => {
      const proofInner =
        v.proof.length === 0
          ? ""
          : v.proof.map((p) => "            " + bytesLiteral(p) + ",").join("\n");
      const proofStr = v.proof.length === 0 ? "&[]" : `&[\n${proofInner}\n        ]`;
      return `    Vector {
        name: ${JSON.stringify(v.name)},
        leaf: ${bytesLiteral(v.leaf)},
        proof: ${proofStr},
        root: ${bytesLiteral(v.root)},
        expected: ${v.expected},
    },`;
    })
    .join("\n");

  return header + body + "\n];\n";
}

function main() {
  const doc = JSON.parse(fs.readFileSync(JSON_PATH, "utf8"));
  const rendered = render(doc);

  if (process.argv.includes("--check")) {
    let failed = false;

    const current = fs.readFileSync(RS_PATH, "utf8");
    if (current !== rendered) {
      console.error("ERROR: src/vectors.rs is out of sync with merkle-vectors.json.");
      console.error("       Run: node scripts/build-vectors.mjs");
      failed = true;
    } else {
      console.log("OK: src/vectors.rs matches merkle-vectors.json");
    }

    const computed = sha256File(JSON_PATH);
    const committed = fs.existsSync(HASH_PATH) ? fs.readFileSync(HASH_PATH, "utf8").trim() : "";
    if (committed !== computed) {
      console.error("ERROR: merkle-vectors.json.sha256 is stale.");
      console.error(`       expected: ${computed}`);
      console.error(`       committed: ${committed || "(missing)"}`);
      console.error("       Update the hash when you change the vectors.");
      failed = true;
    } else {
      console.log("OK: merkle-vectors.json.sha256 matches");
    }

    if (failed) process.exit(1);
    console.log("vector parity check passed");
    return;
  }

  fs.writeFileSync(RS_PATH, rendered);
  // Keep the committed hash authoritative alongside the JSON.
  fs.writeFileSync(HASH_PATH, sha256File(JSON_PATH) + "\n");
  console.log(`Wrote ${doc.vectors.length} vectors to ${path.relative(process.cwd(), RS_PATH)}`);
  console.log(`Wrote hash to ${path.relative(process.cwd(), HASH_PATH)}`);
}

main();
