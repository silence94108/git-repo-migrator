use git_repo_migrator_domain::{RefClassification, RefPolicy, RefPolicyDecision};
use git_repo_migrator_git_runner::{discover_refs, push_allowlisted_refs, verify_refs, GitRunner, RunOptions};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn run(runner: &GitRunner, dir: &Path, args: &[&str]) {
    runner
        .run(
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            RunOptions {
                current_dir: Some(dir.to_path_buf()),
                ..Default::default()
            },
        )
        .unwrap();
}

#[test]
fn migrates_heads_and_tags_without_private_refs() {
    let temp = tempfile::tempdir().unwrap();
    let work = temp.path().join("work");
    let source = temp.path().join("source.git");
    let target = temp.path().join("target.git");
    fs::create_dir(&work).unwrap();
    let runner = GitRunner::system().unwrap();

    run(&runner, &work, &["init", "-b", "main"]);
    run(&runner, &work, &["config", "user.name", "Migrator Test"]);
    run(&runner, &work, &["config", "user.email", "migrator@example.test"]);
    fs::write(work.join("README.md"), "migration fixture\n").unwrap();
    run(&runner, &work, &["add", "README.md"]);
    run(&runner, &work, &["commit", "-m", "initial"]);
    run(&runner, &work, &["tag", "v1.0.0"]);
    run(&runner, temp.path(), &["init", "--bare", source.to_str().unwrap()]);
    run(&runner, &work, &["remote", "add", "origin", source.to_str().unwrap()]);
    run(&runner, &work, &["push", "origin", "refs/heads/main", "refs/tags/v1.0.0"]);
    run(&runner, &source, &["update-ref", "refs/pull/1/head", "refs/heads/main"]);
    run(&runner, temp.path(), &["init", "--bare", target.to_str().unwrap()]);

    let policy = RefPolicy::default();
    let source_refs = discover_refs(&runner, &source, &policy).unwrap();
    assert!(source_refs.iter().any(|entry| entry.name == "refs/heads/main"));
    assert!(source_refs.iter().any(|entry| entry.name == "refs/tags/v1.0.0"));
    assert!(source_refs.iter().any(|entry| {
        entry.name == "refs/pull/1/head"
            && entry.classification == RefClassification::PlatformPrivate
            && entry.decision == RefPolicyDecision::Ignore
    }));

    push_allowlisted_refs(&runner, &source, target.to_str().unwrap(), &source_refs, &policy).unwrap();
    let target_refs = discover_refs(&runner, &target, &policy).unwrap();
    let target_map = target_refs
        .iter()
        .map(|entry| (entry.name.clone(), entry.oid.clone()))
        .collect::<BTreeMap<_, _>>();
    let verification = verify_refs(&source_refs, &target_map);
    assert!(verification.matched);
    assert!(verification.excluded.iter().any(|name| name == "refs/pull/1/head"));
    assert!(!target_map.contains_key("refs/pull/1/head"));
}
