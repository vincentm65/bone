use std::path::PathBuf;

fn bone_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("bone-rust");
    }

    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".bone-rust"))
        .unwrap_or_else(|| {
            eprintln!("bone: warning: neither $HOME nor $XDG_CONFIG_HOME is set; using /tmp/.bone-rust");
            PathBuf::from("/tmp/.bone-rust")
        })
}

pub fn config_path() -> PathBuf {
    bone_dir().join("bone.yaml")
}

pub fn providers_path() -> PathBuf {
    bone_dir().join("providers.yaml")
}
