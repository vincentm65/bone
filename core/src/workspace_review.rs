//! Bounded, read-only Git inspection on the daemon host.
use bone_protocol::{HostErrorCode, HostResponse};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const LIMIT: usize = 256 * 1024;

fn git(root: &Path, args: &[&str]) -> Result<(String, bool), String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.fsmonitor=false", "-c", "core.quotePath=true"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start Git: {e}"))?;
    fn capture(
        pipe: impl Read + Send + 'static,
    ) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
            Ok(bytes)
        })
    }
    let out = capture(child.stdout.take().unwrap());
    let err = capture(child.stderr.take().unwrap());
    let start = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if start.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("Git review timed out after 5 seconds".to_string());
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("Git review failed: {e}"));
            }
        }
    };
    let bytes = out
        .join()
        .map_err(|_| "Git output reader failed")?
        .map_err(|e| e.to_string())?;
    let errors = err
        .join()
        .map_err(|_| "Git error reader failed")?
        .map_err(|e| e.to_string())?;
    let status = result?;
    let truncated = bytes.len() > LIMIT;
    // A full pipe is deliberately closed at the bound; Git may exit via SIGPIPE.
    if !status.success() && !truncated {
        return Err(String::from_utf8_lossy(&errors).trim().to_string());
    }
    Ok((
        String::from_utf8_lossy(&bytes[..bytes.len().min(LIMIT)]).into_owned(),
        truncated,
    ))
}

pub fn review(root: &Path) -> HostResponse {
    let result = (|| {
        let (status, a) = git(root, &["status", "--short", "--untracked-files=normal"])?;
        let (staged, b) = git(
            root,
            &[
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--no-renames",
                "--",
            ],
        )?;
        let (unstaged, c) = git(
            root,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--no-renames",
                "--",
            ],
        )?;
        Ok::<_, String>(HostResponse::WorkspaceReview {
            workspace: root.display().to_string(),
            status,
            staged,
            unstaged,
            truncated: a || b || c,
        })
    })();
    result.unwrap_or_else(|message| HostResponse::Error {
        code: HostErrorCode::Unavailable,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_reports_staged_unstaged_and_untracked_without_mutation() {
        let root = std::env::temp_dir().join(format!("bone-review-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(root.join("tracked.txt"), "staged\n").unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["add", "tracked.txt"])
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(root.join("tracked.txt"), "unstaged\n").unwrap();
        std::fs::write(root.join("new.txt"), "untracked\n").unwrap();
        let before = std::fs::read(root.join(".git/index")).unwrap();
        let HostResponse::WorkspaceReview {
            status,
            staged,
            unstaged,
            truncated,
            ..
        } = review(&root)
        else {
            panic!("review failed")
        };
        assert!(status.contains("new.txt"));
        assert!(staged.contains("+staged"));
        assert!(unstaged.contains("+unstaged"));
        assert!(!truncated);
        assert_eq!(before, std::fs::read(root.join(".git/index")).unwrap());
        std::fs::write(root.join("tracked.txt"), "large line\n".repeat(LIMIT)).unwrap();
        let HostResponse::WorkspaceReview {
            unstaged,
            truncated,
            ..
        } = review(&root)
        else {
            panic!("large review failed");
        };
        assert!(truncated);
        assert!(unstaged.len() <= LIMIT);
        assert_eq!(before, std::fs::read(root.join(".git/index")).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
}
