use serde::{Deserialize, Serialize};

use crate::Config;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct Prefs {
    pub Input: Option<String>,
    pub Output: Option<String>,
}

fn FilePath() -> Option<std::path::PathBuf> {
    Config::StateDir().ok().map(|Dir| Dir.join("voice.json"))
}

pub fn Load() -> Prefs {
    FilePath()
        .and_then(|File| std::fs::read(File).ok())
        .and_then(|Bytes| serde_json::from_slice(&Bytes).ok())
        .unwrap_or_default()
}

pub fn Save(Current: &Prefs) {
    let Some(File) = FilePath() else { return };
    if let Some(Dir) = File.parent() {
        let _ = std::fs::create_dir_all(Dir);
    }
    if let Ok(Bytes) = serde_json::to_vec_pretty(Current) {
        let _ = std::fs::write(File, Bytes);
    }
}
