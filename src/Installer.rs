use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::{Client, Response};

use crate::Config::{self, Kind};
use crate::Ui::{Phase, SetPhase, SetProgress, SharedState};

const AppSettingsXml: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n",
    "<Settings>\r\n",
    "    <ContentFolder>content</ContentFolder>\r\n",
    "    <BaseUrl>http://pekora.pro</BaseUrl>\r\n",
    "</Settings>\r\n",
);

const UserAgent: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) PekoraBootstrapper/0.1 (+https://www.pekora.pro)";

fn HttpClient(Timeout: Duration) -> Result<Client> {
    Ok(Client::builder()
        .user_agent(UserAgent)
        .timeout(Timeout)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?)
}

fn SaveBody(mut Body: Response, Destination: &Path, mut OnProgress: impl FnMut(u64, Option<u64>)) -> Result<()> {
    let Total = Body.content_length();
    let mut File = fs::File::create(Destination).with_context(|| format!("creating {}", Destination.display()))?;
    let mut Buffer = [0u8; 64 * 1024];
    let mut Downloaded = 0u64;
    loop {
        let Count = Body.read(&mut Buffer)?;
        if Count == 0 {
            break;
        }
        File.write_all(&Buffer[..Count])?;
        Downloaded += Count as u64;
        OnProgress(Downloaded, Total);
    }
    File.flush()?;
    Ok(())
}

fn WriteCreatingParent(Path: &Path, Contents: &str) {
    if let Some(Parent) = Path.parent() {
        let _ = fs::create_dir_all(Parent);
    }
    let _ = fs::write(Path, Contents);
}

fn ReadTrimmed(Path: Result<PathBuf>) -> Option<String> {
    let Value = fs::read_to_string(Path.ok()?).ok()?.trim().to_string();
    (!Value.is_empty()).then_some(Value)
}

pub fn ResolveTargetVersion(SelectedKind: Kind, ClientName: &str) -> Result<String> {
    match FetchRemoteVersion(SelectedKind, ClientName) {
        Ok(Version) => {
            if let Ok(File) = Config::CachedVersionFile(SelectedKind, ClientName) {
                WriteCreatingParent(&File, &Version);
            }
            Ok(Version)
        }
        Err(NetError) => {
            log::warn!("could not reach setup endpoint for {} {ClientName}: {NetError:#}", SelectedKind.Label());
            ReadTrimmed(Config::InstalledVersionFile(SelectedKind, ClientName))
                .or_else(|| ReadTrimmed(Config::CachedVersionFile(SelectedKind, ClientName)))
                .ok_or_else(|| NetError.context("no cached version available either"))
        }
    }
}

fn FetchRemoteVersion(SelectedKind: Kind, ClientName: &str) -> Result<String> {
    let Version = HttpClient(Duration::from_secs(15))?
        .get(Config::VersionUrl(SelectedKind, ClientName))
        .send()?
        .error_for_status()?
        .text()?
        .trim()
        .to_string();
    if Version.is_empty() {
        return Err(anyhow!("server returned an empty version string"));
    }
    Ok(Version)
}

pub fn IsInstalled(SelectedKind: Kind, ClientName: &str, Version: &str) -> bool {
    Config::TargetExe(SelectedKind, ClientName).map_or(false, |Exe| Exe.is_file())
        && ReadTrimmed(Config::InstalledVersionFile(SelectedKind, ClientName)).as_deref() == Some(Version)
}

pub fn InstallClient(SelectedKind: Kind, ClientName: &str, Version: &str, State: &SharedState) -> Result<()> {
    let Label = SelectedKind.Label();
    let Target = Config::InstallDir(SelectedKind, ClientName)?;
    fs::create_dir_all(&Target).with_context(|| format!("creating {}", Target.display()))?;

    let ZipPath = Target.with_extension("zip.partial");
    let _ = fs::remove_file(&ZipPath);
    DownloadWithProgress(&Config::BundleUrl(SelectedKind, ClientName, Version), &ZipPath, State, (0.0, 0.7))
        .with_context(|| format!("downloading {Label} bundle"))?;

    SetPhase(State, Phase::Configuring, &format!("Upgrading {Label}..."), None);
    ExtractZipWithProgress(&ZipPath, &Target, State, (0.7, 0.95))
        .with_context(|| format!("extracting {Label} bundle"))?;
    let _ = fs::remove_file(&ZipPath);

    let AppSettings = Target.join("AppSettings.xml");
    if SelectedKind == Kind::Player && !AppSettings.exists() {
        fs::write(&AppSettings, AppSettingsXml).context("writing AppSettings.xml")?;
    }

    if let Ok(File) = Config::CachedVersionFile(SelectedKind, ClientName) {
        WriteCreatingParent(&File, Version);
    }
    let Marker = Config::InstalledVersionFile(SelectedKind, ClientName)?;
    fs::write(&Marker, Version).with_context(|| format!("writing {}", Marker.display()))?;

    SetProgress(State, 0.97);
    Ok(())
}

fn PrepareLauncherPath() -> Result<PathBuf> {
    let Destination = Config::LauncherExePath()?;
    if let Some(Parent) = Destination.parent() {
        fs::create_dir_all(Parent)?;
    }
    Ok(Destination)
}

pub fn InstallSelf() -> Result<PathBuf> {
    let Destination = PrepareLauncherPath()?;
    let Source = std::env::current_exe().context("locating current executable")?;
    if SameFile(&Source, &Destination) {
        return Ok(Destination);
    }
    if let Err(Error) = fs::copy(&Source, &Destination) {
        if !Destination.is_file() {
            return Err(anyhow!("copying launcher to {}: {Error}", Destination.display()));
        }
        log::warn!("could not refresh launcher copy ({Error}); keeping existing one");
    }
    Ok(Destination)
}

pub fn InstallLauncherFromServer() -> Result<PathBuf> {
    let Destination = PrepareLauncherPath()?;
    let Url = Config::LauncherUrl();
    let Body = match HttpClient(Duration::from_secs(60 * 5))?.get(&Url).send().and_then(|Body| Body.error_for_status()) {
        Ok(Body) => Body,
        Err(Error) => {
            log::warn!("could not download launcher from {Url}: {Error:#}");
            if Destination.is_file() {
                return Ok(Destination);
            }
            return Err(anyhow!("no launcher cached locally and download from {Url} failed: {Error}"));
        }
    };
    let Partial = Destination.with_extension("exe.partial");
    let _ = fs::remove_file(&Partial);
    SaveBody(Body, &Partial, |_, _| {})?;
    if let Err(Error) = fs::rename(&Partial, &Destination) {
        if !Destination.is_file() {
            return Err(anyhow!("renaming downloaded launcher into place at {}: {Error}", Destination.display()));
        }
        log::warn!("could not replace launcher at {} ({Error}); keeping existing copy", Destination.display());
        let _ = fs::remove_file(&Partial);
    }
    Ok(Destination)
}

fn SameFile(First: &Path, Second: &Path) -> bool {
    matches!((fs::canonicalize(First), fs::canonicalize(Second)), (Ok(First), Ok(Second)) if First == Second)
}

fn Lerp(From: f32, To: f32, Amount: f32) -> f32 {
    From + (To - From) * Amount.clamp(0.0, 1.0)
}

fn DownloadWithProgress(Url: &str, Destination: &Path, State: &SharedState, Range: (f32, f32)) -> Result<()> {
    let Body = HttpClient(Duration::from_secs(60 * 10))?.get(Url).send()?.error_for_status()?;
    SaveBody(Body, Destination, |Downloaded, Total| {
        if let Some(Total) = Total.filter(|Total| *Total > 0) {
            SetProgress(State, Lerp(Range.0, Range.1, Downloaded as f32 / Total as f32));
        }
    })?;
    SetProgress(State, Range.1);
    Ok(())
}

fn ExtractZipWithProgress(ZipPath: &Path, Target: &Path, State: &SharedState, Range: (f32, f32)) -> Result<()> {
    let mut Archive = zip::ZipArchive::new(fs::File::open(ZipPath)?)?;
    let Total = Archive.len() as f32;
    for Index in 0..Archive.len() {
        let mut Entry = Archive.by_index(Index)?;
        let Some(Relative) = Entry.enclosed_name() else { continue };
        let Out = Target.join(Relative);
        if Entry.is_dir() {
            fs::create_dir_all(&Out)?;
        } else {
            if let Some(Parent) = Out.parent() {
                fs::create_dir_all(Parent)?;
            }
            let mut Writer = fs::File::create(&Out).with_context(|| format!("creating {}", Out.display()))?;
            io::copy(&mut Entry, &mut Writer)?;
        }
        SetProgress(State, Lerp(Range.0, Range.1, (Index + 1) as f32 / Total));
    }
    Ok(())
}

#[cfg(windows)]
pub fn RegisterUrlSchemes(Launcher: &Path) -> Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let CurrentUser = RegKey::predef(HKEY_CURRENT_USER);
    let LauncherPath = Launcher.to_string_lossy();
    let Registered = [Config::UrlSchemesPlayer[0], Config::UrlSchemesStudio[0]];
    for Scheme in Registered {
        let Base = format!("Software\\Classes\\{Scheme}");
        let (Key, _) = CurrentUser.create_subkey(&Base)?;
        Key.set_value("", &format!("URL:{} Protocol", Config::AppName))?;
        Key.set_value("URL Protocol", &"")?;
        CurrentUser.create_subkey(format!("{Base}\\DefaultIcon"))?.0.set_value("", &format!("\"{LauncherPath}\",0"))?;
        CurrentUser
            .create_subkey(format!("{Base}\\shell\\open\\command"))?
            .0
            .set_value("", &format!("\"{LauncherPath}\" \"%1\""))?;
    }
    for Scheme in Config::UrlSchemesPlayer.iter().chain(Config::UrlSchemesStudio) {
        if !Registered.contains(Scheme) {
            let _ = CurrentUser.delete_subkey_all(format!("Software\\Classes\\{Scheme}"));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn RegisterUrlSchemes(_Launcher: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn RegisterFileAssociations(Launcher: &Path) -> Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let CurrentUser = RegKey::predef(HKEY_CURRENT_USER);
    let LauncherPath = Launcher.to_string_lossy();
    let ProgId = "Pekora.StudioFile";
    let (Class, _) = CurrentUser.create_subkey(format!("Software\\Classes\\{ProgId}"))?;
    Class.set_value("", &"Pekora Studio File")?;
    Class.set_value("FriendlyTypeName", &"Pekora Studio File")?;
    CurrentUser
        .create_subkey(format!("Software\\Classes\\{ProgId}\\DefaultIcon"))?
        .0
        .set_value("", &format!("\"{LauncherPath}\",0"))?;
    CurrentUser
        .create_subkey(format!("Software\\Classes\\{ProgId}\\shell\\open\\command"))?
        .0
        .set_value("", &format!("\"{LauncherPath}\" \"%1\""))?;
    Ok(())
}

#[cfg(not(windows))]
pub fn RegisterFileAssociations(_Launcher: &Path) -> Result<()> {
    Ok(())
}

pub fn SelfUpdateIfNeeded(Args: &[String]) -> bool {
    if std::env::var("PEKORA_NO_SELFUPDATE").is_ok() {
        return false;
    }
    let Some(Http) = HttpClient(Duration::from_secs(15)).ok() else { return false };
    let Live = match Http.get(Config::LauncherVersionUrl()).send().and_then(|Body| Body.error_for_status()) {
        Ok(Body) => Body.text().unwrap_or_default().trim().to_string(),
        Err(_) => return false,
    };
    if Live.is_empty() || Live == Config::LauncherVersion {
        return false;
    }
    log::warn!("launcher {} -> {}; self-updating", Config::LauncherVersion, Live);
    let (Ok(Installed), Ok(Root)) = (Config::LauncherExePath(), Config::InstallRoot()) else { return false };
    let _ = fs::create_dir_all(&Root);
    let Staged = Root.join("bootstrapper.update.exe");
    let _ = fs::remove_file(&Staged);
    let Downloaded = Http
        .get(Config::LauncherUrl())
        .send()
        .and_then(|Body| Body.error_for_status())
        .map_err(anyhow::Error::from)
        .and_then(|Body| SaveBody(Body, &Staged, |_, _| {}));
    if Downloaded.is_err() {
        let _ = fs::remove_file(&Staged);
        return false;
    }
    let Retired = RetiredPath(&Root);
    if Installed.exists() {
        let _ = fs::rename(&Installed, Retired);
    }
    if fs::rename(&Staged, &Installed).is_err() && fs::copy(&Staged, &Installed).is_err() {
        return false;
    }
    let mut Cmd = std::process::Command::new(&Installed);
    Cmd.args(Args).env("PEKORA_NO_SELFUPDATE", "1");
    if let Some(Parent) = Installed.parent() {
        Cmd.current_dir(Parent);
    }
    Cmd.spawn().is_ok()
}

pub fn EnforceGlobalVersion(State: &SharedState) -> Result<()> {
    let Live = HttpClient(Duration::from_secs(15))?
        .get(Config::GlobalVersionUrl())
        .send()?
        .error_for_status()?
        .text()?
        .trim()
        .to_string();
    if Live.is_empty() {
        return Err(anyhow!("server returned an empty global version"));
    }

    let CachePath = Config::GlobalVersionFile()?;
    let Cached = fs::read_to_string(&CachePath).map(|Value| Value.trim().to_string()).unwrap_or_default();
    if Cached == Live {
        return Ok(());
    }
    if !Cached.is_empty() {
        log::warn!("global version changed ({} -> {}); wiping all installs", Cached, Live);
        SetPhase(State, Phase::Configuring, "Updating Pekora to a new version...", None);
        WipeAllInstalls()?;
    }
    if let Some(Parent) = CachePath.parent() {
        let _ = fs::create_dir_all(Parent);
    }
    fs::write(&CachePath, &Live).with_context(|| format!("writing {}", CachePath.display()))?;
    Ok(())
}

fn WipeAllInstalls() -> Result<()> {
    for SelectedKind in [Kind::Player, Kind::Studio] {
        let Root = Config::KindRoot(SelectedKind)?;
        if Root.exists() {
            log::info!("removing {}", Root.display());
            if let Err(Error) = fs::remove_dir_all(&Root) {
                log::warn!("could not fully remove {}: {Error}", Root.display());
            }
        }
    }
    if let Ok(StateDir) = Config::StateDir() {
        if StateDir.is_dir() {
            for Entry in fs::read_dir(&StateDir)?.flatten() {
                if Entry.file_name().to_string_lossy().starts_with("LATEST-") {
                    let _ = fs::remove_file(Entry.path());
                }
            }
        }
    }
    Ok(())
}

fn RetiredPath(Root: &Path) -> PathBuf {
    if let Ok(Entries) = fs::read_dir(Root) {
        for Entry in Entries.flatten() {
            let Name = Entry.file_name().to_string_lossy().to_ascii_lowercase();
            if Name.starts_with("bootstrapper.old") && Name.ends_with(".exe") {
                let _ = fs::remove_file(Entry.path());
            }
        }
    }
    let Old = Root.join("bootstrapper.old.exe");
    if Old.exists() {
        Root.join(format!("bootstrapper.old-{}.exe", std::process::id()))
    } else {
        Old
    }
}
