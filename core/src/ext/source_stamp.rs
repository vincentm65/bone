//! Deterministic Lua source fingerprinting.
//!
//! Computes a SHA-256 digest over `config_dir/init.lua` and every regular
//! `*.lua` file found recursively under `config_dir/lua`.  Paths are sorted
//! lexicographically (relative to `config_dir`) so the digest is
//! independent of filesystem iteration order.  Additions, deletions and
//! renames all change the hash; non-Lua files are ignored.

use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

/// A deterministic SHA-256 digest over the user's Lua source tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceHash(pub [u8; 32]);

impl std::fmt::Display for SourceHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Errors that can occur while scanning the Lua source tree.
#[derive(Debug)]
pub struct SourceStampError {
    path: PathBuf,
    source: io::Error,
}

impl SourceStampError {
    fn new(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self {
            path: path.into(),
            source,
        }
    }
}

impl std::fmt::Display for SourceStampError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not scan {}: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for SourceStampError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Collect every regular `*.lua` file under `dir`, returning sorted paths
/// relative to `config_dir`.
fn collect_lua_files(
    config_dir: &Path,
    dir: &Path,
) -> Result<Vec<(PathBuf, PathBuf)>, SourceStampError> {
    let mut results = Vec::new();
    walk_lua(config_dir, dir, &mut results)?;
    results.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(results)
}

fn walk_lua(
    config_dir: &Path,
    dir: &Path,
    out: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), SourceStampError> {
    let entries = std::fs::read_dir(dir).map_err(|error| SourceStampError::new(dir, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| SourceStampError::new(dir, error))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| SourceStampError::new(&path, error))?;
        if file_type.is_dir() {
            walk_lua(config_dir, &path, out)?;
        } else if file_type.is_file()
            && path.extension().is_some_and(|extension| extension == "lua")
        {
            let relative = path
                .strip_prefix(config_dir)
                .expect("walked path must be below config directory")
                .to_path_buf();
            out.push((relative, path));
        }
    }
    Ok(())
}

/// Compute a deterministic SHA-256 fingerprint over the Lua source tree
/// rooted at `config_dir`.
///
/// - Hashes `config_dir/init.lua` (relative path `"init.lua"`) if it exists.
/// - Recursively hashes every regular `*.lua` file under
///   `config_dir/lua`, sorted by relative path.
/// - Each file contributes its relative path and contents to the digest.
/// - Returns the final hash or an error if a filesystem operation fails.
pub fn stamp(config_dir: &Path) -> Result<SourceHash, SourceStampError> {
    let mut files = Vec::new();
    let init_path = config_dir.join("init.lua");
    match std::fs::metadata(&init_path) {
        Ok(metadata) if metadata.is_file() => files.push((PathBuf::from("init.lua"), init_path)),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(SourceStampError::new(&init_path, error)),
    }

    let lua_dir = config_dir.join("lua");
    match std::fs::metadata(&lua_dir) {
        Ok(metadata) if metadata.is_dir() => {
            files.extend(collect_lua_files(config_dir, &lua_dir)?);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(SourceStampError::new(&lua_dir, error)),
    }
    files.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut hasher = Sha256::new();
    for (relative, absolute) in files {
        let contents =
            std::fs::read(&absolute).map_err(|error| SourceStampError::new(&absolute, error))?;
        let path = relative.as_os_str().as_encoded_bytes();
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path);
        hasher.update((contents.len() as u64).to_le_bytes());
        hasher.update(contents);
    }

    Ok(SourceHash(hasher.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, content) in files {
            let full = dir.path().join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full, *content).unwrap();
        }
        dir
    }

    #[test]
    fn init_lua_changes_affect_hash() {
        let dir = make_config_dir(&[("init.lua", "-- v1")]);
        let h1 = stamp(dir.path()).unwrap();

        // Change content of init.lua.
        let dir2 = make_config_dir(&[("init.lua", "-- v2")]);
        let h2 = stamp(dir2.path()).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn adding_lua_file_changes_hash() {
        let dir = make_config_dir(&[("init.lua", "hello")]);
        let h1 = stamp(dir.path()).unwrap();

        let dir2 = make_config_dir(&[("init.lua", "hello"), ("lua/mod.lua", "return 1")]);
        let h2 = stamp(dir2.path()).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn deleting_lua_file_changes_hash() {
        let dir = make_config_dir(&[("init.lua", "hello"), ("lua/mod.lua", "return 1")]);
        let h1 = stamp(dir.path()).unwrap();

        let dir2 = make_config_dir(&[("init.lua", "hello")]);
        let h2 = stamp(dir2.path()).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn renaming_lua_file_changes_hash() {
        let dir = make_config_dir(&[("lua/a.lua", "content_a")]);
        let h1 = stamp(dir.path()).unwrap();

        let dir2 = make_config_dir(&[("lua/b.lua", "content_a")]);
        let h2 = stamp(dir2.path()).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn non_lua_files_ignored() {
        let dir = make_config_dir(&[("init.lua", "hello"), ("lua/readme.txt", "ignore me")]);
        let h1 = stamp(dir.path()).unwrap();

        let dir2 = make_config_dir(&[("init.lua", "hello"), ("lua/readme.txt", "changed")]);
        let h2 = stamp(dir2.path()).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn recursive_lua_files_included() {
        let dir = make_config_dir(&[
            ("init.lua", "root"),
            ("lua/a.lua", "a"),
            ("lua/sub/b.lua", "b"),
        ]);
        let h = stamp(dir.path()).unwrap();

        // Changing a nested file should change the hash.
        let dir2 = make_config_dir(&[
            ("init.lua", "root"),
            ("lua/a.lua", "a"),
            ("lua/sub/b.lua", "changed"),
        ]);
        let h2 = stamp(dir2.path()).unwrap();
        assert_ne!(h, h2);
    }

    #[test]
    fn hash_is_deterministic_across_calls() {
        let dir = make_config_dir(&[
            ("init.lua", "init content"),
            ("lua/lib/utils.lua", "return {}"),
            ("lua/plugins/theme.lua", "return { dark = true }"),
        ]);
        let h1 = stamp(dir.path()).unwrap();
        let h2 = stamp(dir.path()).unwrap();
        let h3 = stamp(dir.path()).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h2, h3);
    }

    #[test]
    fn display_produces_64_hex_chars() {
        let dir = make_config_dir(&[("init.lua", "test")]);
        let h = stamp(dir.path()).unwrap();
        let s = h.to_string();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn same_size_edit_changes_hash() {
        let dir = make_config_dir(&[("init.lua", "alpha")]);
        let before = stamp(dir.path()).unwrap();
        std::fs::write(dir.path().join("init.lua"), "bravo").unwrap();
        assert_ne!(before, stamp(dir.path()).unwrap());
    }

    #[test]
    fn path_and_content_framing_prevents_concatenation_collisions() {
        let one_file = make_config_dir(&[("lua/a.lua", "Xlua/b.luaY")]);
        let two_files = make_config_dir(&[("lua/a.lua", "X"), ("lua/b.lua", "Y")]);
        assert_ne!(
            stamp(one_file.path()).unwrap(),
            stamp(two_files.path()).unwrap()
        );
    }

    #[test]
    fn no_init_lua_but_lua_files_still_hashed() {
        let dir = make_config_dir(&[("lua/mod.lua", "only lua dir")]);
        let h = stamp(dir.path()).unwrap();
        // Should not error even without init.lua.
        let dir2 = make_config_dir(&[]);
        let h2 = stamp(dir2.path()).unwrap();
        assert_ne!(h, h2);
    }
}
