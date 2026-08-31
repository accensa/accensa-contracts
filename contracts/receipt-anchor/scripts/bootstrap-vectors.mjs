// Bootstrap the canonical `merkle-vectors.json` from the current `vectors.rs`
// plus the additional structural edge cases required by issue #53.
//
// This is a one-shot recovery/authoring tool. The committed source of truth is
// `merkle-vectors.json`; `build-vectors.mjs` regenerates `vectors.rs` from it.
// Run this again only when you need to (re)seed the JSON from hand-maintained
// Rust vectors — normally you edit `merkle-vectors.json` directly.
//
// Usage: node bootstrap-vectors.mjs

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

const HERE = import.meta.dirname;
const SRC = path.join(HERE, "..", "src", "vectors.rs");
const OUT = path.join(HERE, "..", "merkle-vectors.json");

// This is a one-shot authoring tool. If the canonical JSON already exists,
// refuse to clobber/duplicate it unless explicitly forced — normally you edit
// merkle-vectors.json directly and regenerate vectors.rs with build-vectors.mjs.
if (fs.existsSync(OUT) && !process.argv.includes("--force")) {
  console.error(`Refusing to overwrite existing ${OUT}.`);
  console.error("Edit merkle-vectors.json directly, then run build-vectors.mjs.");
  console.error("Pass --force to regenerate from vectors.rs (lost JSON edits).");
  process.exit(1);
}

// --- hashing primitives, mirroring receipt-shard verify_receipt -----------
function sha256(buf) {
  return crypto.createHash("sha256").update(buf).digest();
}
function hashPair(a, b) {
  const ab = Buffer.compare(a, b) <= 0 ? Buffer.concat([a, b]) : Buffer.concat([b, a]);
  return sha256(ab);
}
function fold(leaf, proof) {
  let c = Buffer.from(leaf);
  for (const s of proof) c = hashPair(c, s);
  return c;
}
function leaf(s) {
  return sha256(Buffer.from(s, "utf8"));
}

// --- tree builder for generating valid proofs/roots ------------------------
function buildRoot(leaves) {
  let level = leaves.map((x) => Buffer.from(x));
  while (level.length > 1) {
    if (level.length % 2 === 1) level.push(Buffer.from(level[level.length - 1]));
    const next = [];
    for (let i = 0; i < level.length; i += 2) next.push(hashPair(level[i], level[i + 1]));
    level = next;
  }
  return level[0];
}
function buildProof(leaves, idx) {
  let level = leaves.map((x) => Buffer.from(x));
  const proof = [];
  let i = idx;
  while (level.length > 1) {
    if (level.length % 2 === 1) level.push(Buffer.from(level[level.length - 1]));
    const sib = i % 2 === 0 ? level[i + 1] : level[i - 1];
    proof.push(Buffer.from(sib));
    const next = [];
    for (let j = 0; j < level.length; j += 2) next.push(hashPair(level[j], level[j + 1]));
    level = next;
    i = Math.floor(i / 2);
  }
  return proof;
}
function hex(b) {
  return "0x" + Buffer.from(b).toString("hex");
}

// --- lift the existing hand-written vectors.rs verbatim --------------------
function parseVectorsRs(text) {
  const vectors = [];
  const re = /(?<!struct\s)Vector\s*\{/g;
  let m;
  while ((m = re.exec(text)) !== null) {
    let i = m.index + m[0].length;
    let depth = 1;
    while (i < text.length && depth > 0) {
      if (text[i] === "{") depth++;
      else if (text[i] === "}") depth--;
      i++;
    }
    const block = text.slice(m.index, i);
    const name = block.match(/name:\s*"([^"]+)"/)[1];
    const leaf = parseBytes32(block.match(/leaf:\s*(\[[^\]]*\])/)[1]);
    const proof = parseProofBlock(block);
    const root = parseBytes32(block.match(/root:\s*(\[[^\]]*\])/)[1]);
    const expected = /expected:\s*true/.test(block);
    vectors.push({ name, leaf: hex(leaf), proof: proof.map(hex), root: hex(root), expected });
  }
  return vectors;
}
// Bracket-balanced extraction of the `proof: &[ ... ]` array, robust to nested
// brackets (each element is itself a 32-byte array literal).
function parseProofBlock(block) {
  const pm = block.match(/proof:\s*&?\[/);
  if (!pm) return [];
  const start = pm.index + pm[0].length - 1; // index of the opening '['
  let depth = 0;
  let i = start;
  for (; i < block.length; i++) {
    if (block[i] === "[") depth++;
    else if (block[i] === "]") {
      depth--;
      if (depth === 0) break;
    }
  }
  const inner = block.slice(start + 1, i);
  return parseProof("[" + inner + "]");
}
function parseBytes32(s) {
  const nums = s.match(/0x[0-9a-fA-F]{2}/g).map((h) => parseInt(h, 16));
  return Buffer.from(nums);
}
function parseProof(s) {
  const out = [];
  const re = /\[0x[0-9a-fA-F]{2}(?:,\s*0x[0-9a-fA-F]{2})*\]/g;
  let mm;
  while ((mm = re.exec(s)) !== null) {
    out.push(Buffer.from(mm[0].match(/0x[0-9a-fA-F]{2}/g).map((h) => parseInt(h, 16))));
  }
  return out;
}

// --- new structural edge cases (issue #53) ---------------------------------
function generatedVectors() {
  const out = [];

  // Odd leaf count (5) requiring promotion of a promoted node — leaf 0.
  {
    const leaves = [leaf("accensa-edge-5-0"), leaf("accensa-edge-5-1"), leaf("accensa-edge-5-2"), leaf("accensa-edge-5-3"), leaf("accensa-edge-5-4")];
    const idx = 0;
    const proof = buildProof(leaves, idx);
    const root = buildRoot(leaves);
    out.push({ name: "five-leaf batch — leaf 0 (odd count, promotion)", leaf: hex(leaves[idx]), proof: proof.map(hex), root: hex(root), expected: true });
  }

  // Odd leaf count (5) — last leaf exercises the promoted tail path.
  {
    const leaves = [leaf("accensa-edge-5-0"), leaf("accensa-edge-5-1"), leaf("accensa-edge-5-2"), leaf("accensa-edge-5-3"), leaf("accensa-edge-5-4")];
    const idx = 4;
    const proof = buildProof(leaves, idx);
    const root = buildRoot(leaves);
    out.push({ name: "five-leaf batch — leaf 4 (odd count, promoted tail)", leaf: hex(leaves[idx]), proof: proof.map(hex), root: hex(root), expected: true });
  }

  // Duplicate leaves — two identical leaf values must not corrupt the build.
  {
    const D = leaf("accensa-dup-D");
    const E = leaf("accensa-dup-E");
    const F = leaf("accensa-dup-F");
    const leaves = [D, D, E, F];
    const idx = 0;
    const proof = buildProof(leaves, idx);
    const root = buildRoot(leaves);
    out.push({ name: "duplicate-leaf batch — membership (two identical leaves)", leaf: hex(leaves[idx]), proof: proof.map(hex), root: hex(root), expected: true });
  }

  // Sorted-pair tie — both siblings hash identically, so the fold must handle
  // a == b (lexicographic tie) without positional flags.
  {
    const X = leaf("accensa-tie-X");
    const leaves = [X, X];
    const idx = 0;
    const proof = buildProof(leaves, idx);
    const root = buildRoot(leaves);
    out.push({ name: "sorted-pair tie — both siblings identical (2-leaf [X, X])", leaf: hex(leaves[idx]), proof: proof.map(hex), root: hex(root), expected: true });
  }

  // Over-long proof (wrong length) — a valid leaf/root plus one extra sibling
  // must be rejected, not silently accepted.
  {
    const A = leaf("accensa-long-A");
    const B = leaf("accensa-long-B");
    const G = leaf("accensa-long-garbage");
    const leaves = [A, B];
    const idx = 0;
    const good = buildProof(leaves, idx); // [B]
    const root = buildRoot(leaves);
    const badProof = good.concat([G]);
    // sanity: the over-long proof must NOT verify
    if (Buffer.compare(fold(A, badProof), root) === 0) {
      throw new Error("over-long proof unexpectedly verified; test data is wrong");
    }
    out.push({ name: "over-long proof (wrong length) is rejected", leaf: hex(A), proof: badProof.map(hex), root: hex(root), expected: false });
  }

  return out;
}

// --- assemble & validate ---------------------------------------------------
const existing = parseVectorsRs(fs.readFileSync(SRC, "utf8"));
const generated = generatedVectors();

// Validate every vector against the contract's exact fold so the Rust and
// TypeScript suites cannot disagree on these by construction.
for (const v of [...existing, ...generated]) {
  const got = fold(Buffer.from(v.leaf.slice(2), "hex"), v.proof.map((p) => Buffer.from(p.slice(2), "hex")));
  const root = Buffer.from(v.root.slice(2), "hex");
  const matches = Buffer.compare(got, root) === 0;
  if (matches !== v.expected) {
    throw new Error(`vector "${v.name}" is internally inconsistent with the fold (expected ${v.expected}, fold=${matches})`);
  }
}

const doc = {
  schemaVersion: "1",
  convention:
    "Sorted-pair SHA-256 Merkle inclusion proofs. A proof is a position-flag-free array of sibling hashes; at each level the running hash and the sibling are sorted lexicographically then concatenated and SHA-256 hashed. Odd levels promote the final node by duplication. verify = (fold(leaf, proof) == root).",
  owner: "accensa-contracts (canonical). accensa-app vendors a byte-identical copy.",
  vectors: [...existing, ...generated],
};

fs.writeFileSync(OUT, JSON.stringify(doc, null, 2) + "\n");
console.log(`Wrote ${doc.vectors.length} vectors to ${path.relative(process.cwd(), OUT)}`);
console.log(`  existing: ${existing.length}, generated edge cases: ${generated.length}`);
