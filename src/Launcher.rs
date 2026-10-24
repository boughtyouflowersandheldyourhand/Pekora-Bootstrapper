use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use percent_encoding::percent_decode_str;
use windows_sys::Win32::Foundation::{CloseHandle, BOOL, FILETIME, HWND, INVALID_HANDLE_VALUE, LPARAM};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsHungAppWindow, IsWindowVisible,
};

use crate::Config::{self, Kind};
use crate::Installer;
use crate::Ui::{Phase, SetPhase, SetProgress, SharedState};

const ImagePathCapacity: usize = 1024;
const StuckWithoutWindowSecs: u64 = 60;

pub struct LaunchInput {
    pub PlayUrl: Option<String>,
    pub LocalFile: Option<String>,
    pub Client: String,
    pub Kind: Kind,
    pub InstallOnly: bool,
}

#[derive(Debug, Default)]
pub struct PlayParams {
    pub LaunchMode: String,
    pub GameInfo: Option<String>,
    pub PlaceLauncherUrl: Option<String>,
    pub LaunchTime: Option<String>,
    pub ClientVersion: Option<String>,
    pub ClientYear: Option<String>,
    pub LaunchExp: Option<String>,
    pub BrowserTrackerId: Option<String>,
    pub Task: Option<String>,
    pub PlaceId: Option<String>,
    pub UniverseId: Option<String>,
    pub UserId: Option<String>,
    pub Script: Option<String>,
}

pub fn ParseArgv(Args: &[String]) -> LaunchInput {
    let mut PlayUrl = None;
    let mut LocalFile = None;
    let mut Client = None;
    let mut SelectedKind = None;
    let mut InstallOnly = false;
    let mut Iter = Args.iter();
    while let Some(Arg) = Iter.next() {
        match Arg.as_str() {
            "--play-url" => PlayUrl = Iter.next().cloned().or(PlayUrl),
            "--client" => Client = Iter.next().cloned().or(Client),
            "--install" | "--installer" => InstallOnly = true,
            "--studio" => SelectedKind = Some(Kind::Studio),
            "--player" => SelectedKind = Some(Kind::Player),
            "--open" => {
                if let Some(Value) = Iter.next() {
                    LocalFile = Some(Value.clone());
                    SelectedKind = SelectedKind.or(Some(Kind::Studio));
                }
            }
            Other => {
                if let Some(SchemeKind) = Other.split_once(':').and_then(|(Scheme, _)| Kind::FromScheme(Scheme)) {
                    PlayUrl = Some(Other.to_string());
                    SelectedKind = SelectedKind.or(Some(SchemeKind));
                }
            }
        }
    }
    if Client.is_none() {
        if let Some(Parsed) = PlayUrl.as_deref().and_then(|Url| ParsePlayUri(Url).ok()) {
            Client = Parsed
                .ClientYear
                .or_else(|| LegacyClientYear(Parsed.ClientVersion.as_deref()).map(str::to_string));
        }
    }
    LaunchInput {
        PlayUrl,
        LocalFile,
        Client: Client.unwrap_or_else(|| Config::DefaultClient.to_string()),
        Kind: SelectedKind.unwrap_or(Kind::Player),
        InstallOnly,
    }
}

fn LegacyClientYear(ClientVersion: Option<&str>) -> Option<&'static str> {
    match ClientVersion {
        Some("2016L") => Some("2016"),
        Some("2018L") => Some("2018"),
        Some("2020L") => Some("2020"),
        _ => None,
    }
}

pub fn ParsePlayUri(Uri: &str) -> Result<PlayParams> {
    let AfterScheme = Uri.splitn(2, ':').nth(1).ok_or_else(|| anyhow!("missing scheme separator in URI"))?;
    let mut Params = PlayParams::default();
    for Token in AfterScheme.split('+') {
        let Some((Key, Value)) = Token.split_once(':') else { continue };
        let Value = percent_decode_str(Value).decode_utf8_lossy().to_string();
        match Key {
            "1" | "launchmode" => Params.LaunchMode = Value,
            "gameinfo" => Params.GameInfo = Some(Value),
            "placelauncherurl" => Params.PlaceLauncherUrl = Some(Value),
            "launchtime" => Params.LaunchTime = Some(Value),
            "clientversion" => Params.ClientVersion = Some(Value),
            "clientyear" => Params.ClientYear = Some(Value),
            "launchexp" => Params.LaunchExp = Some(Value),
            "browsertrackerid" => Params.BrowserTrackerId = Some(Value),
            "task" => Params.Task = Some(Value),
            "placeId" | "placeid" => Params.PlaceId = Some(Value),
            "universeId" | "universeid" => Params.UniverseId = Some(Value),
            "userId" | "userid" => Params.UserId = Some(Value),
            "script" => Params.Script = Some(Value),
            _ => {}
        }
    }
    Ok(Params)
}

fn PushArg(Args: &mut Vec<String>, Flag: &str, Value: Option<&String>) {
    if let Some(Value) = Value {
        Args.push(Flag.into());
        Args.push(Value.clone());
    }
}

pub fn BuildPlayerArgs(Params: &PlayParams) -> Vec<String> {
    let mut Args = vec!["-a".into(), format!("{}/Login/Negotiate.ashx", Config::BaseUrl)];
    PushArg(&mut Args, "-t", Params.GameInfo.as_ref());
    PushArg(&mut Args, "-j", Params.PlaceLauncherUrl.as_ref());
    PushArg(&mut Args, "-b", Params.LaunchTime.as_ref());
    let Legacy = LegacyClientYear(Params.ClientVersion.as_deref()).is_some();
    PushArg(&mut Args, "--rbxexp", Params.ClientVersion.as_ref().filter(|_| !Legacy));
    PushArg(&mut Args, "--launchexp", Params.LaunchExp.as_ref().filter(|_| !Legacy));
    PushArg(&mut Args, "--browserTrackerId", Params.BrowserTrackerId.as_ref());
    Args
}

pub fn BuildStudioArgs(Params: &PlayParams) -> Vec<String> {
    let Task = Params.Task.clone().unwrap_or_else(|| match Params.LaunchMode.as_str() {
        "" | "edit" => "EditPlace".to_string(),
        Other => Other.to_string(),
    });
    let mut Args = vec!["-a".into(), format!("{}/Login/Negotiate.ashx", Config::BaseUrl), "-task".into(), Task];
    PushArg(&mut Args, "-placeId", Params.PlaceId.as_ref());
    PushArg(&mut Args, "-universeId", Params.UniverseId.as_ref());
    PushArg(&mut Args, "-userId", Params.UserId.as_ref());
    PushArg(&mut Args, "-script", Params.Script.as_ref());
    Args
}

pub fn RunPipeline(State: &SharedState) -> Result<()> {
    let (PlayUrl, LocalFile, Client, SelectedKind, InstallOnly) = {
        let Locked = State.lock().unwrap();
        (Locked.PlayUrl.clone(), Locked.LocalFile.clone(), Locked.Client.clone(), Locked.Kind, Locked.InstallOnly)
    };
    let Label = SelectedKind.Label();
    let Upgrading = format!("Upgrading {Label}...");

    SetPhase(
        State,
        Phase::Connecting,
        if InstallOnly { "Upgrading Pekora..." } else { &Upgrading },
        None,
    );

    let InstalledLauncher =
        if InstallOnly { Installer::InstallLauncherFromServer()? } else { Installer::InstallSelf()? };
    if let Err(Error) = Installer::RegisterUrlSchemes(&InstalledLauncher) {
        log::warn!("URL scheme registration failed (non-fatal): {Error:#}");
    }
    if let Err(Error) = Installer::RegisterFileAssociations(&InstalledLauncher) {
        log::warn!("file-association registration failed (non-fatal): {Error:#}");
    }
    if let Err(Error) = Installer::EnforceGlobalVersion(State) {
        log::warn!("global version enforcement skipped: {Error:#}");
    }

    if InstallOnly {
        InstallKind(State, Kind::Player, &Client, (0.0, 0.5))?;
        let _ = InstallKind(State, Kind::Studio, &Client, (0.5, 1.0));
        SetPhase(State, Phase::Idle, "Pekora is installed. You can now play from the website.", Some(1.0));
        return Ok(());
    }

    let Version = Installer::ResolveTargetVersion(SelectedKind, &Client)
        .with_context(|| format!("resolving version for {Label} {Client}"))?;
    log::info!("{Label} {Client} target version: {Version}");

    if Installer::IsInstalled(SelectedKind, &Client, &Version) {
        log::info!("{Label} {Client} already at {Version}, skipping download");
        SetProgress(State, 0.95);
    } else {
        SetPhase(State, Phase::Configuring, &Upgrading, Some(0.0));
        Installer::InstallClient(SelectedKind, &Client, &Version, State)?;
    }

    SetPhase(State, Phase::Configuring, &Upgrading, Some(0.97));

    let ExePath = Config::TargetExe(SelectedKind, &Client)?;
    if !ExePath.is_file() {
        return Err(anyhow!("install reported success but {} is missing", ExePath.display()));
    }

    let Args = match (LocalFile.as_deref(), PlayUrl.as_deref()) {
        (Some(File), _) => {
            if SelectedKind != Kind::Studio {
                log::warn!("local file supplied with kind {Label}; forcing Studio open");
            }
            vec![File.to_string()]
        }
        (None, Some(Url)) => {
            let Parsed = ParsePlayUri(Url).context("parsing launch URI")?;
            match SelectedKind {
                Kind::Player => BuildPlayerArgs(&Parsed),
                Kind::Studio => BuildStudioArgs(&Parsed),
            }
        }
        (None, None) => {
            SetPhase(State, Phase::Idle, &format!("{Label} is ready. Launch from the website to start."), Some(1.0));
            return Ok(());
        }
    };

    SetPhase(State, Phase::Starting, &format!("Starting {Label}..."), Some(1.0));
    log::info!("spawning {} {:?}", ExePath.display(), Args);
    let ClientDir = ExePath.parent();
    if let Some(Dir) = ClientDir {
        EnsureClientRuntimeDirs(Dir);
    }

    let Watched = SelectedKind == Kind::Player && {
        let _ = crate::Fps::SpawnWindow();
        let VoiceToken = PlayUrl
            .as_deref()
            .and_then(|Url| ParsePlayUri(Url).ok())
            .and_then(|Params| Params.GameInfo)
            .and_then(|Ticket| crate::Voice::FetchToken(&Ticket));
        if let (Some(_), Some(Dir)) = (&VoiceToken, ClientDir) {
            crate::Voice::InstallTextures(Dir);
        }
        crate::Rpc::SpawnWatcher(&ExePath, &Args, VoiceToken).is_ok()
    };
    if !Watched {
        SpawnTarget(&ExePath, &Args)?;
    }
    SetPhase(State, Phase::Done, "Have fun!", Some(1.0));
    std::thread::sleep(Duration::from_millis(1200));
    State.lock().unwrap().RequestClose = true;
    Ok(())
}

fn InstallKind(State: &SharedState, SelectedKind: Kind, Client: &str, Range: (f32, f32)) -> Result<()> {
    let Label = SelectedKind.Label();
    SetPhase(State, Phase::Configuring, &format!("Upgrading {Label}..."), Some(Range.0));

    let Version = Installer::ResolveTargetVersion(SelectedKind, Client)
        .with_context(|| format!("resolving version for {Label} {Client}"))?;
    log::info!("{Label} {Client} target version: {Version}");

    if Installer::IsInstalled(SelectedKind, Client, &Version) {
        log::info!("{Label} {Client} already at {Version}, skipping download");
    } else {
        Installer::InstallClient(SelectedKind, Client, &Version, State)?;
    }
    SetProgress(State, Range.1);
    Ok(())
}

fn EnsureClientRuntimeDirs(ClientDir: &Path) {
    if let Some(Base) = ClientDir.ancestors().nth(3) {
        for Dir in ["meta", "logs", "logs/archive", "LocalStorage"] {
            let _ = std::fs::create_dir_all(Base.join(Dir));
        }
    }
}

fn SpawnTarget(Exe: &Path, Args: &[String]) -> Result<()> {
    let mut Cmd = Command::new(Exe);
    Cmd.args(Args);
    if let Some(Parent) = Exe.parent() {
        EnsureClientRuntimeDirs(Parent);
        Cmd.current_dir(Parent);
    }
    Cmd.spawn().with_context(|| format!("spawning {}", Exe.display()))?;
    Ok(())
}

pub fn InstallRootLowercase() -> Result<String> {
    Ok(Config::InstallRoot()?.to_string_lossy().to_ascii_lowercase())
}

pub fn ClientAlreadyRunning() -> Result<bool> {
    let Root = InstallRootLowercase()?;
    let mut Running = false;
    for Pid in PekoraClientPids(&Root)? {
        unsafe {
            if ClientIsStuck(Pid) {
                log::warn!("closing stuck client (pid {Pid})");
                let Process = OpenProcess(PROCESS_TERMINATE, 0, Pid);
                if Process != 0 {
                    TerminateProcess(Process, 1);
                    CloseHandle(Process);
                }
            } else {
                Running = true;
            }
        }
    }
    Ok(Running)
}

fn WideToString(Value: &[u16]) -> String {
    let End = Value.iter().position(|&Char| Char == 0).unwrap_or(Value.len());
    String::from_utf16_lossy(&Value[..End])
}

pub fn ProcessIds() -> Result<Vec<(u32, String)>> {
    let mut Found = Vec::new();
    unsafe {
        let Snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if Snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error().into());
        }
        let mut Entry: PROCESSENTRY32W = std::mem::zeroed();
        Entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut More = Process32FirstW(Snapshot, &mut Entry) != 0;
        while More {
            Found.push((Entry.th32ProcessID, WideToString(&Entry.szExeFile)));
            More = Process32NextW(Snapshot, &mut Entry) != 0;
        }
        CloseHandle(Snapshot);
    }
    Ok(Found)
}

pub fn PekoraClientPids(Root: &str) -> Result<Vec<u32>> {
    let ClientExe = Kind::Player.ExeName();
    Ok(ProcessIds()?
        .into_iter()
        .filter(|(Pid, Name)| Name.eq_ignore_ascii_case(ClientExe) && unsafe { ProcessImageUnder(*Pid, Root) })
        .map(|(Pid, _)| Pid)
        .collect())
}

struct WindowScan {
    Pid: u32,
    Visible: bool,
    CrashDialog: bool,
    Hung: bool,
}

unsafe extern "system" fn ScanWindow(Window: HWND, Param: LPARAM) -> BOOL {
    let Scan = &mut *(Param as *mut WindowScan);
    let mut Owner = 0u32;
    GetWindowThreadProcessId(Window, &mut Owner);
    if Owner != Scan.Pid || IsWindowVisible(Window) == 0 {
        return 1;
    }
    Scan.Visible = true;
    Scan.Hung |= IsHungAppWindow(Window) != 0;
    let mut Title = [0u16; 128];
    let Length = GetWindowTextW(Window, Title.as_mut_ptr(), Title.len() as i32);
    if Length > 0 && String::from_utf16_lossy(&Title[..Length as usize]).contains("Crash") {
        Scan.CrashDialog = true;
    }
    1
}

unsafe fn ClientIsStuck(Pid: u32) -> bool {
    let mut Scan = WindowScan { Pid, Visible: false, CrashDialog: false, Hung: false };
    EnumWindows(Some(ScanWindow), &mut Scan as *mut WindowScan as LPARAM);
    Scan.CrashDialog
        || Scan.Hung
        || (!Scan.Visible && ProcessAgeSecs(Pid).map_or(false, |Age| Age >= StuckWithoutWindowSecs))
}

unsafe fn ProcessAgeSecs(Pid: u32) -> Option<u64> {
    let Process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, Pid);
    if Process == 0 {
        return None;
    }
    let mut Created: FILETIME = std::mem::zeroed();
    let mut Exited: FILETIME = std::mem::zeroed();
    let mut Kernel: FILETIME = std::mem::zeroed();
    let mut User: FILETIME = std::mem::zeroed();
    let Succeeded = GetProcessTimes(Process, &mut Created, &mut Exited, &mut Kernel, &mut User) != 0;
    CloseHandle(Process);
    if !Succeeded {
        return None;
    }
    let CreatedTicks = ((Created.dwHighDateTime as u64) << 32) | Created.dwLowDateTime as u64;
    let CreatedUnix = (CreatedTicks / 10_000_000).checked_sub(11_644_473_600)?;
    let Now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(Now.saturating_sub(CreatedUnix))
}

pub unsafe fn ProcessImageUnder(Pid: u32, Root: &str) -> bool {
    let Process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, Pid);
    if Process == 0 {
        return false;
    }
    let mut Path = vec![0u16; ImagePathCapacity];
    let mut Length = Path.len() as u32;
    let Found = QueryFullProcessImageNameW(Process, 0, Path.as_mut_ptr(), &mut Length) != 0;
    CloseHandle(Process);
    Found && String::from_utf16_lossy(&Path[..Length as usize]).to_ascii_lowercase().starts_with(Root)
}
