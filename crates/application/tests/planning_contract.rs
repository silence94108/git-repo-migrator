use git_repo_migrator_application::{build_preview, Candidate, SelectionSet, TargetState};
use git_repo_migrator_domain::ConflictPolicy;
use std::collections::HashMap;

#[test]
fn hundred_repository_selection_is_not_page_bound() {
    let candidates: Vec<_> = (0..100)
        .map(|i| Candidate {
            id: i.to_string(),
            source_url: format!("https://source/{i}"),
            name: format!("repo-{i}"),
            namespace: "team".into(),
            target_url: Some(format!("https://target/{i}")),
            target_name: None,
        })
        .collect();
    let mut selection = SelectionSet::select_all((0..100).map(|i| i.to_string()));
    selection.exclude("17");
    let states = candidates
        .iter()
        .map(|c| (c.id.clone(), TargetState::Missing))
        .collect::<HashMap<_, _>>();
    let preview = build_preview(
        &selection,
        &candidates,
        &states,
        ConflictPolicy::default(),
        "cap-v1",
    );
    assert_eq!(preview.mappings.len(), 99);
    assert!(preview.blocking.is_empty());
}

#[test]
fn overwrite_requires_confirmation_before_freeze() {
    let candidate = Candidate {
        id: "a".into(),
        source_url: "https://s/a".into(),
        name: "a".into(),
        namespace: "n".into(),
        target_url: Some("https://t/a".into()),
        target_name: None,
    };
    let policy = ConflictPolicy {
        skip_non_empty: false,
        allow_overwrite: true,
        ..ConflictPolicy::default()
    };
    let states = HashMap::from([(String::from("a"), TargetState::NonEmpty)]);
    let preview = build_preview(
        &SelectionSet::select_all(vec!["a".into()]),
        &[candidate],
        &states,
        policy.clone(),
        "cap",
    );
    assert!(!preview.can_freeze(false));
    assert!(preview.freeze(Default::default(), policy, true).is_ok());
}
