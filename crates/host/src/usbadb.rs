//! Talks to the tablet's adbd directly over USB, without adb.exe or any local network socket.
//! This removes two processes' worth of hops from the pen's path. It's the same protocol and the
//! same authorization adb.exe uses, so it needs adb.exe's server to be stopped: only one program
//! can hold the USB interface.

use std::io;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rsa::pkcs8::DecodePrivateKey;
use rsa::{Pkcs1v15Sign, RsaPrivateKey};
use sha1::Sha1;
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Devices::Usb::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;

const A_CNXN: u32 = 0x4e58_4e43;
const A_AUTH: u32 = 0x4854_5541;
const A_OPEN: u32 = 0x4e45_504f;
const A_OKAY: u32 = 0x5941_4b4f;
const A_CLSE: u32 = 0x4553_4c43;
const A_WRTE: u32 = 0x4554_5257;
// This version lets both sides skip payload checksums.
const A_VERSION: u32 = 0x0100_0001;
const MAX_PAYLOAD: u32 = 1024 * 1024;
const AUTH_TOKEN: u32 = 1;
const AUTH_SIGNATURE: u32 = 2;
const AUTH_RSAPUBLICKEY: u32 = 3;
const HEADER_LEN: usize = 24;
const READ_BUFFER: usize = 64 * 1024;

// WinUSB's generic device interface, which is how this tablet's ADB interface registers.
const GUID_WINUSB_DEVICE: GUID = GUID::from_u128(0xdee824ef_729b_4a0e_9c14_b7117d33a817);
const GUID_ADB_DEVICE: GUID = GUID::from_u128(0xf72fe0d4_cbcb_407d_8814_9ed673d0dd6b);

/// One adb message. Its 24-byte header goes in one USB transfer and its payload in the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub command: u32,
    pub arg0: u32,
    pub arg1: u32,
    pub data_length: u32,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&self.command.to_le_bytes());
        b[4..8].copy_from_slice(&self.arg0.to_le_bytes());
        b[8..12].copy_from_slice(&self.arg1.to_le_bytes());
        b[12..16].copy_from_slice(&self.data_length.to_le_bytes());
        // Bytes 16..20 are the checksum, which A_VERSION leaves at zero.
        b[20..24].copy_from_slice(&(self.command ^ 0xffff_ffff).to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Option<Header> {
        if b.len() != HEADER_LEN {
            return None;
        }
        let word = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        (word(0) ^ 0xffff_ffff == word(20)).then(|| Header {
            command: word(0),
            arg0: word(4),
            arg1: word(8),
            data_length: word(12),
        })
    }
}

/// True for the path of the tablet's ADB interface.
pub fn is_adb_interface(path: &str) -> bool {
    let path = path.to_lowercase();
    path.contains("vid_04e8") && path.contains("adb")
}

fn interface_paths(guid: &GUID) -> Vec<Vec<u16>> {
    let mut paths = Vec::new();
    unsafe {
        let set = SetupDiGetClassDevsW(guid, null(), null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set as isize == -1 {
            return paths;
        }
        let mut index = 0;
        loop {
            let mut data: SP_DEVICE_INTERFACE_DATA = zeroed();
            data.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, null(), guid, index, &mut data) == 0 {
                break;
            }
            index += 1;
            let mut needed = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &data, null_mut(), 0, &mut needed, null_mut());
            // The detail struct starts with its size field, and is aligned for that.
            let mut buffer = vec![0u64; (needed as usize).div_ceil(8) + 1];
            let detail = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &data, detail, needed, null_mut(), null_mut()) != 0 {
                let start = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
                let len = (0..).take_while(|&i| *start.add(i) != 0).count();
                let mut path = std::slice::from_raw_parts(start, len).to_vec();
                path.push(0);
                paths.push(path);
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    paths
}

fn find_adb_interface() -> Option<Vec<u16>> {
    [GUID_WINUSB_DEVICE, GUID_ADB_DEVICE].iter().find_map(|guid| {
        interface_paths(guid).into_iter().find(|p| is_adb_interface(&String::from_utf16_lossy(&p[..p.len() - 1])))
    })
}

/// The USB bulk pipes of the ADB interface. Reads and writes can run on different threads.
pub struct UsbPipes {
    device: HANDLE,
    winusb: WINUSB_INTERFACE_HANDLE,
    pipe_in: u8,
    pipe_out: u8,
    send_lock: Mutex<()>,
}

// WinUSB allows concurrent use of different pipes from different threads.
unsafe impl Send for UsbPipes {}
unsafe impl Sync for UsbPipes {}

impl UsbPipes {
    pub fn open() -> io::Result<UsbPipes> {
        let path = find_adb_interface().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no ADB interface found"))?;
        unsafe {
            let device = CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                null_mut(),
            );
            if device == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut winusb: WINUSB_INTERFACE_HANDLE = null_mut();
            if WinUsb_Initialize(device, &mut winusb) == 0 {
                let e = io::Error::last_os_error();
                CloseHandle(device);
                return Err(e);
            }
            let mut pipes = UsbPipes { device, winusb, pipe_in: 0, pipe_out: 0, send_lock: Mutex::new(()) };
            let mut info: WINUSB_PIPE_INFORMATION = zeroed();
            let mut index = 0;
            while WinUsb_QueryPipe(winusb, 0, index, &mut info) != 0 {
                if info.PipeType == UsbdPipeTypeBulk {
                    if info.PipeId & 0x80 != 0 {
                        pipes.pipe_in = info.PipeId;
                    } else {
                        pipes.pipe_out = info.PipeId;
                    }
                }
                index += 1;
            }
            if pipes.pipe_in == 0 || pipes.pipe_out == 0 {
                return Err(io::Error::other("the ADB interface has no bulk pipes"));
            }
            Ok(pipes)
        }
    }

    pub fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut got = 0u32;
        let ok = unsafe { WinUsb_ReadPipe(self.winusb, self.pipe_in, buffer.as_mut_ptr(), buffer.len() as u32, &mut got, null_mut()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(got as usize)
    }

    pub fn write(&self, buffer: &[u8]) -> io::Result<()> {
        let mut sent = 0u32;
        let ok = unsafe { WinUsb_WritePipe(self.winusb, self.pipe_out, buffer.as_ptr(), buffer.len() as u32, &mut sent, null_mut()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn send(&self, command: u32, arg0: u32, arg1: u32, payload: &[u8]) -> io::Result<()> {
        let header = Header { command, arg0, arg1, data_length: payload.len() as u32 };
        // A header and its payload must stay together: another thread's message landing between
        // them corrupts the stream, and the tablet then waits forever.
        let _guard = self.send_lock.lock().unwrap();
        self.write(&header.encode())?;
        if !payload.is_empty() {
            self.write(payload)?;
        }
        Ok(())
    }
}

impl Drop for UsbPipes {
    fn drop(&mut self) {
        unsafe {
            WinUsb_Free(self.winusb);
            CloseHandle(self.device);
        }
    }
}

/// Reads one whole message, with a buffer big enough for any USB transfer it might receive.
fn read_message(pipes: &UsbPipes, buffer: &mut [u8]) -> io::Result<(Header, usize)> {
    let got = pipes.read(buffer)?;
    let header = Header::decode(&buffer[..got]).ok_or_else(|| io::Error::other("bad adb header"))?;
    let mut filled = 0;
    while filled < header.data_length as usize {
        filled += pipes.read(&mut buffer[filled..])?;
    }
    Ok((header, filled))
}

/// Signs the tablet's challenge with the key adb.exe made and the tablet already trusts.
pub fn sign_token(key_pem: &str, token: &[u8]) -> Result<Vec<u8>, String> {
    let key = RsaPrivateKey::from_pkcs8_pem(key_pem).map_err(|e| e.to_string())?;
    // adbd hands us the digest itself, so this signs it as-is, labelled as SHA-1.
    key.sign(Pkcs1v15Sign::new::<Sha1>(), token).map_err(|e| e.to_string())
}

fn android_key_dir() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var_os("USERPROFILE")?).join(".android"))
}

/// Does the connect and authorize handshake.
pub fn handshake(pipes: &UsbPipes) -> io::Result<()> {
    let dir = android_key_dir().ok_or_else(|| io::Error::other("no user profile folder"))?;
    let key = std::fs::read_to_string(dir.join("adbkey"))?;
    let mut buffer = vec![0u8; READ_BUFFER];
    pipes.send(A_CNXN, A_VERSION, MAX_PAYLOAD, b"host::\0")?;
    let mut sent_signature = false;
    loop {
        let (header, len) = read_message(pipes, &mut buffer)?;
        match header.command {
            A_CNXN => return Ok(()),
            A_AUTH if header.arg0 == AUTH_TOKEN && !sent_signature => {
                let signature = sign_token(&key, &buffer[..len]).map_err(io::Error::other)?;
                pipes.send(A_AUTH, AUTH_SIGNATURE, 0, &signature)?;
                sent_signature = true;
            }
            A_AUTH if header.arg0 == AUTH_TOKEN => {
                // The tablet doesn't know this key, so offer it, which shows the Allow prompt.
                let mut public = std::fs::read(dir.join("adbkey.pub"))?;
                public.push(0);
                pipes.send(A_AUTH, AUTH_RSAPUBLICKEY, 0, &public)?;
            }
            _ => {}
        }
    }
}

/// Opens a stream to a service on the tablet, and returns the tablet's id for it.
pub fn open_stream(pipes: &UsbPipes, local_id: u32, service: &str, buffer: &mut [u8]) -> io::Result<u32> {
    let mut name = service.as_bytes().to_vec();
    name.push(0);
    pipes.send(A_OPEN, local_id, 0, &name)?;
    loop {
        let (header, _) = read_message(pipes, buffer)?;
        match header.command {
            A_OKAY if header.arg1 == local_id => return Ok(header.arg0),
            A_CLSE if header.arg1 == local_id => return Err(io::Error::new(io::ErrorKind::ConnectionRefused, "the tablet refused the stream")),
            A_WRTE => pipes.send(A_OKAY, header.arg1, header.arg0, &[])?,
            _ => {}
        }
    }
}

/// Measures round trips through the helper over this direct path, to compare with `--ping-bench`.
pub fn bench(seconds: u64) -> io::Result<()> {
    use osutab_core::{DownFrame, UpFrame, DOWN_FRAME_LEN};
    const HELPER_STREAM: u32 = 1;
    const PEN_STREAM: u32 = 2;
    const PERIOD: Duration = Duration::from_micros(2080);

    let pipes = Arc::new(UsbPipes::open()?);
    handshake(&pipes)?;
    println!("connected to the tablet directly over USB");
    let mut buffer = vec![0u8; READ_BUFFER];
    open_stream(&pipes, HELPER_STREAM, "shell:pkill -x osutab-helper; exec /data/local/tmp/osutab-helper --no-grab", &mut buffer)?;

    let start_wait = Instant::now();
    let remote = loop {
        match open_stream(&pipes, PEN_STREAM, &format!("localabstract:{}", osutab_core::SOCKET_NAME), &mut buffer) {
            Ok(id) => break id,
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused && start_wait.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(e) => return Err(e),
        }
    };
    println!("pen stream open");

    let clock = Instant::now();
    let credit = Arc::new(AtomicU32::new(1));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let (pipes, credit, stop) = (pipes.clone(), credit.clone(), stop.clone());
        std::thread::spawn(move || -> io::Result<()> {
            let mut next = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                next += PERIOD;
                while Instant::now() < next {
                    std::hint::spin_loop();
                }
                // adb allows one unacknowledged write per stream.
                if credit.swap(0, Ordering::AcqRel) == 0 {
                    continue;
                }
                let host_ns = clock.elapsed().as_nanos() as u64;
                pipes.send(A_WRTE, PEN_STREAM, remote, &UpFrame::Ping { host_ns }.encode())?;
            }
            Ok(())
        })
    };

    let mut times = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        let (header, len) = read_message(&pipes, &mut buffer)?;
        let received = clock.elapsed().as_nanos() as u64;
        match header.command {
            A_OKAY if header.arg1 == PEN_STREAM => credit.store(1, Ordering::Release),
            A_WRTE => {
                pipes.send(A_OKAY, header.arg1, header.arg0, &[])?;
                if header.arg1 == PEN_STREAM {
                    pending.extend_from_slice(&buffer[..len]);
                    while pending.len() >= DOWN_FRAME_LEN {
                        let frame: [u8; DOWN_FRAME_LEN] = pending[..DOWN_FRAME_LEN].try_into().unwrap();
                        pending.drain(..DOWN_FRAME_LEN);
                        if let Some(DownFrame::Pong(p)) = DownFrame::decode(&frame) {
                            times.push(received.saturating_sub(p.echo_ns) as f32 / 1e6);
                        }
                    }
                }
            }
            A_CLSE if header.arg1 == PEN_STREAM => return Err(io::Error::other("the tablet closed the pen stream")),
            _ => {}
        }
    }
    stop.store(true, Ordering::Relaxed);
    // Not joined: a write in flight can wait on a reply nobody is reading now. Exiting ends it.
    drop(writer);
    let d = crate::stats::Dist::of(&mut times);
    println!(
        "round trip ms: p50 {:.3} p90 {:.3} p99 {:.3} p99.9 {:.3} max {:.3} (n={})",
        d.p50, d.p90, d.p99, d.p999, d.max, d.count
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let h = Header { command: A_WRTE, arg0: 7, arg1: 9, data_length: 32 };
        assert_eq!(Header::decode(&h.encode()), Some(h));
    }

    #[test]
    fn known_cnxn_bytes() {
        // Checked against adb's own protocol description: "CNXN", then fields, then the magic.
        let b = Header { command: A_CNXN, arg0: A_VERSION, arg1: MAX_PAYLOAD, data_length: 7 }.encode();
        assert_eq!(&b[0..4], b"CNXN");
        assert_eq!(&b[4..8], &[0x01, 0x00, 0x00, 0x01]);
        assert_eq!(&b[12..16], &[7, 0, 0, 0]);
        assert_eq!(&b[20..24], &[0xbc, 0xb1, 0xa7, 0xb1]);
    }

    #[test]
    fn bad_headers_are_rejected() {
        assert_eq!(Header::decode(&[0u8; 10]), None);
        let mut b = Header { command: A_OKAY, arg0: 1, arg1: 2, data_length: 0 }.encode();
        b[20] ^= 1;
        assert_eq!(Header::decode(&b), None);
    }

    #[test]
    fn recognises_this_tablets_real_interface_path() {
        let real = r"\\?\usb#vid_04e8&pid_6860&adb#7&3073af68&0&0003#{dee824ef-729b-4a0e-9c14-b7117d33a817}";
        assert!(is_adb_interface(real));
        let camera = r"\\?\usb#vid_174f&pid_246f&mi_02#7&2c27ee68&1&0002#{dee824ef-729b-4a0e-9c14-b7117d33a817}";
        assert!(!is_adb_interface(camera));
    }
}
