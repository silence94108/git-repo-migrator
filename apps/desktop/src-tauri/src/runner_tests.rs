//! Worker-pool wiring tests.
//!
//! `flow_tests.rs` proves the IPC state machine; this module proves the part
//! Wave 5 was missing — that a started batch actually reaches `git.exe` and
//! comes back with rows in a terminal state. It uses real bare repositories and
//! the real thread pool, so a regression in the wiring fails here rather than
//! on a Windows machine during a migration.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use git_repo_migrator_application::BatchControl;
use git_repo_migrator_domain::{ConflictPolicy, ModuleSelection, RefPolicy, RepoTaskState};
use git_repo_migrator_git_runner::{GitRunner, RunOptions};
use rusqlite::params;

use crate::events::RecordingSink;
use crate::ports::{BatchLauncher, Clock};
use crate::runner::ThreadPoolLauncher;
use crate::state::AppState;

const BATCH: &str = "batch-1";

struct StepClock {
    now: AtomicI64,
}

impl Clock for StepClock {
    fn now_ms(&self) -> i64 {
        self.now.fetch_add(250, Ordering::Relaxed)
    }
}

fn git() -> GitRunner {
    GitRunner::system().expect("system git")
}

fn run(runner: &GitRunner, dir: &Path, args: &[&str]) {
    runner
        .run(
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            RunOptions {
                current_dir: Some(dir.to_path_buf()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|error| panic!("git {args:?} failed: {error}"));
}

/// A bare source repository with a branch, a tag and a platform-private ref.
fn source_repository(runner: &GitRunner, root: &Path, name: &str) -> PathBuf {
    let work = root.join(format!("{name}-work"));
    let bare = root.join(format!("{name}.git"));
    fs::create_dir_all(&work).expect("work dir");
    run(runner, &work, &["init", "-b", "main"]);
    run(runner, &work, &["config", "user.name", "Migrator Test"]);
    run(
        runner,
        &work,
        &["config", "user.email", "migrator@example.test"],
    );
    fs::write(work.join("README.md"), format!("{name} fixture\n")).expect("readme");
    run(runner, &work, &["add", "README.md"]);
    run(runner, &work, &["commit", "-m", "initial"]);
    run(runner, &work, &["tag", "v1.0.0"]);
    run(
        runner,
        root,
        &["init", "--bare", bare.to_str().expect("path")],
    );
    run(
        runner,
        &work,
        &["remote", "add", "origin", bare.to_str().expect("path")],
    );
    run(
        runner,
        &work,
        &["push", "origin", "refs/heads/main", "refs/tags/v1.0.0"],
    );
    run(
        runner,
        &bare,
        &["update-ref", "refs/pull/1/head", "refs/heads/main"],
    );
    bare
}

fn empty_bare(runner: &GitRunner, root: &Path, name: &str) -> PathBuf {
    let path = root.join(format!("{name}.git"));
    run(
        runner,
        root,
        &["init", "--bare", path.to_str().expect("path")],
    );
    path
}

fn target_tips(runner: &GitRunner, repo: &Path) -> Vec<String> {
    runner
        .run(
            &["for-each-ref".to_owned(), "--format=%(refname)".to_owned()],
            RunOptions {
                current_dir: Some(repo.to_path_buf()),
                ..Default::default()
            },
        )
        .expect("for-each-ref")
        .stdout
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Seeds a frozen plan and a running batch straight into SQLite.
///
/// `plan.freeze` only accepts HTTP(S)/SSH URLs, which local fixture paths are
/// not; seeding keeps this test focused on the pool wiring while
/// `flow_tests.rs` covers the plan gate itself.
fn seed(state: &AppState, tasks: &[(String, String)]) {
    let modules = serde_json::to_string(&ModuleSelection {
        lfs: false,
        metadata: false,
        issues: false,
        pull_requests: false,
        wiki: false,
        releases: false,
    })
    .expect("modules");
    let policy =
        serde_json::to_string(&(ConflictPolicy::default(), RefPolicy::default())).expect("policy");

    state
        .with_connection_for_test(|connection| {
            connection.execute(
                "INSERT INTO connection (id, platform_type, endpoint, credential_ref, created_at_ms)
                 VALUES ('source', 'generic_git', 'file://source', 'credential/windows/demo', 1)",
                [],
            )?;
            connection.execute(
                "INSERT INTO plan (id, selection_json, policy_json, module_json, plan_hash,
                                   status, created_at_ms)
                 VALUES ('plan-1', '{}', ?1, ?2, 'seeded-hash', 'frozen', 1)",
                params![policy, modules],
            )?;
            connection.execute(
                "INSERT INTO batch (id, plan_id, status, total, completed, failed, started_at_ms)
                 VALUES (?1, 'plan-1', 'running', ?2, 0, 0, 1)",
                params![BATCH, i64::try_from(tasks.len()).unwrap_or(0)],
            )?;
            for (index, (source, target)) in tasks.iter().enumerate() {
                let candidate = format!("candidate-{index}");
                connection.execute(
                    "INSERT INTO repository_candidate
                        (id, connection_id, source_url, name, namespace, visibility, role,
                         metadata_json)
                     VALUES (?1, 'source', ?2, ?3, 'ops', 'private', 'full_migration', '{}')",
                    params![candidate, source, format!("repo{index}")],
                )?;
                connection.execute(
                    "INSERT INTO repository_task
                        (id, batch_id, candidate_id, target_url, action, status, attempt,
                         updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, 'reuse_empty', 'planned', 0, 1)",
                    params![format!("task-{index}"), BATCH, candidate, target],
                )?;
            }
            Ok(())
        })
        .expect("seeded rows");
}

struct Fixture {
    state: Arc<AppState>,
    events: Arc<RecordingSink>,
    launcher: Arc<ThreadPoolLauncher>,
    _temp: tempfile::TempDir,
}

fn fixture(tasks: &[(String, String)], temp: tempfile::TempDir, root: &Path) -> Fixture {
    let state = Arc::new(
        AppState::in_memory()
            .expect("in-memory store")
            .with_clock(Arc::new(StepClock {
                now: AtomicI64::new(1_700_000_000_000),
            })),
    );
    seed(&state, tasks);

    let events = Arc::new(RecordingSink::default());
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).expect("workspace dir");
    let launcher = Arc::new(ThreadPoolLauncher::new(
        Arc::downgrade(&state),
        events.clone(),
        workspace,
        Arc::new(git_repo_migrator_credential_store::CredentialStore::in_memory()),
    ));
    state.install_launcher(launcher.clone());
    Fixture {
        state,
        events,
        launcher,
        _temp: temp,
    }
}

/// Waits until the batch leaves `running`, so a wiring regression surfaces as a
/// timeout instead of a flaky assertion.
fn wait_for_end(state: &AppState) -> BatchControl {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let control = state.batch_control(BATCH);
        if control != BatchControl::Running || Instant::now() > deadline {
            return control;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn task_states(state: &AppState) -> Vec<(String, RepoTaskState)> {
    state
        .with_connection_for_test(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, status FROM repository_task WHERE batch_id = ?1 ORDER BY id",
            )?;
            let rows = statement
                .query_map(params![BATCH], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .expect("task rows")
        .into_iter()
        .map(|(id, status)| (id, crate::snapshot::parse_task_state(&status)))
        .collect()
}

#[test]
fn a_started_batch_drives_every_repository_through_git_to_a_terminal_state() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().to_path_buf();
    let runner = git();
    let mut pairs = Vec::new();
    let mut targets = Vec::new();
    for index in 0..3 {
        let source = source_repository(&runner, &root, &format!("source{index}"));
        let target = empty_bare(&runner, &root, &format!("target{index}"));
        pairs.push((
            source.to_str().expect("path").to_owned(),
            target.to_str().expect("path").to_owned(),
        ));
        targets.push(target);
    }

    let fixture = fixture(&pairs, temp, &root);
    fixture.launcher.launch(BATCH, 2);

    assert_eq!(
        wait_for_end(&fixture.state),
        BatchControl::Completed,
        "the pool must drive the batch to completion"
    );
    for (task_id, state) in task_states(&fixture.state) {
        assert_eq!(
            state,
            RepoTaskState::Succeeded,
            "{task_id} did not reach a successful terminal state"
        );
    }

    for target in &targets {
        let tips = target_tips(&runner, target);
        assert!(tips.iter().any(|name| name == "refs/heads/main"));
        assert!(tips.iter().any(|name| name == "refs/tags/v1.0.0"));
        assert!(
            !tips.iter().any(|name| name.starts_with("refs/pull/")),
            "platform-private refs must never reach the target"
        );
    }

    let envelopes = fixture.events.envelopes();
    assert!(!envelopes.is_empty(), "the run must emit progress events");
    for envelope in &envelopes {
        assert!(
            crate::events::forbidden_keys(envelope).is_empty(),
            "event leaked a forbidden key: {envelope:?}"
        );
    }
}

#[test]
fn launching_the_same_batch_twice_does_not_double_the_workers() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().to_path_buf();
    let runner = git();
    let source = source_repository(&runner, &root, "source0");
    let target = empty_bare(&runner, &root, "target0");
    let pairs = vec![(
        source.to_str().expect("path").to_owned(),
        target.to_str().expect("path").to_owned(),
    )];

    let fixture = fixture(&pairs, temp, &root);
    fixture.launcher.launch(BATCH, 1);
    fixture.launcher.launch(BATCH, 1);

    assert_eq!(wait_for_end(&fixture.state), BatchControl::Completed);
    let states = task_states(&fixture.state);
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].1, RepoTaskState::Succeeded);
    // One repository migrated once: a doubled pool would have produced a second
    // attempt and a rejected push.
    assert_eq!(
        target_tips(&runner, &target)
            .iter()
            .filter(|name| name.as_str() == "refs/heads/main")
            .count(),
        1
    );
}

#[test]
fn cancelling_stops_the_pool_and_leaves_finished_work_alone() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().to_path_buf();
    let runner = git();
    let source = source_repository(&runner, &root, "source0");
    let target = empty_bare(&runner, &root, "target0");
    let pairs = vec![(
        source.to_str().expect("path").to_owned(),
        target.to_str().expect("path").to_owned(),
    )];

    let fixture = fixture(&pairs, temp, &root);
    // Cancel before any worker starts: nothing may be written to the target.
    fixture
        .state
        .with_connection_for_test(|connection| {
            connection.execute(
                "UPDATE batch SET status = 'cancelled' WHERE id = ?1",
                params![BATCH],
            )
        })
        .expect("cancel");
    fixture.launcher.launch(BATCH, 1);
    fixture.launcher.cancel(BATCH);

    assert_eq!(wait_for_end(&fixture.state), BatchControl::Cancelled);
    assert_eq!(task_states(&fixture.state)[0].1, RepoTaskState::Planned);
    assert!(target_tips(&runner, &target).is_empty());
}

#[test]
fn a_missing_target_is_refused_with_an_actionable_error() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().to_path_buf();
    let runner = git();
    let source = source_repository(&runner, &root, "source0");
    let missing = root.join("never-created.git");
    let pairs = vec![(
        source.to_str().expect("path").to_owned(),
        missing.to_str().expect("path").to_owned(),
    )];

    let fixture = fixture(&pairs, temp, &root);
    fixture.launcher.launch(BATCH, 1);
    wait_for_end(&fixture.state);

    let states = task_states(&fixture.state);
    assert_eq!(
        states[0].1,
        RepoTaskState::Skipped,
        "an unauthorised creation is a skip, never a silent success"
    );
    assert!(!missing.exists());
}
