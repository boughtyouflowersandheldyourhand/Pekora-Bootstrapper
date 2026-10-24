use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

use nnnoiseless::DenoiseState;
use ringbuf::traits::{Consumer, Observer};

use super::Devices::InputFeed;
use super::Proto::MaxOpusFrame;
use super::Resample::Resampler;
use super::{Net, Shared, FrameSize, SampleRate};

const RnFrame: usize = DenoiseState::FRAME_SIZE;
const VadOpen: f32 = 0.6;
const Hangover: u32 = 30;
const AgcTargetRms: f32 = 0.08;
const AgcMin: f32 = 0.6;
const AgcMax: f32 = 5.0;

struct Encoder {
    Opus: opus::Encoder,
    Out: [u8; MaxOpusFrame],
}

impl Encoder {
    fn New() -> Option<Self> {
        let mut Opus = opus::Encoder::new(SampleRate, opus::Channels::Mono, opus::Application::Voip).ok()?;
        let _ = Opus.set_bitrate(opus::Bitrate::Bits(32_000));
        let _ = Opus.set_inband_fec(true);
        let _ = Opus.set_packet_loss_perc(10);
        Some(Encoder { Opus, Out: [0u8; MaxOpusFrame] })
    }

    fn Encode(&mut self, Pcm: &[f32]) -> Option<&[u8]> {
        let Length = self.Opus.encode_float(Pcm, &mut self.Out).ok()?;
        Some(&self.Out[..Length])
    }
}

pub fn Run(State: Arc<Shared>, Receiver: Receiver<InputFeed>) {
    let Some(mut Encoder) = Encoder::New() else {
        log::warn!("voice: opus encoder init failed");
        return;
    };
    let mut Denoise = DenoiseState::new();
    let mut Feed: Option<InputFeed> = None;
    let mut Converter = Resampler::New(SampleRate, SampleRate, 1);

    let mut Raw: Vec<f32> = Vec::new();
    let mut Pcm48: Vec<f32> = Vec::new();
    let mut Frame: Vec<f32> = Vec::with_capacity(FrameSize);
    let mut Scaled = [0.0f32; RnFrame];
    let mut Clean = [0.0f32; RnFrame];

    let mut Gain = 1.0f32;
    let mut Hang = 0u32;
    let mut Level = 0.0f32;
    let mut Sequence: u16 = 0;
    let mut Preroll: Option<Vec<u8>> = None;
    let mut WasOpen = false;

    while !State.Stopped() {
        while let Ok(NewFeed) = Receiver.try_recv() {
            Converter = Resampler::New(NewFeed.Rate, SampleRate, 1);
            Feed = Some(NewFeed);
            Pcm48.clear();
            Frame.clear();
        }
        let Some(Active) = Feed.as_mut() else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let Available = Active.Consumer.occupied_len();
        if Available == 0 {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        Raw.resize(Available, 0.0);
        let Got = Active.Consumer.pop_slice(&mut Raw);
        Converter.Process(&Raw[..Got], &mut Pcm48);

        let mut Consumed = 0;
        while Pcm48.len() - Consumed >= RnFrame {
            let Chunk = &Pcm48[Consumed..Consumed + RnFrame];
            Consumed += RnFrame;

            for (Destination, Sample) in Scaled.iter_mut().zip(Chunk) {
                *Destination = Sample * 32767.0;
            }
            let Vad = Denoise.process_frame(&mut Clean, &Scaled);

            let Rms = (Clean.iter().map(|Value| (Value / 32767.0).powi(2)).sum::<f32>() / RnFrame as f32).sqrt();
            if Vad > VadOpen {
                Hang = Hangover;
                if Rms > 1e-4 {
                    Gain += ((AgcTargetRms / Rms).clamp(AgcMin, AgcMax) - Gain) * 0.03;
                }
            } else {
                Hang = Hang.saturating_sub(1);
            }
            let Open = Hang > 0;

            let Shown = if Open { (Rms * Gain * 6.0).min(1.0) } else { 0.0 };
            Level = if Shown > Level { Shown } else { Level * 0.85 + Shown * 0.15 };

            for Value in Clean.iter() {
                let Scaled = Value / 32767.0 * Gain;
                Frame.push(if Scaled.abs() > 0.89 {
                    Scaled.signum() * (0.89 + (Scaled.abs() - 0.89).tanh() * 0.11)
                } else {
                    Scaled
                });
            }

            if Frame.len() >= FrameSize {
                let Sending = State.Transmitting() && Open;
                if let Some(Packet) = Encoder.Encode(&Frame[..FrameSize]) {
                    if Sending {
                        if let Some(Pending) = Preroll.take().filter(|_| !WasOpen) {
                            Sequence = Sequence.wrapping_add(1);
                            Net::SendAudio(&State, Sequence, &Pending);
                        }
                        Sequence = Sequence.wrapping_add(1);
                        Net::SendAudio(&State, Sequence, Packet);
                        Preroll = None;
                    } else {
                        Preroll = Some(Packet.to_vec());
                    }
                }
                WasOpen = Sending;
                Frame.drain(..FrameSize);
            }
        }
        Pcm48.drain(..Consumed);

        if let Ok(mut Status) = State.Status.try_lock() {
            Status.Level = if State.Transmitting() { Level } else { 0.0 };
        }
    }
}
