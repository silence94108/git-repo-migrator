//! Workspace path and disk safety contract (CM-007, T-011).
//!
//! Everything the migrator writes lands under one operator-chosen root. These
//! tests pin the boundary itself: a path that resolves outside the root must be
//! rejected *before* any file system call, the root must never be deletable
//! through the cleanup helper, and a second process must not be able to share
//! the same workspace.

use std::fs;
use std::path::{Path, PathBuf};

use git_repo_migrator_workspace::{Workspace, WorkspaceError};

fn workspace() -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().expect("temp dir");
    let workspace = Workspace::new(dir.path()).expect("workspace root");
    (dir, workspace)
}

#[test]
fn child_rejects_every_escape_shape() {
    let (_guard, workspace) = workspace();

    for name in [
        "..",
        "../escape",
        "..\\escape",
        "nested/../../escape",
        "nested\\..\\..\\escape",
        "",
    ] {
        assert!(
            matches!(workspace.child(name), Err(WorkspaceError::OutsideRoot)),
            "child({name:?}) must be rejected as an escape"
        );
    }
}

#[test]
fn child_rejects_an_absolute_path_from_another_root() {
    let (_guard, workspace) = workspace();
    let elsewhere = tempfile::tempdir().expect("second temp dir");
    let absolute = elsewhere
        .path()
        .join("payload")
        .to_str()
        .expect("utf-8 path")
        .to_owned();

    // `Path::join` with an absolute component replaces the root, so this is the
    // shape that would silently write outside the workspace if unchecked.
    assert!(matches!(
        workspace.child(&absolute),
        Err(WorkspaceError::OutsideRoot)
    ));
    assert!(!elsewhere.path().join("payload").exists());
}

#[test]
fn child_rejects_an_interior_nul_byte() {
    let (_guard, workspace) = workspace();
    assert!(matches!(
        workspace.child("repo\0name"),
        Err(WorkspaceError::OutsideRoot)
    ));
}

#[test]
fn child_creates_nested_directories_inside_the_root() {
    let (_guard, workspace) = workspace();
    let nested = workspace.child("mirrors/team/repo.git").expect("nested");
    assert!(nested.is_dir());
    assert!(nested.starts_with(workspace.root()));
}

#[test]
fn cleanup_temp_refuses_the_root_itself() {
    let (_guard, workspace) = workspace();
    let root = workspace.root().to_path_buf();

    assert!(matches!(
        workspace.cleanup_temp(&root),
        Err(WorkspaceError::OutsideRoot)
    ));
    assert!(root.is_dir(), "the workspace root must survive cleanup");
}

#[test]
fn cleanup_temp_refuses_a_sibling_directory() {
    let (_guard, workspace) = workspace();
    let elsewhere = tempfile::tempdir().expect("second temp dir");
    let victim = elsewhere.path().join("keep-me");
    fs::create_dir(&victim).expect("victim dir");
    fs::write(victim.join("data.txt"), b"important").expect("victim file");

    assert!(matches!(
        workspace.cleanup_temp(&victim),
        Err(WorkspaceError::OutsideRoot)
    ));
    assert!(victim.join("data.txt").exists());
}

#[test]
fn cleanup_temp_refuses_a_traversal_that_lands_outside() {
    let (_guard, workspace) = workspace();
    let elsewhere = tempfile::tempdir().expect("second temp dir");
    let victim = elsewhere.path().join("keep-me");
    fs::create_dir(&victim).expect("victim dir");

    // A relative traversal starting inside the workspace still resolves out.
    let traversal: PathBuf = workspace.root().join("..").join(
        victim
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("keep-me"),
    );
    assert!(matches!(
        workspace.cleanup_temp(&traversal),
        Err(WorkspaceError::OutsideRoot)
    ));
    assert!(victim.is_dir());
}

#[test]
fn cleanup_temp_removes_only_the_named_workspace_directory() {
    let (_guard, workspace) = workspace();
    let doomed = workspace.temp_dir("task-1").expect("temp dir");
    let keeper = workspace.child("mirrors").expect("sibling dir");
    fs::write(doomed.join("mirror.pack"), b"objects").expect("payload");

    workspace.cleanup_temp(&doomed).expect("cleanup");

    assert!(!doomed.exists());
    assert!(
        keeper.is_dir(),
        "cleanup must not touch sibling directories"
    );
    assert!(workspace.root().is_dir());
}

#[test]
fn cleanup_temp_is_idempotent() {
    let (_guard, workspace) = workspace();
    let temp = workspace.temp_dir("task-1").expect("temp dir");
    workspace.cleanup_temp(&temp).expect("first cleanup");
    // A crash between removal and checkpoint write replays this call; a second
    // cleanup must not turn into an error the operator has to resolve.
    workspace.cleanup_temp(&temp).expect("second cleanup");
    assert!(!temp.exists());
}

#[test]
fn temp_dirs_are_unique_and_stay_inside_the_root() {
    let (_guard, workspace) = workspace();
    let first = workspace.temp_dir("task-1").expect("first");
    let second = workspace.temp_dir("task-1").expect("second");

    assert_ne!(
        first, second,
        "a resumed attempt must not reuse a half-written directory"
    );
    for path in [&first, &second] {
        assert!(path.starts_with(workspace.root()));
        assert!(path.is_dir());
    }
}

#[test]
fn lock_is_exclusive_and_released_on_drop() {
    let (_guard, workspace) = workspace();
    let other_handle = Workspace::new(workspace.root()).expect("second handle");

    let held = workspace.lock().expect("first lock");
    assert!(matches!(
        other_handle.lock(),
        Err(WorkspaceError::AlreadyLocked)
    ));

    drop(held);
    let reacquired = other_handle.lock().expect("lock after release");
    drop(reacquired);
    assert!(!Path::new(workspace.root()).join(".workspace.lock").exists());
}

#[test]
fn preflight_space_blocks_an_estimate_larger_than_the_volume() {
    let (_guard, workspace) = workspace();

    workspace
        .preflight_space(0)
        .expect("a zero-byte estimate always fits");

    match workspace.preflight_space(u64::MAX) {
        Err(WorkspaceError::InsufficientSpace {
            required,
            available,
        }) => {
            assert_eq!(required, u64::MAX);
            assert!(
                available < u64::MAX,
                "the error must report what is actually free so the UI can explain it"
            );
        }
        other => panic!("expected an insufficient-space error, got {other:?}"),
    }
}

#[test]
fn a_missing_root_is_reported_instead_of_being_created() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("not-created-yet");
    assert!(matches!(
        Workspace::new(&missing),
        Err(WorkspaceError::Io(_))
    ));
    assert!(!missing.exists());
}
