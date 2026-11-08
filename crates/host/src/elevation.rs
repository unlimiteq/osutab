//! Running other programs without administrator rights, even when this one has them. OpenTabletDriver
//! says it must never run as administrator: its settings window can't reach a daemon running as
//! administrator, and its plugin manager goes missing.

use std::io;
use std::path::Path;
use std::process::{Child, Command};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A program this one started, either way.
pub enum Launched {
    Child(Child),
    Handle(HANDLE),
}

// The handle is only used to stop the process.
unsafe impl Send for Launched {}

impl Launched {
    pub fn kill(&mut self) {
        match self {
            Launched::Child(child) => {
                let _ = child.kill();
            }
            Launched::Handle(handle) => unsafe {
                TerminateProcess(*handle, 0);
            },
        }
    }
}

impl Drop for Launched {
    fn drop(&mut self) {
        if let Launched::Handle(handle) = self {
            unsafe { CloseHandle(*handle) };
        }
    }
}

/// True if a process's token is elevated. `None` when Windows won't say, which from a program
/// without administrator rights usually means the other one has them.
fn token_is_elevated(process: HANDLE) -> Option<bool> {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = 0u32;
        let ok = GetTokenInformation(token, TokenElevation, &mut elevation as *mut _ as *mut _, size_of::<TOKEN_ELEVATION>() as u32, &mut size);
        CloseHandle(token);
        (ok != 0).then_some(elevation.TokenIsElevated != 0)
    }
}

pub fn this_program_is_elevated() -> bool {
    token_is_elevated(unsafe { GetCurrentProcess() }).unwrap_or(false)
}

/// Whether another process runs as administrator, as far as this program can tell.
pub fn process_is_elevated(pid: u32) -> Option<bool> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let elevated = token_is_elevated(process);
        CloseHandle(process);
        match elevated {
            Some(answer) => Some(answer),
            // Allowed to see the process but not its token: only a lower one is turned away like that.
            None if !this_program_is_elevated() => Some(true),
            None => None,
        }
    }
}

/// Starts a program without administrator rights. When this program has them, the new one gets the
/// desktop's own rights instead, the ones Explorer runs with.
pub fn spawn_unelevated(exe: &Path, dir: &Path, hidden: bool) -> io::Result<Launched> {
    if !this_program_is_elevated() {
        let mut command = Command::new(exe);
        command.current_dir(dir);
        if hidden {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        return command.spawn().map(Launched::Child);
    }
    unsafe {
        let shell = GetShellWindow();
        if shell.is_null() {
            return Err(io::Error::other("there's no desktop to borrow ordinary rights from"));
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(shell, &mut pid);
        let explorer = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid);
        if explorer.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut token: HANDLE = null_mut();
        let opened = OpenProcessToken(explorer, TOKEN_DUPLICATE, &mut token);
        CloseHandle(explorer);
        if opened == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut primary: HANDLE = null_mut();
        let access = TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT | TOKEN_ADJUST_SESSIONID;
        let duplicated = DuplicateTokenEx(token, access, null(), SecurityImpersonation, TokenPrimary, &mut primary);
        CloseHandle(token);
        if duplicated == 0 {
            return Err(io::Error::last_os_error());
        }
        let wide = |p: &Path| p.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
        use std::os::windows::ffi::OsStrExt;
        let application = wide(exe);
        let mut command_line: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(std::iter::once(0)).collect();
        let directory = wide(dir);
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        let flags = CREATE_UNICODE_ENVIRONMENT | if hidden { CREATE_NO_WINDOW } else { 0 };
        let created = CreateProcessWithTokenW(primary, 0, application.as_ptr(), command_line.as_mut_ptr(), flags, null(), directory.as_ptr(), &startup, &mut info);
        let error = io::Error::last_os_error();
        CloseHandle(primary);
        if created == 0 {
            return Err(error);
        }
        CloseHandle(info.hThread);
        Ok(Launched::Handle(info.hProcess))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_program_can_tell_its_own_rights() {
        // Tests run without administrator rights here, as most people's programs do.
        let elevated = this_program_is_elevated();
        assert_eq!(process_is_elevated(std::process::id()), Some(elevated));
    }

    #[test]
    fn a_program_started_without_rights_runs_and_can_be_stopped() {
        let windows = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap();
        let exe = windows.join("System32").join("PING.EXE");
        let Ok(mut launched) = spawn_unelevated(&exe, &windows, true) else { panic!("couldn't start ping") };
        launched.kill();
    }
}
