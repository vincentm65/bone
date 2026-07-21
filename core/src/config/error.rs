use std::path::{Path, PathBuf};

/// Actionable configuration failure with both document and setting context.
#[derive(Debug)]
pub struct ConfigError {
    operation: &'static str,
    path: PathBuf,
    setting: Option<String>,
    detail: String,
}

impl ConfigError {
    pub fn new(
        operation: &'static str,
        path: impl Into<PathBuf>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            operation,
            path: path.into(),
            setting: None,
            detail: detail.into(),
        }
    }

    pub fn at_setting(mut self, setting: impl Into<String>) -> Self {
        self.setting = Some(setting.into());
        self
    }

    pub fn load(path: &Path, detail: impl Into<String>) -> Self {
        Self::new("load", path, detail)
    }

    pub fn persist(path: &Path, detail: impl Into<String>) -> Self {
        Self::new("persist", path, detail)
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not {} {}", self.operation, self.path.display())?;
        if let Some(setting) = &self.setting {
            write!(f, " at {setting}")?;
        }
        write!(f, ": {}", self.detail)
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_document_operation_and_setting() {
        let error = ConfigError::persist(Path::new("/tmp/config.yaml"), "permission denied")
            .at_setting("general.approval");
        assert_eq!(
            error.to_string(),
            "could not persist /tmp/config.yaml at general.approval: permission denied"
        );
    }
}
