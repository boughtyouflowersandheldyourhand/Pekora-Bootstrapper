use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Pos2, Rect, Rounding, Sense, Stroke, TextureHandle, TextureOptions,
    Vec2,
};

use crate::Config::Kind;

pub const LogoBytes: &[u8] = include_bytes!("../logo.png");
pub const AppIconBytes: &[u8] = include_bytes!("../icon.png");

const CardBackground: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
const ProgressFill: Color32 = Color32::from_rgb(0x40, 0xC1, 0x40);
const ProgressTrack: Color32 = Color32::from_rgb(0xEC, 0xEC, 0xEC);
const TextPrimary: Color32 = Color32::from_rgb(0x19, 0x19, 0x19);
const TextMuted: Color32 = Color32::from_rgb(0x9A, 0x9A, 0x9A);
const CloseIdle: Color32 = Color32::from_rgb(0xC0, 0xC0, 0xC0);
const CloseHover: Color32 = Color32::from_rgb(0x30, 0x30, 0x30);
const ErrorText: Color32 = Color32::from_rgb(0xE0, 0x31, 0x31);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Configuring,
    Starting,
    Done,
    Idle,
    Failed,
}

pub struct BootstrapState {
    pub Phase: Phase,
    pub Status: String,
    pub PlayUrl: Option<String>,
    pub LocalFile: Option<String>,
    pub Client: String,
    pub Kind: Kind,
    pub InstallOnly: bool,
    pub RequestClose: bool,
    pub Error: Option<String>,
    pub Progress: Option<f32>,
}

pub type SharedState = Arc<Mutex<BootstrapState>>;

impl BootstrapState {
    pub fn New(PlayUrl: Option<String>, LocalFile: Option<String>, Client: String, Kind: Kind, InstallOnly: bool) -> Self {
        Self {
            Phase: Phase::Connecting,
            Status: if InstallOnly { "Getting things ready" } else { "Connecting" }.to_string(),
            PlayUrl,
            LocalFile,
            Client,
            Kind,
            InstallOnly,
            RequestClose: false,
            Error: None,
            Progress: None,
        }
    }

    pub fn Fail(&mut self, Message: impl Into<String>) {
        let Message = Message.into();
        self.Phase = Phase::Failed;
        self.Status = Message.clone();
        self.Error = Some(Message);
    }
}

pub fn SetPhase(State: &SharedState, NewPhase: Phase, Status: &str, Progress: Option<f32>) {
    let mut Locked = State.lock().unwrap();
    Locked.Phase = NewPhase;
    Locked.Status = Status.to_string();
    if let Some(Value) = Progress {
        Locked.Progress = Some(Value.clamp(0.0, 1.0));
    }
}

pub fn SetProgress(State: &SharedState, Value: f32) {
    State.lock().unwrap().Progress = Some(Value.clamp(0.0, 1.0));
}

pub struct BootstrapApp {
    State: SharedState,
    StartedAt: Instant,
    Logo: Option<TextureHandle>,
    LogoAspect: f32,
}

impl BootstrapApp {
    pub fn New(Context: &eframe::CreationContext<'_>, State: SharedState) -> Self {
        let (Logo, LogoAspect) = match DecodeLogo(&Context.egui_ctx) {
            Ok((Texture, Width, Height)) => (Some(Texture), Width as f32 / (Height as f32).max(1.0)),
            Err(Error) => {
                log::warn!("failed to load embedded logo: {Error:#}");
                (None, 4.0)
            }
        };
        Self { State, StartedAt: Instant::now(), Logo, LogoAspect }
    }

    fn DrawLogo(&self, Painter: &egui::Painter, Card: Rect) {
        let TargetHeight = 74.0;
        let CenterX = Card.center().x;
        let TopY = Card.top() + 40.0;
        match &self.Logo {
            Some(Texture) => {
                let TargetWidth = (TargetHeight * self.LogoAspect).min(Card.width() - 96.0);
                Painter.image(
                    Texture.id(),
                    Rect::from_min_size(
                        Pos2::new(CenterX - TargetWidth * 0.5, TopY),
                        Vec2::new(TargetWidth, TargetWidth / self.LogoAspect),
                    ),
                    Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                Painter.text(
                    Pos2::new(CenterX, TopY + TargetHeight * 0.5),
                    Align2::CENTER_CENTER,
                    "PEKORA",
                    FontId::proportional(40.0),
                    TextPrimary,
                );
            }
        }
    }
}

fn DecodeLogo(Context: &egui::Context) -> anyhow::Result<(TextureHandle, u32, u32)> {
    let Image = image::load_from_memory(LogoBytes)?.to_rgba8();
    let (Width, Height) = (Image.width(), Image.height());
    let Pixels = ColorImage::from_rgba_unmultiplied([Width as usize, Height as usize], &Image.into_raw());
    Ok((Context.load_texture("pekora_logo", Pixels, TextureOptions::LINEAR), Width, Height))
}

pub fn AppIconRgba() -> Option<(Vec<u8>, u32, u32)> {
    match image::load_from_memory(AppIconBytes) {
        Ok(Image) => {
            let Image = Image.to_rgba8();
            let (Width, Height) = (Image.width(), Image.height());
            Some((Image.into_raw(), Width, Height))
        }
        Err(Error) => {
            log::warn!("failed to decode embedded icon for window: {Error:#}");
            None
        }
    }
}

impl eframe::App for BootstrapApp {
    fn clear_color(&self, _Visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, Context: &egui::Context, _Frame: &mut eframe::Frame) {
        Context.request_repaint_after(std::time::Duration::from_millis(16));

        let (CurrentPhase, Status, RequestClose, Error, Progress) = {
            let Locked = self.State.lock().unwrap();
            (Locked.Phase, Locked.Status.clone(), Locked.RequestClose, Locked.Error.clone(), Locked.Progress)
        };
        if RequestClose {
            Context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        egui::CentralPanel::default().frame(egui::Frame::none()).show(Context, |Ui| {
            let Card = Ui.max_rect();
            Ui.painter().rect_filled(Card, Rounding::same(8.0), CardBackground);

            let CloseRect = CloseButtonRect(Card);
            let Drag = Ui.interact(Card, egui::Id::new("winmove"), Sense::click_and_drag());
            if Drag.drag_started() {
                let OverClose = Ui.ctx().pointer_interact_pos().map_or(false, |Pos| CloseRect.contains(Pos));
                if !OverClose {
                    Ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
            }

            self.DrawLogo(Ui.painter(), Card);
            if CurrentPhase == Phase::Failed {
                if let Some(Error) = &Error {
                    DrawError(Ui.painter(), Card, &Status, Error);
                }
            } else {
                DrawStatus(Ui.painter(), Card, &Status);
                DrawProgress(Ui.painter(), Card, Progress, self.StartedAt.elapsed().as_secs_f32());
            }
            DrawClose(Ui, CloseRect);
        });
    }
}

fn CloseButtonRect(Card: Rect) -> Rect {
    let Size = 30.0;
    Rect::from_min_size(Pos2::new(Card.right() - Size - 10.0, Card.top() + 10.0), Vec2::splat(Size))
}

fn DrawClose(Ui: &mut egui::Ui, Button: Rect) {
    let Response = Ui.interact(Button, egui::Id::new("close_btn"), Sense::click());
    let Hovered = Response.hovered();
    let Line = Stroke::new(if Hovered { 2.0 } else { 1.6 }, if Hovered { CloseHover } else { CloseIdle });
    let Center = Button.center();
    let Radius = 5.0;
    let Painter = Ui.painter();
    Painter.line_segment(
        [Pos2::new(Center.x - Radius, Center.y - Radius), Pos2::new(Center.x + Radius, Center.y + Radius)],
        Line,
    );
    Painter.line_segment(
        [Pos2::new(Center.x + Radius, Center.y - Radius), Pos2::new(Center.x - Radius, Center.y + Radius)],
        Line,
    );
    if Response.clicked() {
        Ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

fn DrawStatus(Painter: &egui::Painter, Card: Rect, Status: &str) {
    Painter.text(
        Pos2::new(Card.center().x, Card.top() + 158.0),
        Align2::CENTER_CENTER,
        Status,
        FontId::proportional(15.0),
        TextPrimary,
    );
}

fn DrawProgress(Painter: &egui::Painter, Card: Rect, Progress: Option<f32>, Time: f32) {
    let TrackHeight = 6.0;
    let MarginX = 46.0;
    let TrackY = Card.top() + 198.0;
    let Track = Rect::from_min_max(
        Pos2::new(Card.left() + MarginX, TrackY),
        Pos2::new(Card.right() - MarginX, TrackY + TrackHeight),
    );
    let Corner = Rounding::same(TrackHeight * 0.5);
    Painter.rect_filled(Track, Corner, ProgressTrack);

    let Bar = match Progress {
        Some(Fraction) => {
            let FillWidth = (Track.width() * Fraction.clamp(0.0, 1.0)).max(TrackHeight);
            Rect::from_min_size(Track.min, Vec2::new(FillWidth, TrackHeight))
        }
        None => {
            let SegmentWidth = Track.width() * 0.28;
            let Cycle = ((Time * 1.5).sin() * 0.5 + 0.5).clamp(0.0, 1.0);
            let Left = Track.left() + (Track.width() - SegmentWidth) * Cycle;
            Rect::from_min_size(Pos2::new(Left, Track.top()), Vec2::new(SegmentWidth, TrackHeight))
        }
    };
    Painter.rect_filled(Bar, Corner, ProgressFill);
}

fn DrawError(Painter: &egui::Painter, Card: Rect, Status: &str, Error: &str) {
    Painter.text(
        Pos2::new(Card.center().x, Card.top() + 148.0),
        Align2::CENTER_CENTER,
        Status,
        FontId::proportional(15.0),
        ErrorText,
    );
    Painter.text(
        Pos2::new(Card.center().x, Card.top() + 172.0),
        Align2::CENTER_CENTER,
        if Error.len() > 90 { &Error[..90] } else { Error },
        FontId::proportional(11.0),
        TextMuted,
    );
}
