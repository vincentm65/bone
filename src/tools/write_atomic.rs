use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::{fs, io::AsyncWriteExt};

/// Write `content` to `path` atomically via a temp file.
/// If `permissions` is Some, those permissions are applied to the temp file before rename.
pub async fn write_atomic(
    path: &Path,
    content: &str,
    permissions: Option<std::fs::Permissions>,
) -> Result<(), String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_else(|_| std::process::id() as u128);
    let pid = std::process::id();
    let temp_path = path.with_extension(format!("bone-tmp-{pid}-{nanos}"));

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

    fs::rename(&temp_path, path).await.map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        e.to_string()
    })?;
    Ok(())
}
