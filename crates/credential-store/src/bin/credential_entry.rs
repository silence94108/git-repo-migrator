//! Console companion for credential entry.
//!
//! A separate console-subsystem binary, deliberately: the GUI process never
//! reads a token, so no secret can reach the webview, a Tauri command payload
//! or a crash report from the GUI. It writes straight to Windows Credential
//! Manager and prints only the resulting reference.
//!
//! Usage: `git-repo-migrator-credential <name>`

use std::process::ExitCode;

use git_repo_migrator_credential_store::prompt::{store_interactively, validate_name};
use git_repo_migrator_credential_store::CredentialStore;
use git_repo_migrator_platform_core::PlatformError;

fn read_hidden(prompt: &str) -> Result<String, PlatformError> {
    rpassword::prompt_password(prompt)
        .map(|value| value.trim_end_matches(['\r', '\n']).to_owned())
        .map_err(|_| PlatformError::validation("无法从控制台读取输入"))
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(name) = args.next() else {
        eprintln!("用法: git-repo-migrator-credential <凭据名称>");
        eprintln!("例如: git-repo-migrator-credential source");
        return ExitCode::from(2);
    };
    if args.next().is_some() {
        eprintln!("错误: 只接受一个凭据名称参数");
        return ExitCode::from(2);
    }
    if let Err(error) = validate_name(&name) {
        eprintln!("错误: {}", error.safe_message);
        eprintln!("建议: {}", error.action);
        return ExitCode::from(2);
    }

    println!("Git Repo Migrator - 本机凭据录入");
    println!("令牌只写入当前 Windows 用户的凭据管理器，不会进入界面、日志或报告。");
    println!("凭据名称: {name}");

    match store_interactively(&CredentialStore::new(), &name, read_hidden) {
        Ok(stored) => {
            println!();
            println!("已保存。请在连接页的「凭据引用」中填写：");
            println!("{}", stored.reference.as_str());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!();
            eprintln!("错误: {}", error.safe_message);
            eprintln!("建议: {}", error.action);
            ExitCode::FAILURE
        }
    }
}
