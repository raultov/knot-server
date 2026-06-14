use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let knot_version = read_knot_version().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=KNOT_VERSION={knot_version}");
}

/// Resolves the exact `knot` package version that this crate will link
/// against by reading `Cargo.lock`. We deliberately use the lock file (not
/// `Cargo.toml`) so the reported version always matches what cargo will
/// actually compile, including patch updates and yanked-version resolutions.
fn read_knot_version() -> Option<String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lock_path = manifest_dir.join("Cargo.lock");
    let manifest_path = manifest_dir.join("Cargo.toml");

    for path in [&lock_path, &manifest_path] {
        if let Some(version) = parse_version(&fs::read_to_string(path).ok()?) {
            return Some(version);
        }
    }

    None
}

fn parse_version(contents: &str) -> Option<String> {
    let mut in_knot_block = false;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with("[[package]]") {
            in_knot_block = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix("name = ") {
            let name = unquote(rest);
            in_knot_block = name == "knot";
            continue;
        }
        if in_knot_block && let Some(rest) = line.strip_prefix("version = ") {
            return Some(unquote(rest).to_string());
        }
    }
    None
}

fn unquote(raw: &str) -> &str {
    raw.trim().trim_matches('"')
}
