use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ringbuf::traits::{Observer, Producer};

use super::Devices::OutputFeed;
use super::Resample::Resampler;
use super::{Shared, Spatial, FrameSize, SampleRate};

const Prebuffer: usize = 3;
const MaxQueue: usize = 8;
const MaxConceal: u32 = 3;
const OutputTargetMs: usize = 60;
const IdleDrop: Duration = Duration::from_secs(20);
const DefaultNear: f32 = 7.0;
const BehindLowpass: f32 = 0.28;

pub struct SpeakerQueue {
    Decoder: opus::Decoder,
    Packets: BTreeMap<i64, Vec<u8>>,
    Highest: Option<(i64, u16)>,
    Next: Option<i64>,
    Conceal: u32,
    LastReceived: Instant,
    Gain: (f32, f32),
    Lowpass: f32,
    LowpassState: f32,
}

impl SpeakerQueue {
    fn New() -> Option<Self> {
        Some(SpeakerQueue {
            Decoder: opus::Decoder::new(SampleRate, opus::Channels::Mono).ok()?,
            Packets: BTreeMap::new(),
            Highest: None,
            Next: None,
            Conceal: 0,
            LastReceived: Instant::now(),
            Gain: (0.0, 0.0),
            Lowpass: 1.0,
            LowpassState: 0.0,
        })
    }

    fn Extend(&mut self, Sequence: u16) -> i64 {
        let Extended = match self.Highest {
            None => Sequence as i64,
            Some((HighestExtended, HighestRaw)) => {
                HighestExtended + Sequence.wrapping_sub(HighestRaw) as i16 as i64
            }
        };
        if self.Highest.map_or(true, |(HighestExtended, _)| Extended > HighestExtended) {
            self.Highest = Some((Extended, Sequence));
        }
        Extended
    }

    fn Push(&mut self, Sequence: u16, Data: &[u8]) {
        let Extended = self.Extend(Sequence);
        self.LastReceived = Instant::now();
        if self.Next.map_or(false, |Next| Extended < Next) {
            return;
        }
        if self.Packets.len() < 64 {
            self.Packets.insert(Extended, Data.to_vec());
        }
    }

    fn FirstKey(&self) -> Option<i64> {
        self.Packets.keys().next().copied()
    }

    fn NextFrame(&mut self, Out: &mut [f32; FrameSize]) -> bool {
        if self.Next.is_none() {
            if self.Packets.len() < Prebuffer {
                return false;
            }
            self.Next = self.FirstKey();
            self.Conceal = 0;
        }
        if self.Packets.len() > MaxQueue {
            while self.Packets.len() > Prebuffer {
                self.Packets.pop_first();
            }
            self.Next = self.FirstKey();
        }
        let Playhead = self.Next.unwrap();
        while self.FirstKey().map_or(false, |Key| Key < Playhead) {
            self.Packets.pop_first();
        }

        let Decoded = if let Some(Packet) = self.Packets.remove(&Playhead) {
            self.Conceal = 0;
            self.Decoder.decode_float(&Packet, Out, false).is_ok()
        } else if self.Packets.is_empty() {
            self.Conceal += 1;
            if self.Conceal > MaxConceal {
                self.Next = None;
                return false;
            }
            self.Decoder.decode_float(&[], Out, false).is_ok()
        } else if let Some(Following) = self.Packets.get(&(Playhead + 1)) {
            self.Decoder.decode_float(Following, Out, true).is_ok()
        } else {
            self.Decoder.decode_float(&[], Out, false).is_ok()
        };
        self.Next = Some(Playhead + 1);
        Decoded
    }
}

pub fn PushPacket(State: &Shared, From: u64, Sequence: u16, Data: &[u8]) {
    if Data.is_empty() {
        return;
    }
    let mut Speakers = State.Speakers.lock().unwrap();
    if !Speakers.contains_key(&From) {
        if Speakers.len() >= 48 {
            return;
        }
        let Some(Queue) = SpeakerQueue::New() else { return };
        Speakers.insert(From, Queue);
    }
    Speakers.get_mut(&From).unwrap().Push(Sequence, Data);
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Placement {
    Left: f32,
    Right: f32,
    Lowpass: f32,
}

const Center: Placement = Placement { Left: 1.0, Right: 1.0, Lowpass: 1.0 };

fn Place(Space: &Spatial, UserId: u64) -> Placement {
    let (Some(Me), Some(Peer)) = (Space.Me, Space.Peers.get(&UserId)) else {
        return Center;
    };
    let Near = if Space.Near > 0.5 { Space.Near } else { DefaultNear };
    let Range = Space.Range.max(Near * 2.0);
    let Delta = [Peer[0] - Me.Pos[0], Peer[1] - Me.Pos[1], Peer[2] - Me.Pos[2]];
    let Distance = (Delta[0] * Delta[0] + Delta[1] * Delta[1] + Delta[2] * Delta[2]).sqrt();

    let mut Volume = if Distance <= Near { 1.0 } else { Near / Distance };
    let FadeStart = Range * 0.75;
    if Distance > FadeStart {
        Volume *= ((Range - Distance) / (Range - FadeStart)).clamp(0.0, 1.0);
    }

    let (LookX, LookZ) = (Me.Look[0], Me.Look[2]);
    let LookLength = (LookX * LookX + LookZ * LookZ).sqrt();
    let FlatLength = (Delta[0] * Delta[0] + Delta[2] * Delta[2]).sqrt();
    if LookLength < 1e-3 || FlatLength < 0.75 {
        return Placement { Left: Volume, Right: Volume, Lowpass: 1.0 };
    }
    let (LookX, LookZ) = (LookX / LookLength, LookZ / LookLength);
    let (DirX, DirZ) = (Delta[0] / FlatLength, Delta[2] / FlatLength);
    let Side = (DirX * -LookZ + DirZ * LookX).clamp(-1.0, 1.0);
    let Front = (DirX * LookX + DirZ * LookZ).clamp(-1.0, 1.0);

    let Angle = (Side * 0.85 + 1.0) * std::f32::consts::FRAC_PI_4;
    let LeftGain = (Angle.cos() * std::f32::consts::SQRT_2).min(1.0);
    let RightGain = (Angle.sin() * std::f32::consts::SQRT_2).min(1.0);

    let Behind = (-Front).max(0.0);
    let Volume = Volume * (1.0 - 0.3 * Behind);
    Placement {
        Left: Volume * LeftGain,
        Right: Volume * RightGain,
        Lowpass: 1.0 - (1.0 - BehindLowpass) * Behind,
    }
}

pub fn Run(State: Arc<Shared>, Receiver: Receiver<OutputFeed>) {
    let mut Feed: Option<OutputFeed> = None;
    let mut Converter = Resampler::New(SampleRate, SampleRate, 2);
    let mut Mono = [0.0f32; FrameSize];
    let mut Mix = vec![0.0f32; FrameSize * 2];
    let mut DeviceSamples: Vec<f32> = Vec::with_capacity(FrameSize * 6);

    while !State.Stopped() {
        while let Ok(NewFeed) = Receiver.try_recv() {
            Converter = Resampler::New(SampleRate, NewFeed.Rate, 2);
            Feed = Some(NewFeed);
        }
        let Some(Active) = Feed.as_mut() else {
            State.Speakers.lock().unwrap().clear();
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        if Active.Producer.occupied_len() >= Active.Rate as usize * 2 * OutputTargetMs / 1000 {
            std::thread::sleep(Duration::from_millis(4));
            continue;
        }

        Mix.fill(0.0);
        let Silent = {
            let Current = State.Settings.lock().unwrap();
            Current.Deafened || !Current.InGame
        };
        if !Silent {
            let Space = State.Spatial.lock().unwrap();
            let mut Speakers = State.Speakers.lock().unwrap();
            Speakers.retain(|_, Queue| Queue.LastReceived.elapsed() < IdleDrop);
            for (&UserId, Queue) in Speakers.iter_mut() {
                if !Queue.NextFrame(&mut Mono) {
                    continue;
                }
                let Target = Place(&Space, UserId);
                let (StartLeft, StartRight) = Queue.Gain;
                let EndLeft = StartLeft + (Target.Left - StartLeft) * 0.35;
                let EndRight = StartRight + (Target.Right - StartRight) * 0.35;
                Queue.Lowpass += (Target.Lowpass - Queue.Lowpass) * 0.35;
                let Coefficient = Queue.Lowpass;
                for (Index, Sample) in Mono.iter().enumerate() {
                    Queue.LowpassState += Coefficient * (Sample - Queue.LowpassState);
                    let Value = Queue.LowpassState;
                    let Fraction = Index as f32 / FrameSize as f32;
                    Mix[Index * 2] += Value * (StartLeft + (EndLeft - StartLeft) * Fraction);
                    Mix[Index * 2 + 1] += Value * (StartRight + (EndRight - StartRight) * Fraction);
                }
                Queue.Gain = (EndLeft, EndRight);
            }
        }
        for Value in Mix.iter_mut() {
            if Value.abs() > 0.9 {
                *Value = Value.signum() * (0.9 + (Value.abs() - 0.9).tanh() * 0.1);
            }
        }

        DeviceSamples.clear();
        Converter.Process(&Mix, &mut DeviceSamples);
        let Length = DeviceSamples.len() & !1;
        if Active.Producer.vacant_len() >= Length {
            Active.Producer.push_slice(&DeviceSamples[..Length]);
        }
    }
}

#[cfg(test)]
mod Tests {
    use super::*;

    fn EncodeFrame(Encoder: &mut opus::Encoder, Frequency: f32) -> Vec<u8> {
        let Pcm: Vec<f32> = (0..FrameSize).map(|Index| (Index as f32 * Frequency).sin() * 0.3).collect();
        let mut Out = [0u8; 1000];
        let Length = Encoder.encode_float(&Pcm, &mut Out).unwrap();
        Out[..Length].to_vec()
    }

    #[test]
    fn ReordersConcealsAndStops() {
        let mut Encoder = opus::Encoder::new(SampleRate, opus::Channels::Mono, opus::Application::Voip).unwrap();
        Encoder.set_inband_fec(true).unwrap();
        Encoder.set_packet_loss_perc(10).unwrap();
        let Packets: Vec<Vec<u8>> = (0..10).map(|_| EncodeFrame(&mut Encoder, 0.05)).collect();
        let mut Queue = SpeakerQueue::New().unwrap();
        let mut Out = [0.0; FrameSize];

        let Base: u16 = 65533;
        for &Index in &[0usize, 2, 1, 4, 5, 6] {
            Queue.Push(Base.wrapping_add(Index as u16), &Packets[Index]);
        }
        let mut Played = 0;
        while Queue.NextFrame(&mut Out) {
            Played += 1;
            assert!(Played < 20);
        }
        assert_eq!(Played, 7 + MaxConceal as usize);
        assert!(Queue.Next.is_none());
    }

    #[test]
    fn PlacesTalkersIn3d() {
        let mut Space = Spatial { Range: 80.0, Near: 7.0, ..Default::default() };
        Space.Me = Some(super::super::Listener { Pos: [0.0, 0.0, 0.0], Look: [0.0, 0.0, -1.0] });
        Space.Peers.insert(1, [20.0, 0.0, 0.0]);
        Space.Peers.insert(2, [0.0, 0.0, -5.0]);
        Space.Peers.insert(3, [0.0, 0.0, -200.0]);
        Space.Peers.insert(4, [0.0, 0.0, 5.0]);
        Space.Peers.insert(5, [0.0, 0.0, -40.0]);
        Space.Peers.insert(6, [-20.0, 0.0, 0.0]);

        let Right = Place(&Space, 1);
        assert!(Right.Right > Right.Left * 4.0, "{Right:?}");
        let Left = Place(&Space, 6);
        assert!(Left.Left > Left.Right * 4.0, "{Left:?}");

        let Ahead = Place(&Space, 2);
        assert!((Ahead.Left - 1.0).abs() < 1e-3 && (Ahead.Right - 1.0).abs() < 1e-3 && Ahead.Lowpass == 1.0);

        let Behind = Place(&Space, 4);
        assert!(Behind.Lowpass < 0.4 && Behind.Left < 0.8, "{Behind:?}");

        let Far = Place(&Space, 5);
        assert!(Far.Left < 0.2 && Far.Left > 0.1, "{Far:?}");

        let Gone = Place(&Space, 3);
        assert_eq!((Gone.Left, Gone.Right), (0.0, 0.0));
        assert_eq!(Place(&Space, 99), Center);
    }
}
