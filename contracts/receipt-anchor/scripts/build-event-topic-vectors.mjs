// Regenerate `tests/fixtures/event_topic_vectors.rs` from the canonical
// `event-topic-vectors.json`.
//
// `event-topic-vectors.json` is the single source of truth for the event
// topic shapes each contract version publishes (issue #942). This script
// turns it into the Rust fixture both contracts' `event_topic_versions`
// tests consume via `include!` — the refund-vault test reads the same file
// by relative path, so there is exactly one generated copy. Running it with
// `--check` verifies that the committed fixture is up to date; a drift fails
// the build, not a warning.
//
// Unlike the Merkle vectors there is no cross-repo consumer, so no `.sha256`
// sidecar is kept: `--check` against this fixture is the whole enforcement.
//
// Usage:
//   node build-event-topic-vectors.mjs            # (re)write the fixture
//   node build-event-topic-vectors.mjs --check    # verify the fixture, exit 1 on drift

import fs from "node:fs";
import path from "node:path";

const HERE = import.meta.dirname;
const ROOT = path.join(HERE, "..");
const JSON_PATH = path.join(ROOT, "event-topic-vectors.json");
const RS_PATH = path.join(ROOT, "tests", "fixtures", "event_topic_vectors.rs");

function constName(contract, event, arm) {
  const prefix = event.toUpperCase();
  void contract;
  return arm === "current" ? `${prefix}_CURRENT` : `${prefix}_LEGACY_0_1_0`;
}

function docComment(text, indent) {
  return text
    .split("\n")
    .map((line) => `${indent}/// ${line}`)
    .join("\n");
}

function renderArm(contract, event, arm, entry) {
  const name = constName(contract, event.event, arm);
  // Shared fixture: each consuming test uses a subset of the arms, so
  // unused arms must not trip dead_code in either crate.
  const allow = "#[allow(dead_code)]";
  if (entry === null || entry === undefined) {
    const reason =
      arm === "legacy_0_1_0"
        ? event.legacy_absent_reason || "no legacy arm recorded"
        : "no entry recorded";
    return `${docComment(`No ${arm} arm: ${reason}.`, "")}\n${allow}\npub const ${name}: Option<&[&str]> = None;\n`;
  }
  const topics = entry.topics.map((t) => JSON.stringify(t)).join(", ");
  const names = (entry.topic_names || []).join(", ");
  const provenance = entry.provenance || "(no provenance recorded)";
  return (
    `${docComment(`Topics (${names}). Provenance: ${provenance}`, "")}\n` +
    `${allow}\n` +
    (arm === "current"
      ? `pub const ${name}: &[&str] = &[${topics}];\n`
      : `pub const ${name}: Option<&[&str]> = Some(&[${topics}]);\n`)
  );
}

function render(doc) {
  const discrepancies = (doc.discrepancies || [])
    .map(
      (d) =>
        `//   - ${d.id} [${d.status}]\n` +
        `//     code: ${d.code}\n` +
        `//     docs: ${d.docs}\n` +
        `//     snapshots: ${d.snapshots}`
    )
    .join("\n");

  const header = `// GENERATED FILE — do not edit by hand.
//
// Emitted by contracts/receipt-anchor/scripts/build-event-topic-vectors.mjs
// from the canonical contracts/receipt-anchor/event-topic-vectors.json (issue
// #942). That JSON file is the single source of truth for which topic tuple
// each contract version publishes; both contracts' \`event_topic_versions\`
// tests consume this fixture (the refund-vault test includes it by relative
// path), so a shape change anywhere fails one of the suites.
//
// Tag semantics: a bare word is a Symbol of that name, "u64" is a u64 scalar
// position, "bytes32" is a BytesN<32> position.
//
// Recorded discrepancies (named, not settled — no side was edited):
${discrepancies}
//
// To regenerate locally:
//   node contracts/receipt-anchor/scripts/build-event-topic-vectors.mjs

`;

  const body = doc.events
    .map((e) => {
      const current = renderArm(e.contract, e, "current", e.current);
      const legacy = renderArm(e.contract, e, "legacy_0_1_0", e.legacy_0_1_0);
      return `// ${e.contract} :: ${e.event}\n${current}\n${legacy}`;
    })
    .join("\n");

  return header + body;
}

function main() {
  const doc = JSON.parse(fs.readFileSync(JSON_PATH, "utf8"));
  const rendered = render(doc);

  if (process.argv.includes("--check")) {
    const current = fs.existsSync(RS_PATH) ? fs.readFileSync(RS_PATH, "utf8") : "";
    if (current !== rendered) {
      console.error(
        "ERROR: tests/fixtures/event_topic_vectors.rs is out of sync with event-topic-vectors.json."
      );
      console.error("       Run: node scripts/build-event-topic-vectors.mjs");
      process.exit(1);
    }
    console.log("OK: tests/fixtures/event_topic_vectors.rs matches event-topic-vectors.json");
    console.log("event-topic-vector check passed");
    return;
  }

  fs.mkdirSync(path.dirname(RS_PATH), { recursive: true });
  fs.writeFileSync(RS_PATH, rendered);
  console.log(
    `Wrote ${doc.events.length} event shapes to ${path.relative(process.cwd(), RS_PATH)}`
  );
}

main();
