use std::process::Command;

fn main() {
    let git_dir = Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|dir| dir.trim().to_string());

    // Keep the embedded hash honest: re-run this script when the checked-out
    // commit, the branch ref it points at, or the index changes, so a cached
    // build can never report a stale commit or a clean tree that is actually
    // dirty.
    if let Some(git_dir) = &git_dir {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        if let Some(ref_path) = git_ref_path(git_dir) {
            println!("cargo:rerun-if-changed={ref_path}");
        }
        println!("cargo:rerun-if-changed={git_dir}/index");
    }
    println!("cargo:rerun-if-changed=src");

    match git_sha() {
        Some(sha) => {
            println!("cargo:rustc-env=GIT_SHA={sha}");
            println!("cargo:rustc-env=GIT_DIRTY={}", git_dirty());
        }
        None => {
            // No git access: a source tarball, a Docker layer without .git, or
            // git missing entirely. Keep tarball builds working, but make the
            // degraded provenance loud instead of silent.
            println!(
                "cargo:warning=git rev-parse HEAD failed — contractmeta commit will be 'unknown' (build provenance degraded)"
            );
            println!("cargo:rustc-env=GIT_SHA=unknown");
            println!("cargo:rustc-env=GIT_DIRTY=unknown");
        }
    }
}

fn git_ref_path(git_dir: &str) -> Option<String> {
    let head = std::fs::read_to_string(format!("{git_dir}/HEAD")).ok()?;
    head.strip_prefix("ref: ")
        .map(|reference| format!("{git_dir}/{}", reference.trim()))
}

fn git_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?;
    let sha = sha.trim();
    if sha.is_empty() {
        None
    } else {
        Some(sha.to_string())
    }
}

fn git_dirty() -> &'static str {
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map(|output| output.status.success() && !output.stdout.is_empty())
        .unwrap_or(false);
    if dirty {
        "1"
    } else {
        "0"
    }
}
