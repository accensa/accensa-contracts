# Conformance: cross-implementation parity for `verify_receipt`

This document explains how `accensa-contracts` and `accensa-app` prove that
their two independent implementations of Merkle receipt verification agree, and
— just as importantly — what that proof does and does not guarantee. It is the
machine-enforced answer to the failure mode called out in issue #53: **drift,
not inability**.

The shared artifact is a fixed set of inclusion-proof vectors. Both the Rust
contract (`ReceiptAnchor` → `ReceiptShard::verify_receipt`) and the TypeScript
SDK run `verify_receipt(leaf, proof, root)` against the same vectors and must
return the same `expected` boolean for every one. Cross-implementation
conformance is *demonstrated*, not asserted.

## Ownership decision (single source of truth)

**`accensa-contracts` owns the vectors.** The canonical file is
`contracts/receipt-anchor/merkle-vectors.json`.

`accensa-app` does **not** own a copy. It vendors a byte-identical copy at
`packages/sdk/merkle-vectors.json` and consumes it directly. The two copies are
kept in lock-step by the cross-repo CI described below; neither repo is free to
drift because the build fails the moment they do.

Rationale for this repo owning the source:

- The Rust contract is the on-chain authority; its `verify_receipt` is what
  actually gates refunds, so the canonical test data should live next to it.
- A JSON data file is the lowest-friction common format for two languages (Rust
  parses it through a generator; TypeScript imports it natively). No code has to
  be translated by hand.
- A committed **content hash** (`merkle-vectors.json.sha256`) gives both repos a
  single constant to compare, independent of formatting or transport.

The previous arrangement (accensa-app generated `vectors.rs` via
`generate-vectors.mjs` and this repo imported it) had no automation enforcing
that the two sides ever agreed. That gap is what this document and the
surrounding CI close.

## The three layers of enforcement

1. **JSON → Rust, in-repo.** `src/vectors.rs` is *generated*, not edited by
   hand. `scripts/build-vectors.mjs` reads `merkle-vectors.json` and emits it.
   CI runs `node scripts/build-vectors.mjs --check`, which fails if
   `vectors.rs` is out of sync with the JSON. (Re-run the script locally to
   regenerate.)

2. **Hash freshness, in-repo.** The same `--check` step fails if
   `merkle-vectors.json.sha256` does not match the current
   `merkle-vectors.json`. Changing the vectors without bumping the hash is
   therefore impossible to merge.

3. **Cross-repo hash equality.** A `vector-parity` job in
   `.github/workflows/ci.yml` fetches accensa-app's vendored
   `packages/sdk/merkle-vectors.json` (at a pinned ref) and fails the build when
   its hash differs from our committed `merkle-vectors.json.sha256`. accensa-app
   runs the mirror image (`cross-repo/accensa-app/.github/workflows/vector-parity.yml`),
   comparing its vendored copy's hash against the canonical file fetched from
   this repo.

The cross-repo jobs run on push to `main`, on pull requests, on a daily
`schedule`, and on `repository_dispatch` (a push here pings accensa-app to
re-check immediately). Silence is not an option: a stale copy is caught within a
day at most, and usually within minutes of the change that caused it.

When you update the vectors you must update **both** repos together: edit the
JSON here, regenerate `vectors.rs` and the `.sha256`, land that in
`accensa-contracts`, then copy the new JSON + hash into accensa-app and land it
there. Until both sides carry the same hash, the parity job on the lagging side
will (correctly) be red.

> Implementation note: until accensa-app has vendored the file for the first
> time, the fetch step in this repo's job emits a **warning** rather than a hard
> failure, so the rest of CI is not blocked during the initial sync. Once the
> file exists, any hash mismatch is a hard failure. The accensa-app job is strict
> in both directions (missing file = failure) because its copy is expected to
> exist by the time that workflow is installed — see
> `cross-repo/accensa-app/README.md` for the install steps.

## The vector set

Each vector is `{ name, leaf, proof[], root, expected }`. `proof` is a
position-flag-free array of sibling hashes (sorted-pair SHA-256, see
`docs/ADR-001-merkle-structure.md`). The set intentionally covers the edge cases
where two independent implementations are most likely to disagree:

| Case | Why it matters |
| --- | --- |
| single-leaf (empty proof) | Degenerate tree; the fold must short-circuit to `leaf == root`. |
| two-leaf | Smallest real tree; exercises the only non-trivial fold once. |
| odd counts (three-, five-leaf, promoted tail) | Off-chain builders differ on how to pad the last level; promotion must match or proofs break. |
| duplicate leaves | Two identical leaf values must not corrupt tree building or folding. |
| sorted-pair tie (2-leaf `[X, X]`) | Both siblings hash identically, so the `a <= b` sort branch hits a lexical tie with no positional flag to fall back on. |
| wrong root / forged leaf / reordered proof / truncated proof | Negative cases proving a correct `false`. |
| over-long proof (wrong length) | An extra sibling must be rejected, not silently consumed. |

A test guard (`test_shared_vectors_cover_required_edge_cases` in
`contracts/receipt-anchor/src/test.rs`) asserts these categories are present by
name, so the suite cannot silently shed the very cases that make the proof
meaningful.

## What parity guarantees

- The Rust contract and the TypeScript SDK produce the **same `expected` result**
  for every vector in the shared set, under the same sorting and hashing rules.
- The committed hash means the two repos are running against **byte-identical**
  vectors, not merely "similar" ones.

## What parity does NOT guarantee

- **It is not a correctness proof of either implementation.** If both the Rust
  and TypeScript code share the same bug (e.g. both truncate proofs the same
  wrong way), they still "agree" and the parity job stays green. The vectors
  raise the bar for *independent* mistakes; they do not rule out *correlated*
  ones. Correctness still rests on `docs/SECURITY_MODEL.md`, the fuzz suite, and
  review.
- **It does not cover the off-chain builder.** Verification (`verify_receipt`)
  and tree construction live in different code. Parity proves the two verifiers
  agree; it does not prove the anchors were built correctly upstream.
- **It is only as current as the vector set.** New attack surface (new proof
  shapes, new leaf encodings) is only protected once a vector exercises it. The
  category guard helps, but it is a floor, not a ceiling.
- **A green parity job during the initial cross-repo sync window says nothing**;
  it only becomes meaningful once both repos are vendoring the same file, as
  noted above.

## Regenerating

```sh
# After editing merkle-vectors.json (or to recover vectors.rs):
node contracts/receipt-anchor/scripts/build-vectors.mjs
# Verify nothing drifted (used by CI):
node contracts/receipt-anchor/scripts/build-vectors.mjs --check
```

To add a new structural case, edit `merkle-vectors.json` directly (or use
`scripts/bootstrap-vectors.mjs` to re-seed it from `vectors.rs` plus generated
edge cases), then regenerate and re-bump the hash.
