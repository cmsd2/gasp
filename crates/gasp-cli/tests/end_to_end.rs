//! Integration tests that drive the `gasp` binary against local bare
//! repos created with `git`. Covers M1 (init, list) and M2 (sync clone).

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_gasp");

struct Fixture {
    _root: TempDir,
    workspace: PathBuf,
    bare: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        let workspace = root.path().join("ws");
        let bare = root.path().join("bare");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&bare).unwrap();
        Self {
            _root: root,
            workspace,
            bare,
        }
    }

    /// Build a bare repo at `bare/<name>.git` containing a single commit
    /// on `main` with a README. Returns the path suitable for cloning.
    fn make_bare_repo(&self, name: &str) -> PathBuf {
        let src = self._root.path().join("src").join(name);
        std::fs::create_dir_all(&src).unwrap();
        run(&src, "git", &["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("README.md"), format!("{name}\n")).unwrap();
        run(&src, "git", &["add", "-A"]);
        run(
            &src,
            "git",
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        let bare_path = self.bare.join(format!("{name}.git"));
        run(
            self._root.path(),
            "git",
            &[
                "clone",
                "--bare",
                "-q",
                src.to_str().unwrap(),
                bare_path.to_str().unwrap(),
            ],
        );
        bare_path
    }

    fn write_manifest(&self, contents: &str) -> PathBuf {
        let path = self.workspace.join("seed.toml");
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn gasp(&self, args: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.workspace)
            .output()
            .expect("run gasp")
    }
}

fn run(cwd: &Path, prog: &str, args: &[&str]) {
    let status = Command::new(prog)
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("spawn {prog}: {e}"));
    assert!(status.success(), "{prog} {args:?} failed");
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn new_writes_skeleton_to_default_path() {
    let f = Fixture::new();
    let out = f.gasp(&["new"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let manifest = f.workspace.join("workspace.toml");
    assert!(manifest.is_file());
    let body = std::fs::read_to_string(&manifest).unwrap();
    assert!(body.contains("version = 1"));
    assert!(body.contains("[defaults]"));
}

#[test]
fn new_skeleton_is_a_valid_manifest_for_init() {
    let f = Fixture::new();
    assert!(f.gasp(&["new"]).status.success());
    let out = f.gasp(&["init", "workspace.toml"]);
    assert!(
        out.status.success(),
        "skeleton should be parseable: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn new_refuses_to_overwrite_existing_file() {
    let f = Fixture::new();
    std::fs::write(f.workspace.join("workspace.toml"), "already here\n").unwrap();
    let out = f.gasp(&["new"]);
    assert!(!out.status.success());
    let body = std::fs::read_to_string(f.workspace.join("workspace.toml")).unwrap();
    assert_eq!(body, "already here\n");
}

#[test]
fn new_writes_to_explicit_path() {
    let f = Fixture::new();
    let dest = f.workspace.join("custom-name.toml");
    let out = f.gasp(&["new", dest.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(dest.is_file());
}

#[test]
fn init_creates_dot_workspace_with_manifest_copy() {
    let f = Fixture::new();
    let seed = f.write_manifest("version = 1\n");
    let out = f.gasp(&["init", seed.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let manifest = f.workspace.join(".workspace").join("workspace.toml");
    assert!(manifest.is_file());
    assert_eq!(std::fs::read_to_string(manifest).unwrap(), "version = 1\n");
}

#[test]
fn init_refuses_invalid_manifest_without_touching_disk() {
    let f = Fixture::new();
    let seed = f.write_manifest("version = 99\n");
    let out = f.gasp(&["init", seed.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(!f.workspace.join(".workspace").exists());
}

#[test]
fn list_prints_repos_after_init() {
    let f = Fixture::new();
    let seed = f.write_manifest(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "acme/alpha"
[[repos]]
name = "beta"
url  = "acme/beta"
revision = "v1"
"#,
    );
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());

    let out = f.gasp(&["list"]);
    assert!(out.status.success());
    let s = stdout_of(&out);
    assert!(s.contains("NAME"));
    assert!(s.contains("alpha"));
    assert!(s.contains("beta"));
    assert!(s.contains("https://github.com/acme/alpha.git"));
    assert!(s.contains("v1"));
}

#[test]
fn list_outside_workspace_errors() {
    let f = Fixture::new();
    let out = f.gasp(&["list"]);
    assert!(!out.status.success());
    let s = String::from_utf8_lossy(&out.stderr);
    assert!(s.contains("not inside a gasp workspace"));
}

#[test]
fn sync_clones_all_repos() {
    let f = Fixture::new();
    let alpha = f.make_bare_repo("alpha");
    let beta = f.make_bare_repo("beta");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
[[repos]]
name = "beta"
url  = "{}"
path = "libs/beta"
"#,
        alpha.display(),
        beta.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());

    let out = f.gasp(&["sync"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(f.workspace.join("alpha").join("README.md").is_file());
    assert!(f.workspace.join("libs/beta").join("README.md").is_file());

    let s = stdout_of(&out);
    assert!(s.contains("2 cloned"));
}

#[test]
fn second_sync_skips_existing_repos() {
    let f = Fixture::new();
    let alpha = f.make_bare_repo("alpha");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
"#,
        alpha.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());
    assert!(f.gasp(&["sync"]).status.success());

    let out = f.gasp(&["sync"]);
    assert!(out.status.success());
    let s = stdout_of(&out);
    assert!(s.contains("0 cloned"));
    // A second sync on a clean, on-target repo counts as "unchanged".
    assert!(s.contains("1 unchanged"), "{s}");
}

#[test]
fn sync_reports_failure_continues_with_others_and_exits_nonzero() {
    let f = Fixture::new();
    let alpha = f.make_bare_repo("alpha");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
[[repos]]
name = "ghost"
url  = "/definitely/does/not/exist.git"
"#,
        alpha.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());

    let out = f.gasp(&["sync"]);
    assert!(!out.status.success());
    let s = stdout_of(&out);
    assert!(s.contains("1 cloned"));
    assert!(s.contains("1 failed"));
    assert!(s.contains("ghost"));
    // alpha was cloned despite ghost failing
    assert!(f.workspace.join("alpha").join("README.md").is_file());
}

#[test]
fn sync_checks_out_specified_revision() {
    let f = Fixture::new();
    // Build a repo with two commits and a tag on the first.
    let src = f._root.path().join("src").join("tagged");
    std::fs::create_dir_all(&src).unwrap();
    run(&src, "git", &["init", "-q", "-b", "main", "."]);
    std::fs::write(src.join("v"), "v1\n").unwrap();
    run(&src, "git", &["add", "-A"]);
    run(
        &src,
        "git",
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "c1",
        ],
    );
    run(&src, "git", &["tag", "v1"]);
    std::fs::write(src.join("v"), "v2\n").unwrap();
    run(&src, "git", &["add", "-A"]);
    run(
        &src,
        "git",
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "c2",
        ],
    );
    let bare = f.bare.join("tagged.git");
    run(
        f._root.path(),
        "git",
        &[
            "clone",
            "--bare",
            "-q",
            src.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );

    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name     = "tagged"
url      = "{}"
revision = "v1"
"#,
        bare.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());
    assert!(f.gasp(&["sync"]).status.success());

    let v = std::fs::read_to_string(f.workspace.join("tagged").join("v")).unwrap();
    assert_eq!(v.trim(), "v1");
}

#[test]
fn status_reports_missing_clean_and_dirty() {
    let f = Fixture::new();
    let a = f.make_bare_repo("alpha");
    let b = f.make_bare_repo("beta");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
revision = "main"
[[repos]]
name = "beta"
url  = "{}"
revision = "main"
[[repos]]
name = "missing-one"
url  = "{}"
"#,
        a.display(),
        b.display(),
        a.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());
    // Clone only alpha and beta — skip the third by deleting it after clone
    assert!(f.gasp(&["sync"]).status.success());
    std::fs::remove_dir_all(f.workspace.join("missing-one")).unwrap();

    // Dirty alpha
    std::fs::write(f.workspace.join("alpha").join("README.md"), "modified\n").unwrap();

    let out = f.gasp(&["status"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = stdout_of(&out);
    assert!(s.contains("alpha"));
    assert!(s.contains("dirty"));
    assert!(s.contains("beta"));
    assert!(s.contains("clean"));
    assert!(s.contains("missing-one"));
    assert!(s.contains("missing"));
}

#[test]
fn status_strict_exits_zero_when_clean() {
    let f = Fixture::new();
    let a = f.make_bare_repo("alpha");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
revision = "main"
"#,
        a.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());
    assert!(f.gasp(&["sync"]).status.success());

    let out = f.gasp(&["status", "--strict"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn status_strict_exits_nonzero_when_dirty() {
    let f = Fixture::new();
    let a = f.make_bare_repo("alpha");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
revision = "main"
"#,
        a.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());
    assert!(f.gasp(&["sync"]).status.success());
    std::fs::write(f.workspace.join("alpha").join("README.md"), "dirty\n").unwrap();

    let out = f.gasp(&["status", "--strict"]);
    assert!(!out.status.success());
    // Without --strict, status is still informational and exits 0.
    let out = f.gasp(&["status"]);
    assert!(out.status.success());
}

#[test]
fn status_reports_ahead_after_local_commit() {
    let f = Fixture::new();
    let a = f.make_bare_repo("alpha");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
revision = "main"
"#,
        a.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());
    assert!(f.gasp(&["sync"]).status.success());

    // Add a local commit in the workspace clone
    let repo = f.workspace.join("alpha");
    std::fs::write(repo.join("local"), "x\n").unwrap();
    run(&repo, "git", &["add", "-A"]);
    run(
        &repo,
        "git",
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "local",
        ],
    );

    let s = stdout_of(&f.gasp(&["status"]));
    assert!(s.contains("ahead"));
}

/// Push a new commit into `bare_src` (the working repo whose pushes
/// land in the bare repo we cloned from).
fn add_upstream_commit(working_src: &Path, bare: &Path, file: &str, contents: &str) {
    std::fs::write(working_src.join(file), contents).unwrap();
    run(working_src, "git", &["add", "-A"]);
    run(
        working_src,
        "git",
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "advance",
        ],
    );
    run(
        working_src,
        "git",
        &["push", "-q", bare.to_str().unwrap(), "main"],
    );
}

fn add_local_commit(repo: &Path, file: &str, contents: &str, msg: &str) {
    std::fs::write(repo.join(file), contents).unwrap();
    run(repo, "git", &["add", "-A"]);
    run(
        repo,
        "git",
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            msg,
        ],
    );
}

#[test]
fn sync_fast_forwards_behind_repo() {
    let f = Fixture::new();
    let bare = f.make_bare_repo("alpha");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
revision = "main"
"#,
        bare.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());
    assert!(f.gasp(&["sync"]).status.success());

    // Advance upstream
    let src = f._root.path().join("src").join("alpha");
    add_upstream_commit(&src, &bare, "x", "x\n");

    let out = f.gasp(&["sync"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = stdout_of(&out);
    assert!(s.contains("1 updated"), "{s}");
    // Verify the working tree advanced
    assert!(f.workspace.join("alpha").join("x").is_file());
}

#[test]
fn sync_default_skips_when_ahead() {
    let f = Fixture::new();
    let bare = f.make_bare_repo("alpha");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
revision = "main"
"#,
        bare.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());
    assert!(f.gasp(&["sync"]).status.success());

    add_local_commit(&f.workspace.join("alpha"), "local", "x\n", "local");

    let out = f.gasp(&["sync"]);
    assert!(out.status.success());
    let s = stdout_of(&out);
    assert!(s.contains("1 skipped"), "{s}");
    assert!(s.contains("ahead"));
}

#[test]
fn sync_default_skips_when_dirty() {
    let f = Fixture::new();
    let bare = f.make_bare_repo("alpha");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
revision = "main"
"#,
        bare.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());
    assert!(f.gasp(&["sync"]).status.success());

    // Dirty the tree (don't commit)
    std::fs::write(f.workspace.join("alpha").join("README.md"), "uncommitted\n").unwrap();

    let out = f.gasp(&["sync"]);
    assert!(out.status.success());
    let s = stdout_of(&out);
    assert!(s.contains("1 skipped"), "{s}");
    assert!(s.contains("uncommitted"));
}

#[test]
fn sync_reset_clobbers_ahead_and_dirty() {
    let f = Fixture::new();
    let bare = f.make_bare_repo("alpha");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
revision = "main"
"#,
        bare.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());
    assert!(f.gasp(&["sync"]).status.success());

    // Local commit + dirty change
    add_local_commit(&f.workspace.join("alpha"), "local", "x\n", "local");
    std::fs::write(f.workspace.join("alpha").join("README.md"), "dirty\n").unwrap();

    let out = f.gasp(&["sync", "--on-conflict", "reset"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = stdout_of(&out);
    assert!(s.contains("1 reset"), "{s}");
    // The local-only file is gone after reset
    assert!(!f.workspace.join("alpha").join("local").exists());
    // README is back to upstream content
    let readme = std::fs::read_to_string(f.workspace.join("alpha").join("README.md")).unwrap();
    assert_eq!(readme, "alpha\n");
}

#[test]
fn sync_rebase_resolves_divergence() {
    let f = Fixture::new();
    let bare = f.make_bare_repo("alpha");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name = "alpha"
url  = "{}"
revision = "main"
"#,
        bare.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());
    assert!(f.gasp(&["sync"]).status.success());

    // Local commit (touches "local"), upstream commit (touches "x") — no conflict
    add_local_commit(&f.workspace.join("alpha"), "local", "L\n", "local");
    let src = f._root.path().join("src").join("alpha");
    add_upstream_commit(&src, &bare, "x", "U\n");

    let out = f.gasp(&["sync", "--on-conflict", "rebase"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = stdout_of(&out);
    assert!(s.contains("1 rebased"), "{s}");
    // Both upstream and local commit should be present after rebase
    assert!(f.workspace.join("alpha").join("x").is_file());
    assert!(f.workspace.join("alpha").join("local").is_file());
}

#[test]
fn sync_group_filter_restricts_clones() {
    let f = Fixture::new();
    let a = f.make_bare_repo("aa");
    let b = f.make_bare_repo("bb");
    let seed = f.write_manifest(&format!(
        r#"
version = 1
[[repos]]
name   = "aa"
url    = "{}"
groups = ["x"]
[[repos]]
name   = "bb"
url    = "{}"
groups = ["y"]
"#,
        a.display(),
        b.display(),
    ));
    assert!(f.gasp(&["init", seed.to_str().unwrap()]).status.success());

    let out = f.gasp(&["sync", "--group", "x"]);
    assert!(out.status.success());
    assert!(f.workspace.join("aa").exists());
    assert!(!f.workspace.join("bb").exists());
}
