//! Atomic file writes via a temp file plus rename.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::{fs, io::AsyncWriteExt};

fn temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_else(|_| std::process::id() as u128);
    path.with_extension(format!("bone-tmp-{}-{nanos}", std::process::id()))
}

/// Atomically write bytes using blocking filesystem operations.
pub fn write_atomic_sync(
    path: &Path,
    content: &[u8],
    permissions: Option<std::fs::Permissions>,
) -> Result<(), String> {
    use std::io::Write;

    let temp_path = temp_path(path);
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(permissions) = permissions.as_ref() {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(permissions.mode());
        }
        let mut file = options.open(&temp_path)?;
        file.write_all(content)?;
        file.flush()?;
        if let Some(permissions) = permissions {
            std::fs::set_permissions(&temp_path, permissions)?;
        }
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp_path, path)?;
        #[cfg(unix)]
        {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result.map_err(crate::util::errstr)
}

/// Write `content` to `path` atomically via a temp file.
/// If `permissions` is Some, those permissions are applied to the temp file before rename.
pub async fn write_atomic(
    path: &Path,
    content: &str,
    permissions: Option<std::fs::Permissions>,
) -> Result<(), String> {
    write_atomic_inner(path, content, permissions, None).await
}

/// Atomically write only if the destination still has `expected` byte content.
/// The comparison happens immediately before rename, minimizing but not
/// eliminating the POSIX check/rename race with uncooperative external writers.
pub async fn write_atomic_if_unchanged(
    path: &Path,
    content: &str,
    permissions: Option<std::fs::Permissions>,
    expected: &[u8],
) -> Result<(), String> {
    write_atomic_inner(path, content, permissions, Some(expected)).await
}

async fn write_atomic_inner(
    path: &Path,
    content: &str,
    permissions: Option<std::fs::Permissions>,
    expected: Option<&[u8]>,
) -> Result<(), String> {
    let temp_path = temp_path(path);

    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await
            .map_err(crate::util::errstr)?;
        f.write_all(content.as_bytes()).await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
        f.flush().await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
    }

    if let Some(perm) = permissions {
        fs::set_permissions(&temp_path, perm).await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
    }

    if let Some(expected) = expected {
        let current = fs::read(path).await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
        if current != expected {
            let _ = fs::remove_file(&temp_path).await;
            return Err(format!(
                "`{}` changed while the edit was being prepared; re-read it and retry",
                path.display()
            ));
        }
    }

    fs::rename(&temp_path, path).await.map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        e.to_string()
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "write_atomic_tests.rs"]
mod tests;
