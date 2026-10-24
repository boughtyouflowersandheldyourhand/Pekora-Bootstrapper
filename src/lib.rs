#![allow(non_snake_case, non_upper_case_globals)]

pub mod Config;
pub mod Fps;
pub mod FpsMem;
pub mod Installer;
pub mod Launcher;
pub mod Rpc;
pub mod Ui;
pub mod Voice;

use std::sync::{Arc, Mutex};

use anyhow::Result;
use eframe::egui;

const WindowWidth: f32 = 480.0;
const WindowHeight: f32 = 250.0;

pub fn Run(Args: Vec<String>, ForceInstallOnly: bool) -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    match Args.first().map(String::as_str) {
        Some("--rpc-play") => return Rpc::WatchAndPlay(&Args[1..]),
        Some("--voice-watch") => return Rpc::WatchVoice(&Args[1..]),
        Some(Fps::FpsFlag) => return Fps::RunWindow(),
        _ => {}
    }

    let mut Input = Launcher::ParseArgv(&Args);
    if ForceInstallOnly {
        Input.InstallOnly = true;
        Input.PlayUrl = None;
        Input.LocalFile = None;
    }

    if Input.Kind == Config::Kind::Player && Input.PlayUrl.is_some() && Launcher::ClientAlreadyRunning()? {
        return Ok(());
    }

    if Installer::SelfUpdateIfNeeded(&Args) {
        return Ok(());
    }

    let Title = if Input.InstallOnly { "Pekora Installer" } else { Input.Kind.Label() }.to_string();

    let State = Arc::new(Mutex::new(Ui::BootstrapState::New(
        Input.PlayUrl,
        Input.LocalFile,
        Input.Client,
        Input.Kind,
        Input.InstallOnly,
    )));

    {
        let State = Arc::clone(&State);
        std::thread::spawn(move || {
            if let Err(Error) = Launcher::RunPipeline(&State) {
                log::error!("launch pipeline failed: {Error:#}");
                State.lock().unwrap().Fail(format!("Launch failed: {Error:#}"));
            }
        });
    }

    let mut Viewport = egui::ViewportBuilder::default()
        .with_inner_size([WindowWidth, WindowHeight])
        .with_min_inner_size([WindowWidth, WindowHeight])
        .with_max_inner_size([WindowWidth, WindowHeight])
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true)
        .with_title(&Title);

    if let Some((Rgba, Width, Height)) = Ui::AppIconRgba() {
        Viewport = Viewport.with_icon(egui::IconData { rgba: Rgba, width: Width, height: Height });
    }

    let Options = eframe::NativeOptions { viewport: Viewport, centered: true, ..Default::default() };

    eframe::run_native(
        "Pekora",
        Options,
        Box::new(move |Context| {
            egui_extras::install_image_loaders(&Context.egui_ctx);
            Box::new(Ui::BootstrapApp::New(Context, State))
        }),
    )
    .map_err(|Error| anyhow::anyhow!("eframe: {Error}"))?;

    Ok(())
}
