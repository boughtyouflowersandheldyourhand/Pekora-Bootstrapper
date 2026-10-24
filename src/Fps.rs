use std::cell::RefCell;
use std::fs;
use std::iter::once;
use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use std::process::Command;
use std::ptr::null;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::{Context, Result};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CheckMenuRadioItem, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, FindWindowExW, GetCursorPos, GetMessageW, LoadIconW,
    PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer, TrackPopupMenu,
    TranslateMessage, MF_BYCOMMAND, MF_SEPARATOR, MF_STRING, MSG, TPM_RIGHTBUTTON, WM_APP, WM_CLOSE,
    WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_OVERLAPPED,
};

use crate::Config;
use crate::FpsMem::Engine;

pub const FpsFlag: &str = "--fps";
const Unlimited: i64 = 9999;
const FpsOptions: [(&str, i64); 8] = [
    ("60", 60),
    ("75", 75),
    ("120", 120),
    ("144", 144),
    ("165", 165),
    ("240", 240),
    ("360", 360),
    ("Unlimited", Unlimited),
];
const WmTrayIcon: u32 = WM_APP + 1;
const MenuBase: u32 = 1000;
const MenuExit: u32 = 2000;
const ClientWaitSeconds: u32 = 60;

static SeenClient: AtomicBool = AtomicBool::new(false);
static WaitedSeconds: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static ClientEngine: RefCell<Engine> = RefCell::new(Engine::New());
}

fn StateFile() -> Result<PathBuf> {
    Ok(Config::StateDir()?.join("fps"))
}

pub fn SavedFps() -> i64 {
    StateFile()
        .ok()
        .and_then(|Path| fs::read_to_string(Path).ok())
        .and_then(|Value| Value.trim().parse().ok())
        .unwrap_or(Unlimited)
}

fn SaveFps(Fps: i64) -> Result<()> {
    let File = StateFile()?;
    fs::create_dir_all(File.parent().unwrap())?;
    fs::write(&File, Fps.to_string()).with_context(|| format!("writing {}", File.display()))
}

fn Apply(Fps: i64) -> bool {
    ClientEngine.with(|Engine| Engine.borrow_mut().Apply(Fps))
}

pub fn SpawnWindow() -> Result<()> {
    Command::new(std::env::current_exe()?).arg(FpsFlag).spawn()?;
    Ok(())
}

fn Wide(Text: &str) -> Vec<u16> {
    Text.encode_utf16().chain(once(0)).collect()
}

pub fn RunWindow() -> Result<()> {
    unsafe {
        let Instance = GetModuleHandleW(null());
        let ClassName = Wide("PekoraFpsTray");
        CloseOtherTrays(&ClassName);
        let mut Class: WNDCLASSW = zeroed();
        Class.lpfnWndProc = Some(WindowProc);
        Class.hInstance = Instance;
        Class.lpszClassName = ClassName.as_ptr();
        RegisterClassW(&Class);

        let Window = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            ClassName.as_ptr(),
            Wide("Pekora FPS").as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            0,
            0,
            Instance,
            null(),
        );

        let mut Icon: NOTIFYICONDATAW = zeroed();
        Icon.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        Icon.hWnd = Window;
        Icon.uID = 1;
        Icon.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        Icon.uCallbackMessage = WmTrayIcon;
        Icon.hIcon = LoadIconW(Instance, 1 as *const u16);
        let Tip = Wide("Pekora FPS");
        Icon.szTip[..Tip.len()].copy_from_slice(&Tip);
        Shell_NotifyIconW(NIM_ADD, &Icon);

        SetTimer(Window, 1, 1000, None);

        let mut Message: MSG = zeroed();
        while GetMessageW(&mut Message, 0, 0, 0) > 0 {
            TranslateMessage(&Message);
            DispatchMessageW(&Message);
        }

        Shell_NotifyIconW(NIM_DELETE, &Icon);
    }
    Ok(())
}

unsafe fn CloseOtherTrays(ClassName: &[u16]) {
    let mut Window = FindWindowExW(0, 0, ClassName.as_ptr(), null());
    while Window != 0 {
        PostMessageW(Window, WM_CLOSE, 0, 0);
        Window = FindWindowExW(0, Window, ClassName.as_ptr(), null());
    }
}

unsafe extern "system" fn WindowProc(Window: HWND, Message: u32, WordParam: WPARAM, LongParam: LPARAM) -> LRESULT {
    match Message {
        WmTrayIcon => {
            let Event = (LongParam & 0xffff) as u32;
            if Event == WM_RBUTTONUP || Event == WM_LBUTTONUP {
                ShowMenu(Window);
            }
            0
        }
        WM_COMMAND => {
            HandleCommand(Window, (WordParam & 0xffff) as u32);
            0
        }
        WM_TIMER => {
            if Apply(SavedFps()) {
                SeenClient.store(true, Ordering::Relaxed);
            } else if SeenClient.load(Ordering::Relaxed)
                || WaitedSeconds.fetch_add(1, Ordering::Relaxed) >= ClientWaitSeconds
            {
                DestroyWindow(Window);
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(Window, Message, WordParam, LongParam),
    }
}

unsafe fn ShowMenu(Window: HWND) {
    let Menu = CreatePopupMenu();
    let Current = SavedFps();
    let mut Checked = MenuBase;
    for (Index, (Label, Value)) in FpsOptions.iter().enumerate() {
        let Id = MenuBase + Index as u32;
        AppendMenuW(Menu, MF_STRING, Id as usize, Wide(Label).as_ptr());
        if *Value == Current {
            Checked = Id;
        }
    }
    CheckMenuRadioItem(Menu, MenuBase, MenuBase + FpsOptions.len() as u32 - 1, Checked, MF_BYCOMMAND);
    AppendMenuW(Menu, MF_SEPARATOR, 0, null());
    AppendMenuW(Menu, MF_STRING, MenuExit as usize, Wide("Exit").as_ptr());

    let mut Point: POINT = zeroed();
    GetCursorPos(&mut Point);
    SetForegroundWindow(Window);
    TrackPopupMenu(Menu, TPM_RIGHTBUTTON, Point.x, Point.y, 0, Window, null());
    DestroyMenu(Menu);
}

unsafe fn HandleCommand(Window: HWND, Id: u32) {
    if Id == MenuExit {
        DestroyWindow(Window);
    } else if let Some((_, Value)) = Id.checked_sub(MenuBase).and_then(|Index| FpsOptions.get(Index as usize)) {
        let _ = SaveFps(*Value);
        Apply(*Value);
    }
}
