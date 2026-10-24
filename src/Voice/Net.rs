use std::net::{ToSocketAddrs, UdpSocket};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use super::Proto::*;
use super::{Listener, Shared};

const HelloEvery: Duration = Duration::from_millis(1000);
const StatusEvery: Duration = Duration::from_millis(1000);
const SilenceTimeout: Duration = Duration::from_secs(6);

static Socket: OnceLock<UdpSocket> = OnceLock::new();

pub fn SendAudio(State: &Shared, Sequence: u16, Opus: &[u8]) {
    let SessionId = State.Session.load(Ordering::Relaxed);
    let (true, Some(Sock)) = (SessionId != 0, Socket.get()) else { return };
    let mut Packet = Vec::with_capacity(11 + Opus.len());
    Packet.push(ClientAudio);
    Packet.extend_from_slice(&SessionId.to_le_bytes());
    Packet.extend_from_slice(&Sequence.to_le_bytes());
    Packet.extend_from_slice(Opus);
    let _ = Sock.send(&Packet);
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "lowercase")]
struct StateMsg {
    V: u32,
    #[serde(rename = "inGame")]
    InGame: bool,
    Muted: bool,
    Deafened: bool,
    Input: Option<String>,
    Output: Option<String>,
    #[serde(rename = "devSeq")]
    DevSeq: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
struct PosMsg {
    Me: [f32; 6],
    #[serde(default)]
    P: Vec<[f64; 4]>,
    #[serde(default)]
    Range: Option<f32>,
    #[serde(default)]
    Near: Option<f32>,
}

pub fn Run(State: Arc<Shared>, Token: String, Addr: String) {
    let Some(Target) = Addr.to_socket_addrs().ok().and_then(|mut Found| Found.find(|Candidate| Candidate.is_ipv4())) else {
        log::warn!("voice: cannot resolve relay {Addr}");
        return;
    };
    let Sock = match UdpSocket::bind("0.0.0.0:0").and_then(|Bound| Bound.connect(Target).map(|_| Bound)) {
        Ok(Bound) => Bound,
        Err(Error) => {
            log::warn!("voice: udp setup failed: {Error}");
            return;
        }
    };
    let _ = Sock.set_read_timeout(Some(Duration::from_millis(100)));
    let Sock = Socket.get_or_init(|| Sock);

    let mut Hello = vec![ClientHello, ProtocolVersion];
    Hello.extend_from_slice(Token.as_bytes());

    let mut LastHello = Instant::now() - HelloEvery;
    let mut LastStatus = Instant::now() - StatusEvery;
    let mut LastHeard = Instant::now();
    let mut Buffer = [0u8; 2048];

    while !State.Stopped() {
        let Now = Instant::now();
        let SessionId = State.Session.load(Ordering::Relaxed);

        if SessionId == 0 {
            if Now.duration_since(LastHello) >= HelloEvery {
                let _ = Sock.send(&Hello);
                LastHello = Now;
            }
        } else {
            if Now.duration_since(LastHeard) > SilenceTimeout {
                log::info!("voice: relay went quiet, reconnecting");
                State.Session.store(0, Ordering::Relaxed);
                continue;
            }
            if State.StatusDirty.swap(false, Ordering::Relaxed) || Now.duration_since(LastStatus) >= StatusEvery {
                SendStatus(&State, Sock, SessionId);
                LastStatus = Now;
            }
        }

        let Ok(Length) = Sock.recv(&mut Buffer) else { continue };
        LastHeard = Instant::now();
        let Packet = &Buffer[..Length];
        match Packet.first() {
            Some(&ServerWelcome) => {
                if let Some(Id) = ReadU64(Packet, 1) {
                    if SessionId != Id {
                        log::info!("voice: connected (user {})", ReadU64(Packet, 9).unwrap_or(0));
                    }
                    State.Session.store(Id, Ordering::Relaxed);
                    State.StatusDirty.store(true, Ordering::Relaxed);
                }
            }
            Some(&ServerState) => OnState(&State, &Packet[1..]),
            Some(&ServerPos) => OnPos(&State, &Packet[1..]),
            Some(&ServerAudio) => {
                if let (Some(From), Some(Sequence)) = (ReadU64(Packet, 1), ReadU16(Packet, 9)) {
                    super::Mixer::PushPacket(&State, From, Sequence, &Packet[11..]);
                }
            }
            Some(&ServerBye) => {
                let Reason = String::from_utf8_lossy(&Packet[1..]).to_string();
                State.Session.store(0, Ordering::Relaxed);
                if Reason == "no-session" {
                    LastHello = Instant::now() - HelloEvery;
                } else {
                    log::info!("voice: relay closed session: {Reason}");
                    State.Stop.store(true, Ordering::Relaxed);
                }
            }
            _ => {}
        }
    }
}

fn SendStatus(State: &Shared, Sock: &UdpSocket, SessionId: u64) {
    let Ack = State.Settings.lock().unwrap().Version;
    let Status = State.Status.lock().unwrap().clone();
    let Body = json!({
        "ack": Ack,
        "inputs": Status.Inputs,
        "outputs": Status.Outputs,
        "input": Status.Input,
        "output": Status.Output,
        "level": (Status.Level * 100.0).round() / 100.0,
        "err": Status.Err,
    });
    let mut Packet = vec![ClientStatus];
    Packet.extend_from_slice(&SessionId.to_le_bytes());
    Packet.extend_from_slice(Body.to_string().as_bytes());
    let _ = Sock.send(&Packet);
}

fn OnState(State: &Shared, Body: &[u8]) {
    let Ok(Message) = serde_json::from_slice::<StateMsg>(Body) else { return };
    let mut Current = State.Settings.lock().unwrap();
    let DevicesChanged = Message.DevSeq != Current.DevSeq && (Message.Input.is_some() || Message.Output.is_some());
    let Changed = Current.Version != Message.V || Current.InGame != Message.InGame;
    Current.Version = Message.V;
    Current.InGame = Message.InGame;
    Current.Muted = Message.Muted;
    Current.Deafened = Message.Deafened;
    Current.DevSeq = Message.DevSeq;
    drop(Current);
    if DevicesChanged {
        *State.DeviceRequest.lock().unwrap() = Some((Message.Input, Message.Output));
    }
    if Changed {
        State.StatusDirty.store(true, Ordering::Relaxed);
    }
    if !Message.InGame || Message.Deafened {
        State.Speakers.lock().unwrap().clear();
    }
}

fn OnPos(State: &Shared, Body: &[u8]) {
    let Ok(Message) = serde_json::from_slice::<PosMsg>(Body) else { return };
    let mut Spatial = State.Spatial.lock().unwrap();
    Spatial.Me = Some(Listener {
        Pos: [Message.Me[0], Message.Me[1], Message.Me[2]],
        Look: [Message.Me[3], Message.Me[4], Message.Me[5]],
    });
    Spatial.Peers.clear();
    for Peer in Message.P {
        Spatial.Peers.insert(Peer[0] as u64, [Peer[1] as f32, Peer[2] as f32, Peer[3] as f32]);
    }
    if let Some(Range) = Message.Range.filter(|Range| Range.is_finite() && *Range > 1.0) {
        Spatial.Range = Range;
    }
    if let Some(Near) = Message.Near.filter(|Near| Near.is_finite() && *Near > 0.5) {
        Spatial.Near = Near;
    }
}
