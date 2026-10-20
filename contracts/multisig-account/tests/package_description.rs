//! Package-description coverage for `contracts/multisig-account/Cargo.toml`
//! (issue #730).
//!
//! `cargo info`, the registry listing and any tooling that reads workspace
//! metadata surface a crate through the `description` key of its `[package]`
//! table. This test pins the one-line region the issue assigns: the
//! `[package]` table of `contracts/multisig-account/Cargo.toml` sets a
//! non-empty `description` and places it after `name`.

/// Reads this crate's manifest from disk.
fn manifest() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    std::fs::read_to_string(path).expect("Cargo.toml must be readable")
}

/// The trimmed, non-empty rows of the `[package]` table, in file order. A row
/// belongs to the table only until the next `[section]` header, so a
/// `description` set anywhere else in the file does not satisfy this test.
fn package_table(manifest: &str) -> Vec<&str> {
    let mut in_package = false;
    let mut rows: Vec<&str> = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package && !line.is_empty() {
            rows.push(line);
        }
    }
    rows
}

/// Index of the first row of `prefix` within the table, if present.
fn row_index(rows: &[&str], prefix: &str) -> Option<usize> {
    rows.iter().position(|row| row.starts_with(prefix))
}

/// The quoted value of `key = "…"` within the table, if present.
fn row_value<'a>(rows: &[&'a str], key: &str) -> Option<&'a str> {
    let prefix = format!("{key} = \"");
    let row = rows
        .iter()
        .copied()
        .find(|row| row.starts_with(prefix.as_str()))?;
    row.strip_prefix(prefix.as_str())?.strip_suffix('"')
}

/// The `[package]` table sets a non-empty `description`, so registry and
/// workspace metadata have something to say about what this crate is for.
#[test]
fn package_table_sets_non_empty_description() {
    let manifest = manifest();
    let rows = package_table(&manifest);
    assert!(!rows.is_empty(), "the [package] table must exist");

    let description = row_value(&rows, "description").expect("description key is missing");
    assert!(!description.is_empty(), "description must not be empty");
}

/// The `description` row sits inside `[package]`, after `name`, and appears
/// exactly once — the one-line region this issue owns, and nowhere else.
#[test]
fn description_follows_name_in_package_table() {
    let manifest = manifest();
    let rows = package_table(&manifest);

    let name = row_index(&rows, "name = \"").expect("name key is missing");
    let description = row_index(&rows, "description = \"").expect("description key is missing");

    assert!(
        description > name,
        "description must follow name in [package]"
    );
    let occurrences = manifest.matches("description =").count();
    assert_eq!(
        occurrences, 1,
        "exactly one description row in the manifest"
    );
}
