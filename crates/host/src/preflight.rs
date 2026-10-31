//! Finds running apps that were linked with input stutters in testing.

use std::mem::size_of;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;

/// Which of the known apps these process names belong to, in a fixed order, without repeats.
pub fn interfering<'a>(process_names: impl Iterator<Item = &'a str>) -> Vec<&'static str> {
    const KNOWN: [(&str, &str); 3] = [
        ("powertoys", "PowerToys"),
        ("wallpaper", "Wallpaper Engine"),
        ("nvidia overlay", "NVIDIA overlay"),
    ];
    let names: Vec<String> = process_names.map(str::to_lowercase).collect();
    KNOWN
        .iter()
        .filter(|(prefix, _)| names.iter().any(|n| n.starts_with(prefix)))
        .map(|(_, label)| *label)
        .collect()
}

pub fn process_names() -> Vec<String> {
    let mut names = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return names;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let end = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            names.push(String::from_utf16_lossy(&entry.szExeFile[..end]));
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_each_app_once_from_real_process_names() {
        let names = [
            "PowerToys.exe",
            "PowerToys.FancyZones.exe",
            "wallpaper64.exe",
            "NVIDIA Overlay.exe",
            "NVIDIA Overlay.exe",
            "osu!.exe",
        ];
        assert_eq!(
            interfering(names.iter().copied()),
            vec!["PowerToys", "Wallpaper Engine", "NVIDIA overlay"]
        );
    }

    #[test]
    fn unrelated_apps_are_ignored() {
        let names = ["osu!.exe", "nvcontainer.exe", "explorer.exe", "NVIDIA App.exe"];
        assert!(interfering(names.iter().copied()).is_empty());
    }

    #[test]
    fn this_program_can_list_processes() {
        assert!(process_names().iter().any(|n| n.to_lowercase().contains("osutab")));
    }
}
