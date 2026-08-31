# accensa-app side of issue #53 (cross-repo vector parity)

This directory contains the artifacts that must be added to
[`accensa/accensa-app`](https://github.com/accensa/accensa-app) so the
`verify_receipt` Merkle vectors stay in sync with `accensa-contracts`. It is
the mirror of the `vector-parity` job that lives in
`accensa-contracts/.github/workflows/ci.yml`.

## What to do (open a PR in accensa-app)

1. **Vendor the canonical vectors.** Copy
   `contracts/receipt-anchor/merkle-vectors.json` from `accensa-contracts` into
   `packages/sdk/merkle-vectors.json` **byte-for-byte** (it is the single source
   of truth owned by `accensa-contracts`). Commit the file's content hash:

   ```sh
   sha256sum packages/sdk/merkle-vectors.json > packages/sdk/merkle-vectors.json.sha256
   ```

   The SDK should import this JSON directly instead of maintaining its own copy
   or generating `vectors.ts` from a divergent source.

2. **Add the CI job.** Drop
   `.github/workflows/vector-parity.yml` (this folder) into
   `accensa-app/.github/workflows/vector-parity.yml`.

3. **Pin the source ref.** In the accensa-app repo settings, add a repository
   variable `ACCELSA_CONTRACTS_REF` set to the `accensa-contracts` commit SHA or
   tag you want to track (do not leave it tracking `main` long-term).

4. **Open the PR** and link it back to
   [accensa-contracts issue #53](https://github.com/accensa/accensa-contracts/issues/53).

## How the two halves fit together

| Repo            | Owns                                  | Checks against                          |
| --------------- | ------------------------------------- | --------------------------------------- |
| accensa-contracts | `merkle-vectors.json` (canonical)  | accensa-app's `packages/sdk/merkle-vectors.json` |
| accensa-app     | vendored `packages/sdk/merkle-vectors.json` | accensa-contracts' canonical `merkle-vectors.json` |

Each repo fails its build when its copy's hash differs from the other's. A push
to `accensa-contracts/main` also fires a `repository_dispatch` that re-runs the
accensa-app job, and a daily cron catches silent staleness on either side. See
`docs/CONFORMANCE.md` in `accensa-contracts` for the full mechanism and its
limits.
