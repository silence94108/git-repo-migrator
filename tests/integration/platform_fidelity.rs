use git_repo_migrator_application::platform_modules::ItemFailure;
use git_repo_migrator_application::{execute_module, retry_failed_items, PlatformItem};
use git_repo_migrator_platform_core::{Fidelity, PlatformModule};

fn item(id: &str, mapped_author: Option<&str>) -> PlatformItem {
    PlatformItem {
        source_id: id.into(),
        source_url: format!("https://source.example/items/{id}"),
        title: "title".into(),
        body: "body".into(),
        source_author: "source-user".into(),
        mapped_target_author: mapped_author.map(str::to_owned),
        source_state: "merged".into(),
        mapped_target_state: Some("closed".into()),
        attachments: vec![format!("https://source.example/assets/{id}.zip")],
    }
}

#[test]
fn fidelity_tiers_are_counted_separately_and_archive_is_not_native() {
    let items = [item("1", None), item("2", Some("target-user"))];
    let native = execute_module(
        "b",
        "t",
        "team/repo",
        PlatformModule::Issues,
        Fidelity::NativeRebuild,
        &items,
        |item| Ok(format!("target-{}", item.source_id)),
    );
    let archive = execute_module(
        "b",
        "t",
        "team/repo",
        PlatformModule::PullRequests,
        Fidelity::ReadOnlyArchive,
        &items,
        |_| unreachable!(),
    );
    let unsupported = execute_module(
        "b",
        "t",
        "team/repo",
        PlatformModule::Wiki,
        Fidelity::Unsupported,
        &items,
        |_| unreachable!(),
    );
    assert_eq!((native.migrated, native.archived), (2, 0));
    assert_eq!((archive.migrated, archive.archived), (0, 2));
    assert!(archive.archive.as_ref().unwrap().read_only);
    assert_eq!((unsupported.migrated, unsupported.archived), (0, 0));
    assert_eq!(native.identity_mapping[0].target_author, None);
}

#[test]
fn partial_retry_only_runs_failed_retryable_items() {
    let items = [item("1", None), item("2", None), item("3", None)];
    let mut execution = execute_module(
        "b",
        "t",
        "team/repo",
        PlatformModule::Releases,
        Fidelity::NativeRebuild,
        &items,
        |item| {
            if item.source_id == "1" {
                Ok("target-1".into())
            } else {
                Err(ItemFailure {
                    source_id: item.source_id.clone(),
                    code: if item.source_id == "2" {
                        "network"
                    } else {
                        "permission"
                    }
                    .into(),
                    retryable: item.source_id == "2",
                    safe_message: "failed".into(),
                    action: "review".into(),
                })
            }
        },
    );
    let mut retried = vec![];
    retry_failed_items(&mut execution, &items, |item| {
        retried.push(item.source_id.clone());
        Ok(format!("target-{}", item.source_id))
    });
    assert_eq!(retried, vec!["2"]);
    assert!(execution.item_mappings.contains_key("1"));
    assert!(execution.item_mappings.contains_key("2"));
    assert!(!execution.item_mappings.contains_key("3"));
    assert_eq!(execution.failed, 1);
}
