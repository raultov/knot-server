use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Names of directories that are never copied from `src` to `dst` during a
/// local sync. These are universally generated build/IDE/dependency outputs
/// that carry no source code and can be enormous (e.g. Rust `target/` is
/// routinely 10s of GB; `node_modules/` routinely 1+ GB). The indexer never
/// needs them, and mirroring them would make every sync slow and balloon
/// the workspace.
///
/// Matched on the directory's **base name**, not its full path, so a `target/`
/// anywhere in the tree is skipped.
const SKIP_DIRS: &[&str] = &[
    "target",        // Rust, Scala
    "node_modules",  // JS / TS
    ".gradle",       // Gradle
    "build",         // Maven, Gradle, Ant
    "dist",          // JS bundles, SBT
    "out",           // IntelliJ, various
    ".next",         // Next.js
    ".nuxt",         // Nuxt
    ".svelte-kit",   // SvelteKit
    ".cache",        // misc
    "__pycache__",   // Python
    ".pytest_cache", // pytest
    ".mypy_cache",   // mypy
    ".ruff_cache",   // ruff
    ".tox",          // tox
    ".idea",         // JetBrains IDE
    ".vscode",       // VS Code workspace settings
];

fn should_skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

pub fn is_local_path(url: &str) -> bool {
    if url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("git@")
        || url.starts_with("ssh://")
        || url.starts_with("git://")
    {
        return false;
    }
    let p = Path::new(url);
    if !p.is_dir() {
        return false;
    }
    // A bare git repository is not a working tree — it has no source files
    // to index (only the git object/refs database). Treating it as a local
    // working tree would mirror the git metadata into the destination and
    // leave the indexer with 0 source files. Bare repos must be cloned
    // like any other remote.
    if is_bare_git_repo(p) {
        return false;
    }
    true
}

fn is_bare_git_repo(p: &Path) -> bool {
    p.join("HEAD").is_file()
        && p.join("config").is_file()
        && p.join("objects").is_dir()
        && p.join("refs").is_dir()
}

pub fn sync_local_working_tree(src: &str, dst: &str) -> anyhow::Result<()> {
    let src_path = PathBuf::from(src);
    let dst_path = PathBuf::from(dst);

    if !src_path.is_dir() {
        anyhow::bail!("Source path is not a directory: {}", src);
    }

    // Refuse to copy a directory onto itself. `fs::copy(file, file)` opens
    // the destination with O_TRUNC and then reads from the same file
    // descriptor, leaving every file empty. This happens whenever the
    // caller puts the source repo inside the knot-server workspace and
    // the registry derives `local_path = workspace/<id>` from the same
    // basename. Bail with a clear error instead of corrupting the
    // user's source tree.
    let src_canon = fs::canonicalize(&src_path).unwrap_or_else(|_| src_path.clone());
    let dst_canon = fs::canonicalize(&dst_path).unwrap_or_else(|_| dst_path.clone());
    if src_canon == dst_canon {
        anyhow::bail!(
            "local sync source and destination resolve to the same path ({}); \
             place the source repo outside the knot-server workspace, or \
             register a URL whose basename does not collide with the \
             workspace dir layout",
            src_canon.display()
        );
    }

    // Fail fast on a truly unreadable repo root. Subtrees that become
    // unreadable during recursion are handled inside `copy_tree` (best-
    // effort, with a warning) so a single locked subdirectory does not
    // abort the whole sync.
    fs::read_dir(&src_path).map_err(|e| {
        anyhow::Error::new(e).context(format!("Cannot read source directory: {}", src))
    })?;

    fs::create_dir_all(&dst_path)?;
    copy_tree(&src_path, &dst_path)?;
    prune_tree(&src_path, &dst_path)?;

    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    // Reading a subdirectory can fail with EACCES even when the parent
    // directory is fully accessible — e.g. an unrelated user's data/
    // nested inside the repo, or a `chmod 0` artifact. The whole sync
    // must not abort in that case; log and skip the subtree.
    let entries = match fs::read_dir(src) {
        Ok(it) => it,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            tracing::warn!(
                "skipping unreadable directory during local sync: {} ({e})",
                src.display()
            );
            return Ok(());
        }
        Err(e) => return Err(anyhow::Error::new(e).context(format!("read_dir {}", src.display()))),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("skipping unreadable entry under {}: {e}", src.display());
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "skipping entry with unknown file type under {}: {e}",
                    src.display()
                );
                continue;
            }
        };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str == ".git" {
            continue;
        }
        if file_type.is_dir() && should_skip_dir(&name_str) {
            continue;
        }

        let src_child = entry.path();
        let dst_child = dst.join(&name);

        // Symlinks are not followed. The target might be outside the
        // source tree (e.g. /etc/passwd), might be a broken link, or
        // might be on a different filesystem. Copying the link as-is
        // would either silently pull unrelated content into the mirror
        // or fail with a confusing error.
        if file_type.is_symlink() {
            tracing::debug!(
                "skipping symlink during local sync: {}",
                src_child.display()
            );
            continue;
        }

        if file_type.is_dir() {
            // Probe the source subdir before creating the mirror, so we
            // do not leave an empty `dst_child/` behind when the source
            // is unreadable. EACCES here is the production failure
            // (e.g. a subdir owned by another user); other errors are
            // surfaced as warnings and the subtree is skipped.
            if let Err(e) = fs::read_dir(&src_child) {
                if e.kind() == io::ErrorKind::PermissionDenied {
                    tracing::warn!(
                        "skipping unreadable directory during local sync: {} ({e})",
                        src_child.display()
                    );
                } else {
                    tracing::warn!(
                        "skipping directory: cannot read {}: {e}",
                        src_child.display()
                    );
                }
                continue;
            }
            if let Err(e) = fs::create_dir_all(&dst_child) {
                tracing::warn!(
                    "skipping directory: cannot create mirror {}: {e}",
                    dst_child.display()
                );
                continue;
            }
            if let Err(e) = make_writable(&dst_child) {
                tracing::warn!(
                    "skipping subtree: cannot make mirror writable {}: {e}",
                    dst_child.display()
                );
                continue;
            }
            if let Err(e) = copy_tree(&src_child, &dst_child) {
                tracing::warn!("error syncing subtree {}: {e:#}", src_child.display());
            }
        } else if file_type.is_file() {
            if let Some(parent) = dst_child.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = make_writable(&dst_child) {
                tracing::warn!("cannot make mirror writable {}: {e}", dst_child.display());
            }
            if let Err(e) = fs::copy(&src_child, &dst_child) {
                // EACCES on the source file is the common case (unreadable
                // by the user running the server) — same fix: skip and
                // continue so the rest of the repo still indexes.
                tracing::warn!(
                    "skipping file: cannot copy {} -> {}: {e}",
                    src_child.display(),
                    dst_child.display()
                );
            }
        } else {
            // FIFOs, sockets, device nodes, etc. — not meaningful to copy.
            tracing::debug!(
                "skipping non-regular file during local sync: {}",
                src_child.display()
            );
        }
    }
    Ok(())
}

/// Strip the read-only bit from an existing path so a subsequent
/// `fs::copy` or recursive write can succeed.
///
/// `std::fs::copy` preserves the source file's permission bits when
/// creating the destination, so a source file with mode `0o444` produces
/// a mirror with mode `0o444`. On the next sync, opening that mirror
/// for writing (`O_WRONLY | O_TRUNC`) returns `EACCES (os error 13)`.
/// The same trap applies to directories with mode `0o555`: a subsequent
/// `fs::copy` of any child into the mirror fails because the parent
/// directory denies writes.
///
/// A no-op if the path does not exist, has no read-only bit to clear,
/// or cannot be stat'd. Errors from `set_permissions` propagate so the
/// caller learns about truly unrecoverable permission problems.
fn make_writable(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = fs::metadata(path)?;
    let mut perms = metadata.permissions();
    if perms.readonly() {
        ensure_writable(&mut perms);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Restore the owner-write bit so a read-only path can be written to.
///
/// On Unix, `set_readonly(false)` is implemented as "clear the read-only
/// bit in the mode", which can leave the file world-writable depending on
/// the umask and prior state. Setting the owner-write bit explicitly
/// matches what `fs::create_dir_all` and `fs::copy` produce for freshly
/// created paths, so we are not granting any privilege the caller did
/// not already have.
#[cfg(unix)]
fn ensure_writable(perms: &mut fs::Permissions) {
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(perms.mode() | 0o200);
}

#[cfg(not(unix))]
fn ensure_writable(perms: &mut fs::Permissions) {
    perms.set_readonly(false);
}

fn prune_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if !dst.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(dst)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with(".knot") {
            // Indexer state — must survive pruning.
            continue;
        }
        if file_type.is_dir() && should_skip_dir(&name_str) {
            // Skipped dirs must never accumulate in the mirror, even if
            // they still exist in the source (e.g. `target/` regenerated
            // by `cargo build` between syncs).
            fs::remove_dir_all(entry.path())?;
            continue;
        }

        let dst_child = entry.path();
        let src_child = src.join(&name);

        if !src_child.exists() {
            if file_type.is_dir() {
                fs::remove_dir_all(&dst_child)?;
            } else {
                fs::remove_file(&dst_child)?;
            }
            continue;
        }

        if file_type.is_dir() && src_child.is_dir() {
            prune_tree(&src_child, &dst_child)?;
        }
    }
    Ok(())
}

/// Inspects `.knot/index_state.json` in `repo_path` and removes it if it was
/// written by an older `knot` version (no top-level `version` field).
///
/// Returns `true` if a stale state file was found and deleted, `false` otherwise.
///
/// This guards the local-path sync against a one-time transition when the
/// `knot` library bumps its on-disk state version. Without this, the first
/// sync after the transition would fail at `IndexState::load` with
/// "Detected index_state v0; current version is v3", because local_sync
/// preserves `.knot/` (it is the indexer's incremental state, not part of
/// the source tree). The function is a no-op when:
///   - the file does not exist (fresh mirror, no migration needed)
///   - the file has a `version` field (current format, loadable)
///   - the file is corrupt or unreadable (we do not destroy unknown content)
pub fn clear_stale_index_state(repo_path: &str) -> bool {
    let state_path = Path::new(repo_path).join(".knot").join("index_state.json");
    if !state_path.exists() {
        return false;
    }
    let Ok(content) = fs::read_to_string(&state_path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    let Some(obj) = value.as_object() else {
        return false;
    };
    let is_stale = !obj.contains_key("version");
    if is_stale {
        let _ = fs::remove_file(&state_path);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_is_local_path_absolute() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        assert!(is_local_path(&path));
    }

    #[test]
    fn test_is_local_path_bare_git_repo() {
        // A bare repo at dir/HEAD + dir/config + dir/objects + dir/refs
        // must NOT be treated as a local working tree, because it has no
        // source files to index — only the git object database.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(dir.path().join("config"), "[core]\n\tbare = true\n").unwrap();
        fs::create_dir(dir.path().join("objects")).unwrap();
        fs::create_dir(dir.path().join("refs")).unwrap();

        let path = dir.path().to_string_lossy().to_string();
        assert!(
            !is_local_path(&path),
            "bare git repo must be cloned, not synced as a local working tree"
        );
    }

    #[test]
    fn test_is_local_path_working_tree_with_dot_git() {
        // A regular working tree has a .git/ directory containing the git
        // metadata, not HEAD/config/objects/refs at its root. This must
        // still be detected as a local working tree.
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();

        let path = dir.path().to_string_lossy().to_string();
        assert!(is_local_path(&path));
    }

    #[test]
    fn test_is_local_path_partial_bare_structure() {
        // A directory that has only some of the bare-repo markers (e.g.
        // a project named "config" with an "objects" subdir) must still
        // be treated as a regular local working tree, because the test
        // for bare-ness is the conjunction of all four markers.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("HEAD"), "x").unwrap();
        fs::create_dir(dir.path().join("objects")).unwrap();

        let path = dir.path().to_string_lossy().to_string();
        assert!(is_local_path(&path));
    }

    #[test]
    fn test_is_local_path_remote_ssh() {
        assert!(!is_local_path("git@github.com:org/repo.git"));
    }

    #[test]
    fn test_is_local_path_remote_https() {
        assert!(!is_local_path("https://github.com/org/repo.git"));
    }

    #[test]
    fn test_is_local_path_nonexistent() {
        assert!(!is_local_path("/nonexistent/path/xyz"));
    }

    #[test]
    fn test_sync_copies_new_file() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_path = dst.path().to_string_lossy().to_string();

        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        fs::write(src.path().join("new.java"), "class New {}").unwrap();
        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        assert!(dst.path().join("new.java").exists());
    }

    #[test]
    fn test_sync_same_src_and_dst_fails_loudly() {
        // When src and dst resolve to the same canonical path, an
        // unguarded `fs::copy` would truncate every file to zero bytes.
        // The sync must detect this and bail out before doing damage.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("important.java"), "class Important {}").unwrap();

        let path = dir.path().to_string_lossy().to_string();
        let result = sync_local_working_tree(&path, &path);
        assert!(result.is_err(), "expected same-path sync to fail loudly");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("same path") || msg.contains("outside"),
            "error message should explain the cause, got: {msg}"
        );

        // The original file must still have its full content.
        assert_eq!(
            fs::read_to_string(dir.path().join("important.java")).unwrap(),
            "class Important {}"
        );
    }

    #[test]
    fn test_sync_overwrites_modified_file() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_path = dst.path().to_string_lossy().to_string();

        fs::write(src.path().join("a.java"), "class A {}").unwrap();
        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        fs::write(src.path().join("a.java"), "class A { void newMethod() {} }").unwrap();
        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        let content = fs::read_to_string(dst.path().join("a.java")).unwrap();
        assert!(content.contains("newMethod"));
    }

    #[test]
    fn test_sync_deletes_removed_file() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_path = dst.path().to_string_lossy().to_string();

        fs::write(src.path().join("keep.java"), "class Keep {}").unwrap();
        fs::write(src.path().join("remove.java"), "class Remove {}").unwrap();
        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        assert!(dst.path().join("remove.java").exists());

        fs::remove_file(src.path().join("remove.java")).unwrap();
        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        assert!(dst.path().join("keep.java").exists());
        assert!(!dst.path().join("remove.java").exists());
    }

    #[test]
    fn test_sync_skips_git_dir() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_path = dst.path().to_string_lossy().to_string();

        fs::create_dir_all(src.path().join(".git")).unwrap();
        fs::write(src.path().join(".git").join("config"), "[core]").unwrap();
        fs::write(src.path().join("file.java"), "class F {}").unwrap();

        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        assert!(dst.path().join("file.java").exists());
        assert!(!dst.path().join(".git").exists());
    }

    #[test]
    fn test_sync_preserves_knot_artifacts() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_path = dst.path().to_string_lossy().to_string();

        fs::write(src.path().join("file.java"), "class F {}").unwrap();
        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        fs::write(dst.path().join(".knot.lock"), "lock-content").unwrap();
        fs::write(dst.path().join(".knot_state.json"), "{}").unwrap();

        fs::remove_file(src.path().join("file.java")).unwrap();
        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        assert!(dst.path().join(".knot.lock").exists());
        assert!(dst.path().join(".knot_state.json").exists());
    }

    #[test]
    fn test_sync_skips_target_dir() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_path = dst.path().to_string_lossy().to_string();

        // Simulate a Rust project with a fat target/ directory
        fs::create_dir_all(src.path().join("target").join("debug")).unwrap();
        fs::write(
            src.path().join("target").join("debug").join("big_binary"),
            "x".repeat(10_000).as_bytes(),
        )
        .unwrap();
        fs::write(src.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();

        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        assert!(dst.path().join("Cargo.toml").exists());
        assert!(!dst.path().join("target").exists());
        assert!(!dst.path().join("target").join("debug").exists());
    }

    #[test]
    fn test_sync_skips_node_modules_dir() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_path = dst.path().to_string_lossy().to_string();

        fs::create_dir_all(src.path().join("node_modules").join("react")).unwrap();
        fs::write(
            src.path()
                .join("node_modules")
                .join("react")
                .join("index.js"),
            b"module.exports = {};",
        )
        .unwrap();
        fs::write(src.path().join("package.json"), "{}\n").unwrap();

        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        assert!(dst.path().join("package.json").exists());
        assert!(!dst.path().join("node_modules").exists());
    }

    #[test]
    fn test_sync_prunes_existing_artifact_dir() {
        // The mirror was populated by a previous (unfiltered) sync and has
        // accumulated `target/` and `node_modules/`. The next sync must
        // clean them out so they don't linger forever.
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_path = dst.path().to_string_lossy().to_string();

        fs::write(src.path().join("file.java"), "class F {}").unwrap();

        // Seed dst with leftover artifacts from a prior bad run
        fs::create_dir_all(dst.path().join("target")).unwrap();
        fs::write(dst.path().join("target").join("junk.o"), b"junk").unwrap();
        fs::create_dir_all(dst.path().join("node_modules").join("react")).unwrap();
        fs::write(
            dst.path()
                .join("node_modules")
                .join("react")
                .join("index.js"),
            b"old",
        )
        .unwrap();
        // Also a source file already in dst that should be preserved
        fs::write(dst.path().join("file.java"), "class F {}").unwrap();

        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        assert!(dst.path().join("file.java").exists());
        assert!(!dst.path().join("target").exists());
        assert!(!dst.path().join("node_modules").exists());
    }

    #[test]
    fn test_sync_skips_nested_artifact_dir() {
        // Artifact dirs must be skipped wherever they appear in the tree,
        // not only at the root.
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_path = dst.path().to_string_lossy().to_string();

        fs::create_dir_all(src.path().join("services").join("api").join("target")).unwrap();
        fs::write(
            src.path()
                .join("services")
                .join("api")
                .join("target")
                .join("artifact"),
            b"x",
        )
        .unwrap();
        fs::write(
            src.path().join("services").join("api").join("main.rs"),
            b"fn main() {}",
        )
        .unwrap();

        sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

        assert!(
            dst.path()
                .join("services")
                .join("api")
                .join("main.rs")
                .exists()
        );
        assert!(
            !dst.path()
                .join("services")
                .join("api")
                .join("target")
                .exists()
        );
    }

    #[test]
    fn test_should_skip_dir_known_entries() {
        assert!(should_skip_dir("target"));
        assert!(should_skip_dir("node_modules"));
        assert!(should_skip_dir(".gradle"));
        assert!(should_skip_dir("build"));
        assert!(should_skip_dir("dist"));
        assert!(should_skip_dir("__pycache__"));
        assert!(should_skip_dir(".idea"));
    }

    #[test]
    fn test_should_skip_dir_legit_dirs() {
        // Dirs that look superficially similar but must NOT be skipped
        assert!(!should_skip_dir("src"));
        assert!(!should_skip_dir("tests"));
        assert!(!should_skip_dir("targeted")); // suffix-only — we match exact
        assert!(!should_skip_dir("my-target"));
    }

    #[cfg(unix)]
    mod readonly_tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        /// First sync must succeed even when the source file is read-only.
        /// `fs::copy` will mirror the `0o444` permission to the destination;
        /// what matters is that the write itself is allowed.
        #[test]
        fn test_sync_copies_readonly_source_file() {
            let src = TempDir::new().unwrap();
            let dst = TempDir::new().unwrap();
            let dst_path = dst.path().to_string_lossy().to_string();

            let file = src.path().join("ro.txt");
            fs::write(&file, "data").unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();

            sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

            assert_eq!(
                fs::read_to_string(dst.path().join("ro.txt")).unwrap(),
                "data"
            );
        }

        /// The critical regression: after a first sync, the destination file
        /// is read-only (mirrored from source). The second sync would fail
        /// with `Permission denied (os error 13)` without `make_writable`.
        #[test]
        fn test_sync_overwrites_readonly_destination_file() {
            let src = TempDir::new().unwrap();
            let dst = TempDir::new().unwrap();
            let dst_path = dst.path().to_string_lossy().to_string();

            fs::write(src.path().join("a.txt"), "v1").unwrap();
            sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

            // Force the destination read-only to simulate the failure mode
            // (e.g. second sync mirroring a `chmod 444` source).
            let dst_file = dst.path().join("a.txt");
            fs::set_permissions(&dst_file, fs::Permissions::from_mode(0o444)).unwrap();

            // Modify the source and re-sync. Without `make_writable` this
            // would fail with EACCES from `fs::copy` opening the dst.
            fs::write(src.path().join("a.txt"), "v2").unwrap();
            sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

            assert_eq!(fs::read_to_string(&dst_file).unwrap(), "v2");
        }

        /// Re-syncing must also work when the destination *directory* is
        /// read-only (`0o555`). Without `make_writable`, the inner
        /// `fs::copy` would fail with EACCES because the parent denies
        /// writes.
        #[test]
        fn test_sync_overwrites_into_readonly_destination_dir() {
            let src = TempDir::new().unwrap();
            let dst = TempDir::new().unwrap();
            let dst_path = dst.path().to_string_lossy().to_string();

            fs::create_dir_all(src.path().join("sub")).unwrap();
            fs::write(src.path().join("sub").join("a.txt"), "v1").unwrap();
            sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

            let dst_sub = dst.path().join("sub");
            fs::set_permissions(&dst_sub, fs::Permissions::from_mode(0o555)).unwrap();

            fs::write(src.path().join("sub").join("a.txt"), "v2").unwrap();
            sync_local_working_tree(src.path().to_str().unwrap(), &dst_path).unwrap();

            assert_eq!(fs::read_to_string(dst_sub.join("a.txt")).unwrap(), "v2");
        }

        /// `make_writable` must be a no-op for paths that don't exist
        /// (e.g. first sync of a brand-new file).
        #[test]
        fn test_make_writable_missing_path_is_noop() {
            let dir = TempDir::new().unwrap();
            let missing = dir.path().join("nope.txt");
            assert!(make_writable(&missing).is_ok());
        }

        /// The production failure: a subdirectory owned by a different
        /// user (or `chmod 0` for any other reason) returns EACCES on
        /// `read_dir`. The sync must not abort — it must skip the
        /// unreadable subtree and copy everything else.
        #[test]
        fn test_sync_skips_unreadable_source_subdir() {
            let src = TempDir::new().unwrap();
            let dst = TempDir::new().unwrap();
            let dst_path = dst.path().to_string_lossy().to_string();

            fs::create_dir_all(src.path().join("readable")).unwrap();
            fs::write(src.path().join("readable").join("ok.txt"), "ok").unwrap();

            fs::create_dir_all(src.path().join("locked")).unwrap();
            fs::write(src.path().join("locked").join("secret.txt"), "secret").unwrap();

            // Strip all perms so even the owner cannot descend.
            fs::set_permissions(src.path().join("locked"), fs::Permissions::from_mode(0o000))
                .unwrap();

            let result = sync_local_working_tree(src.path().to_str().unwrap(), &dst_path);
            assert!(
                result.is_ok(),
                "sync must tolerate unreadable subdirs, got: {:?}",
                result.err()
            );

            // The readable subtree is mirrored normally.
            assert!(dst.path().join("readable").join("ok.txt").exists());
            assert_eq!(
                fs::read_to_string(dst.path().join("readable").join("ok.txt")).unwrap(),
                "ok"
            );
            // The locked subtree is not mirrored.
            assert!(!dst.path().join("locked").exists());

            // Restore perms so TempDir cleanup succeeds.
            let _ =
                fs::set_permissions(src.path().join("locked"), fs::Permissions::from_mode(0o755));
        }

        /// A source file owned by a different user (or `chmod 0`) cannot
        /// be read with `fs::copy` (EACCES on open). The sync must skip
        /// the file and keep going.
        #[test]
        fn test_sync_skips_unreadable_source_file() {
            let src = TempDir::new().unwrap();
            let dst = TempDir::new().unwrap();
            let dst_path = dst.path().to_string_lossy().to_string();

            fs::write(src.path().join("ok.txt"), "ok").unwrap();
            let locked = src.path().join("locked.txt");
            fs::write(&locked, "secret").unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

            let result = sync_local_working_tree(src.path().to_str().unwrap(), &dst_path);
            assert!(
                result.is_ok(),
                "sync must tolerate unreadable files, got: {:?}",
                result.err()
            );

            assert!(dst.path().join("ok.txt").exists());
            // The locked file's bytes are not mirrored.
            assert!(!dst.path().join("locked.txt").exists());

            // Restore for TempDir cleanup.
            let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o644));
        }

        /// Symlinks must be skipped, not followed. A symlink to
        /// `/etc/passwd` (or anywhere outside the source tree) would
        /// otherwise pull unrelated content into the mirror, and a
        /// broken symlink would error out the sync.
        #[test]
        fn test_sync_skips_symlinks() {
            let src = TempDir::new().unwrap();
            let dst = TempDir::new().unwrap();
            let dst_path = dst.path().to_string_lossy().to_string();

            fs::write(src.path().join("real.txt"), "real").unwrap();
            std::os::unix::fs::symlink("real.txt", src.path().join("link_to_real")).unwrap();
            std::os::unix::fs::symlink("/nonexistent/target", src.path().join("broken")).unwrap();
            std::os::unix::fs::symlink("/etc/passwd", src.path().join("escape")).unwrap();

            let result = sync_local_working_tree(src.path().to_str().unwrap(), &dst_path);
            assert!(result.is_ok(), "got: {:?}", result.err());

            assert!(dst.path().join("real.txt").exists());
            // No symlink was followed or copied as a symlink.
            assert!(!dst.path().join("link_to_real").exists());
            assert!(!dst.path().join("broken").exists());
            assert!(!dst.path().join("escape").exists());
        }
    }

    /// Top-level src must still fail clearly when it cannot be read.
    /// A repo whose root dir is unreadable should not silently produce
    /// an empty mirror.
    #[cfg(unix)]
    #[test]
    fn test_sync_fails_on_unreadable_top_level_src() {
        use std::os::unix::fs::PermissionsExt;
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let dst_path = dst.path().to_string_lossy().to_string();

        fs::set_permissions(src.path(), fs::Permissions::from_mode(0o000)).unwrap();

        let result = sync_local_working_tree(src.path().to_str().unwrap(), &dst_path);
        assert!(result.is_err(), "expected top-level EACCES to fail loudly");

        // Restore for TempDir cleanup.
        let _ = fs::set_permissions(src.path(), fs::Permissions::from_mode(0o755));
    }

    fn seed_state_file(repo: &Path, content: &str) {
        let knot_dir = repo.join(".knot");
        fs::create_dir_all(&knot_dir).unwrap();
        fs::write(knot_dir.join("index_state.json"), content).unwrap();
    }

    #[test]
    fn test_clear_stale_index_state_no_file() {
        let dir = TempDir::new().unwrap();
        assert!(!clear_stale_index_state(dir.path().to_str().unwrap()));
    }

    #[test]
    fn test_clear_stale_index_state_missing_version() {
        let dir = TempDir::new().unwrap();
        seed_state_file(dir.path(), r#"{"file_hashes":{"a.java":"deadbeef"}}"#);

        let removed = clear_stale_index_state(dir.path().to_str().unwrap());
        assert!(removed);
        assert!(!dir.path().join(".knot").join("index_state.json").exists());
    }

    #[test]
    fn test_clear_stale_index_state_with_version() {
        let dir = TempDir::new().unwrap();
        seed_state_file(
            dir.path(),
            r#"{"version":3,"file_hashes":{"a.java":"deadbeef"}}"#,
        );

        let removed = clear_stale_index_state(dir.path().to_str().unwrap());
        assert!(!removed);
        assert!(dir.path().join(".knot").join("index_state.json").exists());
    }

    #[test]
    fn test_clear_stale_index_state_corrupt() {
        let dir = TempDir::new().unwrap();
        seed_state_file(dir.path(), "this is not json {{{");

        let removed = clear_stale_index_state(dir.path().to_str().unwrap());
        assert!(!removed);
        // Do not destroy unreadable content — leave it for human inspection.
        assert!(dir.path().join(".knot").join("index_state.json").exists());
    }

    #[test]
    fn test_clear_stale_index_state_not_object() {
        let dir = TempDir::new().unwrap();
        seed_state_file(dir.path(), "[1, 2, 3]");

        let removed = clear_stale_index_state(dir.path().to_str().unwrap());
        assert!(!removed);
        assert!(dir.path().join(".knot").join("index_state.json").exists());
    }
}
