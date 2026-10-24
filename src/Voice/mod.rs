mod Capture;
mod Devices;
mod Mixer;
mod Net;
mod Prefs;
pub mod Proto;
mod Resample;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::Config;

pub const SampleRate: u32 = 48_000;
pub const FrameSize: usize = 960;
pub const EnvToken: &str = "PEKORA_VOICE_TOKEN";
pub const EnvAddr: &str = "PEKORA_VOICE_ADDR";
pub const DefaultDevice: &str = "Default";

#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub Version: u32,
    pub InGame: bool,
    pub Muted: bool,
    pub Deafened: bool,
    pub DevSeq: u32,
}

#[derive(Debug, Clone, Default)]
pub struct LocalStatus {
    pub Inputs: Vec<String>,
    pub Outputs: Vec<String>,
    pub Input: String,
    pub Output: String,
    pub Level: f32,
    pub Err: String,
}

#[derive(Debug, Clone, Copy)]
pub struct Listener {
    pub Pos: [f32; 3],
    pub Look: [f32; 3],
}

#[derive(Default)]
pub struct Spatial {
    pub Me: Option<Listener>,
    pub Peers: HashMap<u64, [f32; 3]>,
    pub Range: f32,
    pub Near: f32,
}

pub struct Shared {
    pub Stop: AtomicBool,
    pub Session: AtomicU64,
    pub Settings: Mutex<Settings>,
    pub Status: Mutex<LocalStatus>,
    pub StatusDirty: AtomicBool,
    pub Spatial: Mutex<Spatial>,
    pub Speakers: Mutex<HashMap<u64, Mixer::SpeakerQueue>>,
    pub DeviceRequest: Mutex<Option<(Option<String>, Option<String>)>>,
}

impl Shared {
    fn New() -> Self {
        Shared {
            Stop: AtomicBool::new(false),
            Session: AtomicU64::new(0),
            Settings: Mutex::new(Settings { Muted: true, ..Default::default() }),
            Status: Mutex::new(LocalStatus::default()),
            StatusDirty: AtomicBool::new(true),
            Spatial: Mutex::new(Spatial { Range: 80.0, Near: 7.0, ..Default::default() }),
            Speakers: Mutex::new(HashMap::new()),
            DeviceRequest: Mutex::new(None),
        }
    }

    pub fn Stopped(&self) -> bool {
        self.Stop.load(Ordering::Relaxed)
    }

    pub fn Transmitting(&self) -> bool {
        let Current = self.Settings.lock().unwrap();
        Current.InGame && !Current.Muted && !Current.Deafened && self.Session.load(Ordering::Relaxed) != 0
    }

    pub fn SetStatus(&self, Update: impl FnOnce(&mut LocalStatus)) {
        Update(&mut self.Status.lock().unwrap());
        self.StatusDirty.store(true, Ordering::Relaxed);
    }
}

pub fn Start(Token: String, Addr: String) {
    let State = Arc::new(Shared::New());
    let (InputSender, InputReceiver) = std::sync::mpsc::channel();
    let (OutputSender, OutputReceiver) = std::sync::mpsc::channel();

    let Spawn = |Name: &str, Body: Box<dyn FnOnce() + Send>| {
        let _ = std::thread::Builder::new().name(Name.into()).spawn(Body);
    };
    let NetState = State.clone();
    Spawn("voice-net", Box::new(move || Net::Run(NetState, Token, Addr)));
    let CaptureState = State.clone();
    Spawn("voice-capture", Box::new(move || Capture::Run(CaptureState, InputReceiver)));
    let MixerState = State.clone();
    Spawn("voice-mixer", Box::new(move || Mixer::Run(MixerState, OutputReceiver)));
    Spawn("voice-devices", Box::new(move || Devices::Run(State, InputSender, OutputSender)));
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
struct TokenResponse {
    Token: String,
    Host: String,
    Port: u16,
}

pub fn FetchToken(Ticket: &str) -> Option<(String, String)> {
    let Response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .ok()?
        .post(format!("{}/v1/voice/launcher-token", Config::BaseUrl))
        .header("User-Agent", "PekoraLauncher")
        .json(&serde_json::json!({ "ticket": Ticket }))
        .send()
        .ok()?;
    if !Response.status().is_success() {
        log::info!("voice token unavailable: {}", Response.status());
        return None;
    }
    let Body: TokenResponse = Response.json().ok()?;
    Some((Body.Token, format!("{}:{}", Body.Host, Body.Port)))
}

const Icons: &[(&str, &[u8])] = &[
    ("mic.png", include_bytes!("../../assets/voice/mic.png")),
    ("mic_muted.png", include_bytes!("../../assets/voice/mic_muted.png")),
    ("mic_fill.png", include_bytes!("../../assets/voice/mic_fill.png")),
    ("speaker.png", include_bytes!("../../assets/voice/speaker.png")),
    ("headphones_off.png", include_bytes!("../../assets/voice/headphones_off.png")),
    ("check.png", include_bytes!("../../assets/voice/check.png")),
];

pub fn InstallTextures(ClientDir: &Path) {
    let Dir = ClientDir.join("content").join("textures").join("ui").join("PekoraVoice");
    if std::fs::create_dir_all(&Dir).is_err() {
        return;
    }
    for (Name, Bytes) in Icons {
        let File = Dir.join(Name);
        if std::fs::read(&File).map_or(true, |Existing| Existing != *Bytes) {
            let _ = std::fs::write(&File, Bytes);
        }
    }
}
