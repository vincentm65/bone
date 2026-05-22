use std::path::PathBuf;

fn bone_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("bone");
    }

    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".bone"))
        .unwrap_or_else(|| {
            eprintln!("bone: warning: neither $HOME nor $XDG_CONFIG_HOME is set; using /tmp/.bone");
            PathBuf::from("/tmp/.bone")
        })
}

pub fn config_path() -> PathBuf {
    bone_dir().join("bone.yaml")
}

pub fn providers_path() -> PathBuf {
    bone_dir().join("providers.yaml")
}
