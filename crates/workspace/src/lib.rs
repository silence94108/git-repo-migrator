use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub enum WorkspaceError {
    OutsideRoot,
    AlreadyLocked,
    InsufficientSpace { required: u64, available: u64 },
    Io(io::Error),
}
impl From<io::Error> for WorkspaceError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutsideRoot => write!(f, "path is outside workspace root"),
            Self::AlreadyLocked => write!(f, "workspace is already locked"),
            Self::InsufficientSpace {
                required,
                available,
            } => write!(
                f,
                "insufficient disk space: required {required}, available {available}"
            ),
            Self::Io(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for WorkspaceError {}

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}
impl Workspace {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, WorkspaceError> {
        let root = fs::canonicalize(root.into())?;
        Ok(Self { root })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn child(&self, name: &str) -> Result<PathBuf, WorkspaceError> {
        if name.is_empty() || name.contains('\0') {
            return Err(WorkspaceError::OutsideRoot);
        }
        let p = self.root.join(name);
        let norm = normalize(&p);
        if !norm.starts_with(&self.root) {
            return Err(WorkspaceError::OutsideRoot);
        }
        fs::create_dir_all(&norm)?;
        Ok(norm)
    }
    pub fn preflight_space(&self, required: u64) -> Result<(), WorkspaceError> {
        let available = fs2::available_space(&self.root)?;
        if available < required {
            Err(WorkspaceError::InsufficientSpace {
                required,
                available,
            })
        } else {
            Ok(())
        }
    }
    pub fn lock(&self) -> Result<WorkspaceLock, WorkspaceError> {
        let p = self.root.join(".workspace.lock");
        match fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => Ok(WorkspaceLock { path: p }),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                Err(WorkspaceError::AlreadyLocked)
            }
            Err(e) => Err(e.into()),
        }
    }
    pub fn cleanup_temp(&self, path: &Path) -> Result<(), WorkspaceError> {
        let norm = normalize(path);
        if !norm.starts_with(&self.root) || norm == self.root {
            return Err(WorkspaceError::OutsideRoot);
        }
        if norm.exists() {
            fs::remove_dir_all(norm)?;
        }
        Ok(())
    }
    pub fn temp_dir(&self, id: &str) -> Result<PathBuf, WorkspaceError> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        self.child(&format!(".tmp-{id}-{stamp}"))
    }
}
#[derive(Debug)]
pub struct WorkspaceLock {
    path: PathBuf,
}
impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
fn normalize(p: &Path) -> PathBuf {
    if p.exists() {
        fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    } else {
        let mut cur = p.to_path_buf();
        let mut tail = Vec::new();
        while !cur.exists() {
            if let Some(n) = cur.file_name() {
                tail.push(n.to_os_string());
            } else {
                break;
            }
            cur.pop();
        }
        let mut out = fs::canonicalize(&cur).unwrap_or(cur);
        for n in tail.iter().rev() {
            out.push(n);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_escape() {
        let d = tempfile::tempdir().unwrap();
        let w = Workspace::new(d.path()).unwrap();
        assert!(matches!(
            w.child("..\\escape"),
            Err(WorkspaceError::OutsideRoot)
        ));
    }
    #[test]
    fn lock_is_exclusive() {
        let d = tempfile::tempdir().unwrap();
        let w = Workspace::new(d.path()).unwrap();
        let l = w.lock().unwrap();
        assert!(matches!(w.lock(), Err(WorkspaceError::AlreadyLocked)));
        drop(l);
        assert!(w.lock().is_ok());
    }
    #[test]
    fn cleanup_only_workspace() {
        let d = tempfile::tempdir().unwrap();
        let w = Workspace::new(d.path()).unwrap();
        let t = w.temp_dir("x").unwrap();
        assert!(t.exists());
        w.cleanup_temp(&t).unwrap();
        assert!(!t.exists());
        assert!(matches!(
            w.cleanup_temp(Path::new(".")),
            Err(WorkspaceError::OutsideRoot)
        ));
    }
}
