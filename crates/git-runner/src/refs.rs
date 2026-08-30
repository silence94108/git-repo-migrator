use crate::{GitError, GitRunner, RunOptions};
use git_repo_migrator_domain::{RefClassification, RefPolicy, RefPolicyDecision};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    pub name: String,
    pub oid: String,
    pub classification: RefClassification,
    pub decision: RefPolicyDecision,
}

pub fn discover_refs(
    runner: &GitRunner,
    repo: &std::path::Path,
    policy: &RefPolicy,
) -> Result<Vec<RefEntry>, GitError> {
    let out = runner.run(
        &[
            "for-each-ref".into(),
            "--format=%(refname) %(objectname)".into(),
        ],
        RunOptions {
            current_dir: Some(repo.to_path_buf()),
            ..Default::default()
        },
    )?;
    Ok(out
        .stdout
        .lines()
        .filter_map(|line| {
            let (name, oid) = line.split_once(' ')?;
            Some(RefEntry {
                name: name.into(),
                oid: oid.into(),
                classification: RefPolicy::classify(name),
                decision: policy.decide(name),
            })
        })
        .collect())
}

pub fn build_allowlisted_refspecs(entries: &[RefEntry], _policy: &RefPolicy) -> Vec<String> {
    entries
        .iter()
        .filter(|e| {
            e.decision == RefPolicyDecision::Allow
                && matches!(
                    e.classification,
                    RefClassification::Branch | RefClassification::Tag
                )
        })
        .map(|e| format!("+{}:{}", e.name, e.name))
        .collect()
}

/// Pushes the allowlisted refs to the target.
///
/// `auth` carries non-secret environment values (an askpass program and a
/// credential *reference*) that let Git authenticate an HTTP target without the
/// token ever entering argv. Local targets pass an empty map.
pub fn push_allowlisted_refs(
    runner: &GitRunner,
    repo: &std::path::Path,
    target_url: &str,
    entries: &[RefEntry],
    policy: &RefPolicy,
    auth: &std::collections::BTreeMap<String, String>,
) -> Result<crate::GitOutput, GitError> {
    if target_url.contains('@') {
        return Err(GitError::InvalidArgument(
            "target URL userinfo is forbidden".into(),
        ));
    }
    let specs = build_allowlisted_refspecs(entries, policy);
    if specs.is_empty() {
        return Err(GitError::InvalidArgument(
            "no allowlisted refs to push".into(),
        ));
    }
    let mut env = std::collections::BTreeMap::new();
    // A GUI child process must never open an interactive credential prompt;
    // an unauthenticated push has to fail fast and become a visible error.
    env.insert("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned());
    env.extend(auth.clone());
    let mut args = vec!["push".into(), target_url.into()];
    args.extend(specs);
    runner.run(
        &args,
        RunOptions {
            current_dir: Some(repo.to_path_buf()),
            env,
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn excludes_private_refs() {
        let p = RefPolicy::default();
        let entries = vec![
            RefEntry {
                name: "refs/heads/main".into(),
                oid: "a".into(),
                classification: RefClassification::Branch,
                decision: RefPolicyDecision::Allow,
            },
            RefEntry {
                name: "refs/pull/1/head".into(),
                oid: "b".into(),
                classification: RefClassification::PlatformPrivate,
                decision: RefPolicyDecision::Ignore,
            },
        ];
        let s = build_allowlisted_refspecs(&entries, &p);
        assert_eq!(s, vec!["+refs/heads/main:refs/heads/main"]);
    }
    #[test]
    fn no_mirror_or_prune() {
        let p = RefPolicy::default();
        let s = build_allowlisted_refspecs(&[], &p).join(" ");
        assert!(!s.contains("mirror"));
        assert!(!s.contains("prune"));
    }
}
