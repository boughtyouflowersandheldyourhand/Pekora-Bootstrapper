use std::path::{Path, PathBuf};
use std::process::Command;

use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};

use crate::Voice;

const DiscordAppId: &str = "1544211678386978866";
const LargeImage: &str = "pekora";
const LargeText: &str = "Pekora";
const Details: &str = "Playing Pekora";

pub struct Presence {
    Client: DiscordIpcClient,
}

impl Presence {
    pub fn Connect() -> Option<Presence> {
        let mut Client = DiscordIpcClient::new(DiscordAppId).ok()?;
        Client.connect().ok()?;
        Some(Presence { Client })
    }

    pub fn SetPlaying(&mut self) {
        let Start = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |Elapsed| Elapsed.as_secs() as i64);
        let _ = self.Client.set_activity(
            activity::Activity::new()
                .details(Details)
                .assets(activity::Assets::new().large_image(LargeImage).large_text(LargeText))
                .timestamps(activity::Timestamps::new().start(Start)),
        );
    }

    pub fn Clear(&mut self) {
        let _ = self.Client.clear_activity();
        let _ = self.Client.close();
    }
}

fn VoiceEnv() -> Option<(String, String)> {
    Some((std::env::var(Voice::EnvToken).ok()?, std::env::var(Voice::EnvAddr).ok()?))
}

pub fn SpawnWatcher(GameExe: &Path, GameArgs: &[String], VoiceToken: Option<(String, String)>) -> std::io::Result<()> {
    let mut Cmd = Command::new(std::env::current_exe()?);
    Cmd.arg("--rpc-play").arg(GameExe).args(GameArgs);
    if let Some((Token, Addr)) = VoiceToken {
        Cmd.env(Voice::EnvToken, Token).env(Voice::EnvAddr, Addr);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DetachedProcess: u32 = 0x0000_0008;
        Cmd.creation_flags(DetachedProcess);
    }
    Cmd.spawn()?;
    Ok(())
}

pub fn WatchAndPlay(Rest: &[String]) -> anyhow::Result<()> {
    let Exe = Rest.first().map(PathBuf::from).ok_or_else(|| anyhow::anyhow!("--rpc-play requires a game path"))?;
    let mut Cmd = Command::new(&Exe);
    Cmd.args(&Rest[1..]).env_remove(Voice::EnvToken).env_remove(Voice::EnvAddr);
    if let Some(Parent) = Exe.parent() {
        Cmd.current_dir(Parent);
    }
    let mut Child = Cmd.spawn()?;

    if let Some((Token, Addr)) = VoiceEnv() {
        Voice::Start(Token, Addr);
    }

    let mut Presence = Presence::Connect();
    if let Some(Active) = Presence.as_mut() {
        Active.SetPlaying();
    }
    let _ = Child.wait();
    if let Some(Active) = Presence.as_mut() {
        Active.Clear();
    }
    Ok(())
}

pub fn WatchVoice(Rest: &[String]) -> anyhow::Result<()> {
    let Pid: u32 = Rest
        .first()
        .and_then(|Value| Value.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("--voice-watch requires a process id"))?;
    if let Some((Token, Addr)) = VoiceEnv() {
        Voice::Start(Token, Addr);
        WaitForExit(Pid);
    }
    Ok(())
}

#[cfg(windows)]
fn WaitForExit(Pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE};
    unsafe {
        let Process = OpenProcess(PROCESS_SYNCHRONIZE, 0, Pid);
        if Process != 0 {
            WaitForSingleObject(Process, INFINITE);
            CloseHandle(Process);
        }
    }
}

#[cfg(not(windows))]
fn WaitForExit(_Pid: u32) {}
