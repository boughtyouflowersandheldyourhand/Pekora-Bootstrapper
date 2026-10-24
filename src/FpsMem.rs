use std::fs;
use std::mem::{size_of, zeroed};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, MODULEENTRY32W, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};

use crate::Launcher;

const Primary: [u8; 17] = [0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x10, 0x56, 0xE8, 0, 0, 0, 0, 0x8B, 0xF0, 0x8D, 0x45, 0xF0];
const PrimaryMask: [bool; 17] =
    [true, true, true, true, true, true, true, true, false, false, false, false, true, true, true, true, true];
const Secondary: [u8; 8] = [0xA1, 0, 0, 0, 0, 0x8B, 0x4D, 0xF4];
const SecondaryMask: [bool; 8] = [true, false, false, false, false, true, true, true];
const SearchOffset: u32 = 0x100;
const SearchLength: usize = 0x100;

fn FrameDelayBytes(Fps: i64) -> [u8; 8] {
    (if Fps >= 1000 { 1.0 / 10000.0 } else { 1.0 / Fps as f64 }).to_le_bytes()
}

fn MaskedFind(Haystack: &[u8], Pattern: &[u8], Mask: &[bool]) -> Option<usize> {
    if Haystack.len() < Pattern.len() {
        return None;
    }
    (0..=Haystack.len() - Pattern.len())
        .find(|Start| (0..Pattern.len()).all(|Index| !Mask[Index] || Haystack[Start + Index] == Pattern[Index]))
}

fn LeU32(Bytes: &[u8], At: usize) -> Option<u32> {
    Some(u32::from_le_bytes(Bytes.get(At..At + 4)?.try_into().ok()?))
}

fn FindClient() -> Option<u32> {
    let Root = Launcher::InstallRootLowercase().ok()?;
    Launcher::PekoraClientPids(&Root).ok()?.first().copied()
}

fn ModuleBase(Pid: u32) -> Option<(u32, String)> {
    unsafe {
        let mut Snapshot = INVALID_HANDLE_VALUE;
        for _ in 0..5 {
            Snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, Pid);
            if Snapshot != INVALID_HANDLE_VALUE {
                break;
            }
        }
        if Snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut Entry: MODULEENTRY32W = zeroed();
        Entry.dwSize = size_of::<MODULEENTRY32W>() as u32;
        let Found = Module32FirstW(Snapshot, &mut Entry) != 0;
        CloseHandle(Snapshot);
        if !Found {
            return None;
        }
        let End = Entry.szExePath.iter().position(|&Char| Char == 0).unwrap_or(Entry.szExePath.len());
        Some((Entry.modBaseAddr as usize as u32, String::from_utf16_lossy(&Entry.szExePath[..End])))
    }
}

fn PrimaryRva(Exe: &[u8]) -> Option<u32> {
    let Pe = LeU32(Exe, 0x3C)? as usize;
    let Sections = u16::from_le_bytes([*Exe.get(Pe + 6)?, *Exe.get(Pe + 7)?]) as usize;
    let Optional = u16::from_le_bytes([*Exe.get(Pe + 20)?, *Exe.get(Pe + 21)?]) as usize;
    let Table = Pe + 24 + Optional;
    for Index in 0..Sections {
        let Header = Table + Index * 40;
        if !Exe.get(Header..Header + 8)?.starts_with(b".text") {
            continue;
        }
        let VirtualAddress = LeU32(Exe, Header + 12)?;
        let RawSize = LeU32(Exe, Header + 16)? as usize;
        let RawPointer = LeU32(Exe, Header + 20)? as usize;
        let Section = Exe.get(RawPointer..(RawPointer + RawSize).min(Exe.len()))?;
        return Some(VirtualAddress + MaskedFind(Section, &Primary, &PrimaryMask)? as u32);
    }
    None
}

pub struct Engine {
    Session: Option<Session>,
    Scanned: Option<(String, Option<u32>)>,
}

struct Session {
    Handle: HANDLE,
    Pid: u32,
    Slot: u32,
    Scheduler: u32,
    FrameDelay: u32,
}

impl Engine {
    pub fn New() -> Self {
        Engine { Session: None, Scanned: None }
    }

    pub fn Apply(&mut self, Fps: i64) -> bool {
        let Some(Pid) = FindClient() else {
            self.Close();
            return false;
        };
        if self.Session.as_ref().map(|Active| Active.Pid) != Some(Pid) {
            self.Close();
            self.Session = self.Open(Pid);
        }
        if let Some(Active) = self.Session.as_mut() {
            if !Active.Apply(Fps) {
                self.Close();
            }
        }
        true
    }

    fn Open(&mut self, Pid: u32) -> Option<Session> {
        let (Base, Path) = ModuleBase(Pid)?;
        if self.Scanned.as_ref().map(|(Scanned, _)| Scanned) != Some(&Path) {
            let Rva = fs::read(&Path).ok().and_then(|Exe| PrimaryRva(&Exe));
            self.Scanned = Some((Path, Rva));
        }
        Session::Open(Pid, Base, self.Scanned.as_ref()?.1?)
    }

    fn Close(&mut self) {
        if let Some(Active) = self.Session.take() {
            unsafe {
                CloseHandle(Active.Handle);
            }
        }
    }
}

impl Session {
    fn Open(Pid: u32, Base: u32, Rva: u32) -> Option<Session> {
        let Handle = unsafe {
            OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION, 0, Pid)
        };
        if Handle == 0 {
            return None;
        }
        let mut Opened = Session { Handle, Pid, Slot: 0, Scheduler: 0, FrameDelay: 0 };
        match Opened.ResolveSlot(Base, Rva) {
            Some(Slot) => {
                Opened.Slot = Slot;
                Some(Opened)
            }
            None => {
                unsafe {
                    CloseHandle(Handle);
                }
                None
            }
        }
    }

    fn Read(&self, Address: u32, Length: usize) -> Option<Vec<u8>> {
        let mut Buffer = vec![0u8; Length];
        let mut Read = 0usize;
        let Succeeded = unsafe {
            ReadProcessMemory(self.Handle, Address as usize as *const _, Buffer.as_mut_ptr() as *mut _, Length, &mut Read)
        };
        (Succeeded != 0 && Read == Length).then_some(Buffer)
    }

    fn Write(&self, Address: u32, Data: &[u8]) -> bool {
        let mut Written = 0usize;
        let Succeeded = unsafe {
            WriteProcessMemory(self.Handle, Address as usize as *const _, Data.as_ptr() as *const _, Data.len(), &mut Written)
        };
        Succeeded != 0 && Written == Data.len()
    }

    fn ResolveSlot(&self, Base: u32, Rva: u32) -> Option<u32> {
        let PrimaryAddress = Base.wrapping_add(Rva);
        let Head = self.Read(PrimaryAddress, Primary.len())?;
        if MaskedFind(&Head, &Primary, &PrimaryMask) != Some(0) {
            return None;
        }
        let Displacement = LeU32(&Head, 8)?;
        let Function = PrimaryAddress.wrapping_add(12).wrapping_add(Displacement);
        let Body = self.Read(Function, 0x100)?;
        LeU32(&Body, MaskedFind(&Body, &Secondary, &SecondaryMask)? + 1)
    }

    fn FindFrameDelay(&self, Scheduler: u32) -> Option<u32> {
        let Search = Scheduler.wrapping_add(SearchOffset);
        let Data = self.Read(Search, SearchLength)?;
        let Target = (1.0f64 / 60.0).to_le_bytes();
        let mut Matches = (0..=Data.len() - 8).step_by(4).filter(|Offset| Data[*Offset..*Offset + 8] == Target);
        match (Matches.next(), Matches.next()) {
            (Some(Offset), None) => Some(Search.wrapping_add(Offset as u32)),
            _ => None,
        }
    }

    fn Apply(&mut self, Fps: i64) -> bool {
        let Some(Scheduler) = self.Read(self.Slot, 4).and_then(|Bytes| LeU32(&Bytes, 0)) else {
            return false;
        };
        if Scheduler == 0 {
            self.FrameDelay = 0;
            return true;
        }
        if Scheduler != self.Scheduler {
            self.Scheduler = Scheduler;
            self.FrameDelay = 0;
        }
        if self.FrameDelay == 0 {
            match self.FindFrameDelay(Scheduler) {
                Some(Address) => self.FrameDelay = Address,
                None => return true,
            }
        }
        let Desired = FrameDelayBytes(Fps);
        match self.Read(self.FrameDelay, 8) {
            Some(Current) if Current != Desired => {
                let _ = self.Write(self.FrameDelay, &Desired);
            }
            Some(_) => {}
            None => self.FrameDelay = 0,
        }
        true
    }
}
