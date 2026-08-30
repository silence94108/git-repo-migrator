use git_repo_migrator_platform_core::{CredentialRef, PlatformError};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub mod askpass;
pub mod prompt;

#[derive(Clone)]
pub struct CredentialStore {
    entries: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    backend: Backend,
}

#[derive(Clone, Copy)]
enum Backend {
    Memory,
    Windows,
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore {
    pub fn new() -> Self {
        if cfg!(windows) {
            return Self::windows();
        }
        Self::in_memory()
    }

    pub fn in_memory() -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            backend: Backend::Memory,
        }
    }
    pub fn windows() -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            backend: Backend::Windows,
        }
    }

    pub fn put(&self, service: &str, secret: &[u8]) -> Result<CredentialRef, PlatformError> {
        if service.trim().is_empty() || secret.is_empty() {
            return Err(PlatformError::validation("凭据服务名和 secret 不能为空"));
        }
        let id = format!("credential/windows/{}", stable_id(service.trim()));
        match self.backend {
            Backend::Memory => {
                self.entries
                    .lock()
                    .map_err(|_| PlatformError::validation("凭据存储不可用"))?
                    .insert(id.clone(), secret.to_vec());
            }
            Backend::Windows => {
                let value = std::str::from_utf8(secret)
                    .map_err(|_| PlatformError::validation("Windows 凭据必须是 UTF-8 文本"))?;
                keyring::Entry::new("git-repo-migrator", &id)
                    .map_err(keyring_error)?
                    .set_password(value)
                    .map_err(keyring_error)?;
            }
        }
        CredentialRef::new(id)
    }

    pub fn get(&self, reference: &CredentialRef) -> Result<SecretGuard, PlatformError> {
        let value = match self.backend {
            Backend::Memory => self
                .entries
                .lock()
                .map_err(|_| PlatformError::validation("凭据存储不可用"))?
                .get(reference.as_str())
                .cloned()
                .ok_or_else(not_found)?,
            Backend::Windows => keyring::Entry::new("git-repo-migrator", reference.as_str())
                .map_err(keyring_error)?
                .get_password()
                .map_err(keyring_error)?
                .into_bytes(),
        };
        Ok(SecretGuard { bytes: value })
    }

    pub fn delete(
        &self,
        reference: &CredentialRef,
        active_batch: bool,
    ) -> Result<(), PlatformError> {
        if active_batch {
            return Err(PlatformError {
                code: "credential.in_use".into(),
                category: git_repo_migrator_platform_core::PlatformErrorCategory::Conflict,
                retryable: false,
                safe_message: "运行中的批次仍在使用该凭据".into(),
                action: "等待批次完成或取消后再删除".into(),
                retry_after_seconds: None,
            });
        }
        match self.backend {
            Backend::Memory => {
                self.entries
                    .lock()
                    .map_err(|_| PlatformError::validation("凭据存储不可用"))?
                    .remove(reference.as_str());
            }
            Backend::Windows => {
                keyring::Entry::new("git-repo-migrator", reference.as_str())
                    .map_err(keyring_error)?
                    .delete_credential()
                    .map_err(keyring_error)?;
            }
        }
        Ok(())
    }
}

fn not_found() -> PlatformError {
    PlatformError {
        code: "credential.not_found".into(),
        category: git_repo_migrator_platform_core::PlatformErrorCategory::Auth,
        retryable: false,
        safe_message: "凭据不存在或已删除".into(),
        action: "重新保存凭据后重试".into(),
        retry_after_seconds: None,
    }
}
fn keyring_error(_: keyring::Error) -> PlatformError {
    PlatformError {
        code: "credential.store_error".into(),
        category: git_repo_migrator_platform_core::PlatformErrorCategory::Auth,
        retryable: false,
        safe_message: "Windows Credential Manager 操作失败".into(),
        action: "检查当前 Windows 用户的凭据服务后重试".into(),
        retry_after_seconds: None,
    }
}

pub(crate) fn stable_id(value: &str) -> String {
    let mut hash = 2166136261u32;
    for byte in value.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16777619);
    }
    format!("{hash:08x}")
}

pub struct SecretGuard {
    bytes: Vec<u8>,
}
impl SecretGuard {
    pub fn expose(&self) -> &[u8] {
        &self.bytes
    }
}
impl std::fmt::Debug for SecretGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretGuard([REDACTED])")
    }
}
impl Drop for SecretGuard {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secret_never_serializes_or_debugs() {
        let store = CredentialStore::in_memory();
        let reference = store.put("github", b"token-value").unwrap();
        let secret = store.get(&reference).unwrap();
        assert_eq!(secret.expose(), b"token-value");
        assert!(!format!("{secret:?}").contains("token-value"));
        assert!(!format!("{reference:?}").contains(reference.as_str()));
    }
    #[test]
    fn active_batch_blocks_delete() {
        let store = CredentialStore::in_memory();
        let reference = store.put("github", b"x").unwrap();
        assert!(store.delete(&reference, true).is_err());
        assert!(store.delete(&reference, false).is_ok());
    }
}
