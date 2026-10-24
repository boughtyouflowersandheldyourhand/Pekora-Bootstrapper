use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, Host, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::{Prefs, Shared, DefaultDevice};

pub struct InputFeed {
    pub Consumer: HeapCons<f32>,
    pub Rate: u32,
}

pub struct OutputFeed {
    pub Producer: HeapProd<f32>,
    pub Rate: u32,
}

const EnumEvery: Duration = Duration::from_secs(3);
const RetryEvery: Duration = Duration::from_secs(3);

struct Side {
    Want: String,
    Stream: Option<Stream>,
    ActiveName: String,
    Failed: Arc<AtomicBool>,
    Rebuild: bool,
    LastAttempt: Option<Instant>,
}

impl Side {
    fn New(Want: Option<String>) -> Self {
        Side {
            Want: Want.filter(|Name| !Name.is_empty()).unwrap_or_else(|| DefaultDevice.to_string()),
            Stream: None,
            ActiveName: String::new(),
            Failed: Arc::new(AtomicBool::new(false)),
            Rebuild: true,
            LastAttempt: None,
        }
    }

    fn Due(&self) -> bool {
        self.Rebuild && self.LastAttempt.map_or(true, |Attempt| Attempt.elapsed() >= RetryEvery)
    }

    fn Request(&mut self, Name: String) {
        self.Want = Name;
        self.Rebuild = true;
        self.LastAttempt = None;
    }
}

pub fn Run(State: Arc<Shared>, InputSender: Sender<InputFeed>, OutputSender: Sender<OutputFeed>) {
    let AudioHost = cpal::default_host();
    let mut Saved = Prefs::Load();
    let mut Input = Side::New(Saved.Input.clone());
    let mut Output = Side::New(Saved.Output.clone());
    let mut LastEnum: Option<Instant> = None;

    while !State.Stopped() {
        if let Some((RequestedInput, RequestedOutput)) = State.DeviceRequest.lock().unwrap().take() {
            if let Some(Name) = RequestedInput.filter(|Name| !Name.is_empty()) {
                Input.Request(Name.clone());
                Saved.Input = Some(Name);
            }
            if let Some(Name) = RequestedOutput.filter(|Name| !Name.is_empty()) {
                Output.Request(Name.clone());
                Saved.Output = Some(Name);
            }
            Prefs::Save(&Saved);
        }

        for Current in [&mut Input, &mut Output] {
            if Current.Failed.swap(false, Ordering::Relaxed) {
                log::info!("voice: audio stream error, rebuilding");
                Current.Stream = None;
                Current.Rebuild = true;
            }
        }

        if LastEnum.map_or(true, |Last| Last.elapsed() >= EnumEvery) {
            LastEnum = Some(Instant::now());
            let Inputs = Names(AudioHost.input_devices().ok());
            let Outputs = Names(AudioHost.output_devices().ok());
            let DefaultInput = AudioHost.default_input_device().and_then(|Found| Found.name().ok()).unwrap_or_default();
            let DefaultOutput = AudioHost.default_output_device().and_then(|Found| Found.name().ok()).unwrap_or_default();
            for (Current, SystemDefault, List) in
                [(&mut Input, &DefaultInput, &Inputs), (&mut Output, &DefaultOutput, &Outputs)]
            {
                if Current.Stream.is_none() {
                    continue;
                }
                let FollowDefault =
                    Current.Want == DefaultDevice && !SystemDefault.is_empty() && *SystemDefault != Current.ActiveName;
                let PickedBack =
                    Current.Want != DefaultDevice && Current.ActiveName != Current.Want && List.contains(&Current.Want);
                if FollowDefault || PickedBack {
                    Current.Rebuild = true;
                    Current.LastAttempt = None;
                }
            }
            let InputList: Vec<String> = std::iter::once(DefaultDevice.to_string()).chain(Inputs).collect();
            let OutputList: Vec<String> = std::iter::once(DefaultDevice.to_string()).chain(Outputs).collect();
            let Changed = {
                let Status = State.Status.lock().unwrap();
                Status.Inputs != InputList || Status.Outputs != OutputList
            };
            if Changed {
                State.SetStatus(|Status| {
                    Status.Inputs = InputList;
                    Status.Outputs = OutputList;
                });
            }
        }

        if Input.Due() {
            Input.LastAttempt = Some(Instant::now());
            Input.Stream = None;
            let Want = Input.Want.clone();
            match OpenInput(&AudioHost, &Want, Input.Failed.clone()) {
                Ok((Opened, Feed, Name)) => {
                    let _ = InputSender.send(Feed);
                    Input.Stream = Some(Opened);
                    Input.ActiveName = Name;
                    Input.Rebuild = false;
                    State.SetStatus(|Status| {
                        Status.Input = Want;
                        if Status.Err.starts_with("Microphone") {
                            Status.Err.clear();
                        }
                    });
                }
                Err(Error) => {
                    log::warn!("voice: input '{Want}' failed: {Error:#}");
                    State.SetStatus(|Status| {
                        Status.Input = Want;
                        Status.Err = "Microphone unavailable".into();
                    });
                }
            }
        }

        if Output.Due() {
            Output.LastAttempt = Some(Instant::now());
            Output.Stream = None;
            let Want = Output.Want.clone();
            match OpenOutput(&AudioHost, &Want, Output.Failed.clone()) {
                Ok((Opened, Feed, Name)) => {
                    let _ = OutputSender.send(Feed);
                    Output.Stream = Some(Opened);
                    Output.ActiveName = Name;
                    Output.Rebuild = false;
                    State.SetStatus(|Status| {
                        Status.Output = Want;
                        if Status.Err.starts_with("Speakers") {
                            Status.Err.clear();
                        }
                    });
                }
                Err(Error) => {
                    log::warn!("voice: output '{Want}' failed: {Error:#}");
                    State.SetStatus(|Status| {
                        Status.Output = Want;
                        Status.Err = "Speakers unavailable".into();
                    });
                }
            }
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn Names(Devices: Option<impl Iterator<Item = Device>>) -> Vec<String> {
    let mut Out: Vec<String> = Vec::new();
    for Name in Devices.into_iter().flatten().filter_map(|Found| Found.name().ok()) {
        if !Name.is_empty() && !Out.contains(&Name) {
            Out.push(Name);
        }
    }
    Out.truncate(15);
    Out
}

fn Find(AudioHost: &Host, Want: &str, IsInput: bool) -> Option<Device> {
    if Want != DefaultDevice {
        let List = if IsInput { AudioHost.input_devices().ok() } else { AudioHost.output_devices().ok() };
        if let Some(Found) = List.into_iter().flatten().find(|Found| Found.name().map_or(false, |Name| Name == Want)) {
            return Some(Found);
        }
    }
    if IsInput {
        AudioHost.default_input_device()
    } else {
        AudioHost.default_output_device()
    }
}

fn OpenInput(AudioHost: &Host, Want: &str, Failed: Arc<AtomicBool>) -> anyhow::Result<(Stream, InputFeed, String)> {
    let Found = Find(AudioHost, Want, true).ok_or_else(|| anyhow::anyhow!("no input device"))?;
    let Name = Found.name().unwrap_or_default();
    let Supported = Found.default_input_config()?;
    let Format = Supported.sample_format();
    let Config: StreamConfig = Supported.config();
    let Rate = Config.sample_rate.0;
    let (Producer, Consumer) = HeapRb::<f32>::new(Rate as usize).split();
    let Opened = match Format {
        SampleFormat::F32 => InputStream::<f32>(&Found, &Config, Producer, Failed)?,
        SampleFormat::I16 => InputStream::<i16>(&Found, &Config, Producer, Failed)?,
        SampleFormat::U16 => InputStream::<u16>(&Found, &Config, Producer, Failed)?,
        SampleFormat::I32 => InputStream::<i32>(&Found, &Config, Producer, Failed)?,
        Other => anyhow::bail!("unsupported input format {Other:?}"),
    };
    Opened.play()?;
    log::info!("voice: mic '{Name}' {Rate} Hz x{}", Config.channels);
    Ok((Opened, InputFeed { Consumer, Rate }, Name))
}

fn InputStream<T>(
    Found: &Device,
    Config: &StreamConfig,
    mut Producer: HeapProd<f32>,
    Failed: Arc<AtomicBool>,
) -> anyhow::Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let Channels = Config.channels.max(1) as usize;
    let mut Mono: Vec<f32> = Vec::with_capacity(4096);
    Ok(Found.build_input_stream(
        Config,
        move |Data: &[T], _: &cpal::InputCallbackInfo| {
            Mono.clear();
            for Frame in Data.chunks(Channels) {
                Mono.push(Frame.iter().map(|&Sample| f32::from_sample(Sample)).sum::<f32>() / Channels as f32);
            }
            Producer.push_slice(&Mono);
        },
        move |Error| {
            log::warn!("voice: input stream: {Error}");
            Failed.store(true, Ordering::Relaxed);
        },
        None,
    )?)
}

fn OpenOutput(AudioHost: &Host, Want: &str, Failed: Arc<AtomicBool>) -> anyhow::Result<(Stream, OutputFeed, String)> {
    let Found = Find(AudioHost, Want, false).ok_or_else(|| anyhow::anyhow!("no output device"))?;
    let Name = Found.name().unwrap_or_default();
    let Supported = Found.default_output_config()?;
    let Format = Supported.sample_format();
    let Config: StreamConfig = Supported.config();
    let Rate = Config.sample_rate.0;
    let (Producer, Consumer) = HeapRb::<f32>::new(Rate as usize).split();
    let Opened = match Format {
        SampleFormat::F32 => OutputStream::<f32>(&Found, &Config, Consumer, Failed)?,
        SampleFormat::I16 => OutputStream::<i16>(&Found, &Config, Consumer, Failed)?,
        SampleFormat::U16 => OutputStream::<u16>(&Found, &Config, Consumer, Failed)?,
        SampleFormat::I32 => OutputStream::<i32>(&Found, &Config, Consumer, Failed)?,
        Other => anyhow::bail!("unsupported output format {Other:?}"),
    };
    Opened.play()?;
    log::info!("voice: output '{Name}' {Rate} Hz x{}", Config.channels);
    Ok((Opened, OutputFeed { Producer, Rate }, Name))
}

fn OutputStream<T>(
    Found: &Device,
    Config: &StreamConfig,
    mut Consumer: HeapCons<f32>,
    Failed: Arc<AtomicBool>,
) -> anyhow::Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let Channels = Config.channels.max(1) as usize;
    Ok(Found.build_output_stream(
        Config,
        move |Data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for Frame in Data.chunks_mut(Channels) {
                let mut Pair = [0.0f32; 2];
                if Consumer.pop_slice(&mut Pair) < 2 {
                    Pair = [0.0, 0.0];
                }
                if Channels == 1 {
                    Frame[0] = T::from_sample((Pair[0] + Pair[1]) * 0.5);
                } else {
                    for (Index, Sample) in Frame.iter_mut().enumerate() {
                        *Sample = T::from_sample(match Index {
                            0 => Pair[0],
                            1 => Pair[1],
                            _ => 0.0,
                        });
                    }
                }
            }
        },
        move |Error| {
            log::warn!("voice: output stream: {Error}");
            Failed.store(true, Ordering::Relaxed);
        },
        None,
    )?)
}
