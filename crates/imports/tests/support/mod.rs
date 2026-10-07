use cannery_core::contracts::ContractValidator;
use cannery_imports::{BundleLimits, ImportContext};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
pub fn context() -> Result<ImportContext> {
    Ok(ImportContext {
        contracts: Arc::new(ContractValidator::new()?),

        rendering: cannery_research::science::RenderingContext {
            nesting_budget: 200,
        },
        json_budget: 200,
        statement_timeout: std::time::Duration::from_secs(1),
    })
}
pub const LIMITS: BundleLimits = BundleLimits {
    max_files: 20000,
    max_file_bytes: 8 * 1024 * 1024,
    max_report_bytes: 256 * 1024,
    max_total_bytes: 32 * 1024 * 1024,
    max_depth: 200,
    max_nodes: 200_000,
};
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
pub struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Result<Self> {
        let instant = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("cannery-import-{}-{instant}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn copy(from: &Path, to: &Path) -> Result {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
