//! Windows scheduling choices for the pen's path. Each was kept because it measurably helped: with
//! every core busy, raising adb.exe took the median from about 1.5 ms to 0.6 ms and the 99th
//! percentile from about 30 ms to 1 ms. On a quiet PC it changes little.

use std::mem::size_of;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
use windows_sys::Win32::System::Threading::*;

/// Stops Windows treating a process as background work to slow down and save power.
fn no_power_throttling(process: HANDLE) -> bool {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    unsafe {
        SetProcessInformation(
            process,
            ProcessPowerThrottling,
            &state as *const _ as *const _,
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        ) != 0
    }
}

pub fn apply_to_this_program() {
    if !no_power_throttling(unsafe { GetCurrentProcess() }) {
        eprintln!("could not turn off power throttling");
    }
}

fn adb_server_ids() -> Vec<u32> {
    process_ids("adb.exe")
}

/// Every running process with this file name.
pub fn process_ids(name: &str) -> Vec<u32> {
    let mut ids = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return ids;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case(name) {
                ids.push(entry.th32ProcessID);
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    ids
}

/// adb.exe carries the pen, but runs at normal priority, so a busy game makes every report wait
/// its turn for a core. It only uses a sliver of CPU, so raising it costs the game nothing.
pub fn apply_to_adb() {
    for id in adb_server_ids() {
        unsafe {
            let process = OpenProcess(PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION, 0, id);
            if process.is_null() {
                continue;
            }
            no_power_throttling(process);
            SetPriorityClass(process, HIGH_PRIORITY_CLASS);
            CloseHandle(process);
        }
    }
}

/// For a thread that shares the USB write lock with the input thread. If Windows paused one of these
/// while it held the lock, the input thread would have to wait for it, however high its own priority.
pub fn raise_this_thread() {
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
}

/// Keeps every core busy at normal priority, to test the pen's path while something like a game is
/// using the CPU. Moves nothing and sends no input.
pub fn cpu_burn(seconds: u64) {
    let threads = std::thread::available_parallelism().map_or(8, |n| n.get());
    let end = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            std::thread::spawn(move || {
                let mut x = 1u64;
                while std::time::Instant::now() < end {
                    for _ in 0..10_000 {
                        x = std::hint::black_box(x.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(7));
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        let _ = worker.join();
    }
}
