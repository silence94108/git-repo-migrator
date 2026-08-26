use git_repo_migrator_domain::{RefPolicy, RefPolicyDecision};

#[test]
fn platform_private_refs_never_enter_push_refspecs() {
    let policy = RefPolicy::default();
    let samples = [
        "refs/pull/42/head",
        "refs/merge-requests/7/head",
        "refs/changes/12/1012/3",
        "refs/remotes/origin/main",
        "refs/notes/review",
    ];

    for sample in samples {
        assert_ne!(policy.decide(sample), RefPolicyDecision::Allow, "{sample}");
    }
    assert_eq!(
        policy.allowed_refspecs(),
        ["+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*",]
    );
}
