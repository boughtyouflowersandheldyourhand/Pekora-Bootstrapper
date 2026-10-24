use std::path::PathBuf;

use anyhow::Result;
use directories::BaseDirs;

pub const BaseUrl: &str = "http://pekora.pro";
pub const AppName: &str = "Pekora";
pub const LauncherExe: &str = "bootstrapper.exe";
pub const DefaultClient: &str = "2021";
pub const LauncherVersion: &str = "15";
pub const UrlSchemesPlayer: &[&str] = &["pekora-player"];
pub const UrlSchemesStudio: &[&str] = &["pekora-studio"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Player,
    Studio,
}

impl Kind {
    pub fn FromScheme(Scheme: &str) -> Option<Self> {
        let Lower = Scheme.to_ascii_lowercase();
        if UrlSchemesPlayer.contains(&Lower.as_str()) {
            Some(Kind::Player)
        } else if UrlSchemesStudio.contains(&Lower.as_str()) {
            Some(Kind::Studio)
        } else {
            None
        }
    }

    pub fn Label(self) -> &'static str {
        match self {
            Kind::Player => "Pekora",
            Kind::Studio => "Pekora Studio",
        }
    }

    pub fn ExeName(self) -> &'static str {
        match self {
            Kind::Player => "RobloxPlayerBeta.exe",
            Kind::Studio => "RobloxStudioBeta.exe",
        }
    }
}

pub fn InstallRoot() -> Result<PathBuf> {
    let Dirs = BaseDirs::new().ok_or_else(|| anyhow::anyhow!("no LOCALAPPDATA"))?;
    Ok(Dirs.data_local_dir().join(AppName))
}

pub fn LauncherExePath() -> Result<PathBuf> {
    Ok(InstallRoot()?.join(LauncherExe))
}

pub fn KindRoot(Kind: Kind) -> Result<PathBuf> {
    Ok(InstallRoot()?.join(match Kind {
        Kind::Player => "clients",
        Kind::Studio => "Studio",
    }))
}

pub fn InstallDir(Kind: Kind, Client: &str) -> Result<PathBuf> {
    Ok(KindRoot(Kind)?.join(Client))
}

pub fn TargetExe(Kind: Kind, Client: &str) -> Result<PathBuf> {
    Ok(InstallDir(Kind, Client)?.join(Kind.ExeName()))
}

pub fn StateDir() -> Result<PathBuf> {
    Ok(InstallRoot()?.join("state"))
}

pub fn CachedVersionFile(Kind: Kind, Client: &str) -> Result<PathBuf> {
    let Prefix = match Kind {
        Kind::Player => "LATEST",
        Kind::Studio => "LATEST-studio",
    };
    Ok(StateDir()?.join(format!("{Prefix}-{Client}")))
}

pub fn InstalledVersionFile(Kind: Kind, Client: &str) -> Result<PathBuf> {
    Ok(InstallDir(Kind, Client)?.join(".pekora-version"))
}

pub fn VersionUrl(Kind: Kind, Client: &str) -> String {
    match Kind {
        Kind::Player => format!("{BaseUrl}/setup/{Client}/version.txt"),
        Kind::Studio => format!("{BaseUrl}/setup/studio/{Client}/version.txt"),
    }
}

pub fn BundleUrl(Kind: Kind, Client: &str, Version: &str) -> String {
    match Kind {
        Kind::Player => format!("{BaseUrl}/setup/{Client}/{Version}-client.zip"),
        Kind::Studio => format!("{BaseUrl}/setup/studio/{Client}/{Version}-studio.zip"),
    }
}

pub fn LauncherUrl() -> String {
    format!("{BaseUrl}/setup/launcher.exe")
}

pub fn LauncherVersionUrl() -> String {
    format!("{BaseUrl}/setup/launcher/version.txt")
}

pub fn GlobalVersionUrl() -> String {
    format!("{BaseUrl}/version")
}

pub fn GlobalVersionFile() -> Result<PathBuf> {
    Ok(StateDir()?.join("GLOBAL-VERSION"))
}
