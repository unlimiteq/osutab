//! Talks to the tablet's adbd directly over USB, without adb.exe or any local network socket.
//! This removes two processes' worth of hops from the pen's path. It's the same protocol and the
//! same authorization adb.exe uses, so it needs adb.exe's server to be stopped: only one program
//! can hold the USB interface.

use std::io::{self, Read, Write};
use std::mem::{size_of, zeroed};
use std::net::{Ipv4Addr, TcpStream};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

use osutab_core::{DownFrame, Hello, UpFrame, DOWN_FRAME_LEN, PROTOCOL_VERSION};

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
// WinUSB's policy number for how long a transfer may wait, from winusbio.h.
const PIPE_TRANSFER_TIMEOUT: u32 = 0x03;
// Until the helper answers, a silent tablet shouldn't block forever.
const SETUP_TIMEOUT_MS: u32 = 3000;
const OPEN_RETRY: Duration = Duration::from_secs(3);
const HELPER_STREAM: u32 = 1;
const REVERSE_STREAM: u32 = 2;
const PEN_STREAM: u32 = 3;
const FIRST_APP_STREAM: u32 = 100;
const APP_WRITE_TIMEOUT: Duration = Duration::from_millis(200);

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
    usb3: bool,
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
            let mut pipes = UsbPipes { device, winusb, pipe_in: 0, pipe_out: 0, usb3: false, send_lock: Mutex::new(()) };
            let mut info: WINUSB_PIPE_INFORMATION = zeroed();
            let mut index = 0;
            while WinUsb_QueryPipe(winusb, 0, index, &mut info) != 0 {
                if info.PipeType == UsbdPipeTypeBulk {
                    if info.PipeId & 0x80 != 0 {
                        // The USB standard sets this size, so it reveals the link's speed.
                        pipes.usb3 = info.MaximumPacketSize >= 1024;
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

    /// How long a read may wait, or 0 to wait for as long as it takes.
    fn set_read_timeout(&self, ms: u32) -> io::Result<()> {
        let ok = unsafe { WinUsb_SetPipePolicy(self.winusb, self.pipe_in, PIPE_TRANSFER_TIMEOUT, size_of::<u32>() as u32, &ms as *const u32 as *const _) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
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

/// The tablet app's link, carried over this USB connection instead of by adb.exe's `adb reverse`.
struct AppStream {
    local: u32,
    remote: u32,
    tcp: TcpStream,
    can_send: Arc<AtomicBool>,
}

/// The live pen stream over direct USB. It also carries the tablet app's link, so the app doesn't
/// need to know which USB path is in use.
pub struct DirectLink {
    pipes: Arc<UsbPipes>,
    buffer: Vec<u8>,
    remote_pen: u32,
    pending: Vec<u8>,
    // Waiting for an acknowledgement here, instead of before returning a frame, keeps the USB
    // write from ever delaying SendInput.
    unacknowledged: bool,
    can_ping: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    app_streams: Vec<AppStream>,
    next_app_stream: u32,
    app_port: u16,
}

/// Sends pings from another thread.
pub struct DirectPinger {
    pipes: Arc<UsbPipes>,
    remote_pen: u32,
    can_ping: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

impl DirectPinger {
    /// Skips a ping while the last one is unanswered, because adb allows one unacknowledged write
    /// per stream. Fails once the link has closed, which ends the pinging thread.
    pub fn ping(&self, host_ns: u64) -> io::Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::NotConnected.into());
        }
        if self.can_ping.swap(false, Ordering::AcqRel) {
            self.pipes.send(A_WRTE, PEN_STREAM, self.remote_pen, &UpFrame::Ping { host_ns }.encode())?;
        }
        Ok(())
    }
}

/// adb.exe takes a moment to let go of the interface after it's stopped.
fn open_when_free() -> io::Result<UsbPipes> {
    let start = Instant::now();
    loop {
        match UsbPipes::open() {
            Ok(pipes) => return Ok(pipes),
            Err(_) if start.elapsed() < OPEN_RETRY => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e),
        }
    }
}

impl DirectLink {
    /// Connects, starts the helper, and waits for its hello.
    pub fn start(helper_args: &str, app_port: u16) -> io::Result<(DirectLink, Hello)> {
        let pipes = Arc::new(open_when_free()?);
        pipes.set_read_timeout(SETUP_TIMEOUT_MS)?;
        handshake(&pipes)?;
        let mut link = DirectLink {
            pipes,
            buffer: vec![0u8; READ_BUFFER],
            remote_pen: 0,
            pending: Vec::with_capacity(DOWN_FRAME_LEN * 8),
            unacknowledged: false,
            can_ping: Arc::new(AtomicBool::new(true)),
            closed: Arc::new(AtomicBool::new(false)),
            app_streams: Vec::new(),
            next_app_stream: FIRST_APP_STREAM,
            app_port,
        };
        let helper = crate::adb::REMOTE_HELPER;
        link.open(HELPER_STREAM, &format!("shell:pkill -x osutab-helper; exec {helper} {helper_args}"))?;
        link.open(REVERSE_STREAM, &format!("reverse:forward:tcp:{app_port};tcp:{app_port}"))?;

        // The helper needs a moment before its socket exists.
        let start = Instant::now();
        link.remote_pen = loop {
            match link.open(PEN_STREAM, &format!("localabstract:{}", osutab_core::SOCKET_NAME)) {
                Ok(id) => break id,
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused && start.elapsed() < OPEN_RETRY => {
                    std::thread::sleep(Duration::from_millis(30));
                }
                Err(e) => return Err(e),
            }
        };

        let mut frame = [0u8; DOWN_FRAME_LEN];
        link.next_frame(&mut frame)?;
        let hello = match DownFrame::decode(&frame) {
            Some(DownFrame::Hello(h)) if h.version == PROTOCOL_VERSION => h,
            _ => return Err(io::Error::other("unexpected first frame from helper")),
        };
        link.pipes.set_read_timeout(0)?;
        Ok((link, hello))
    }

    pub fn is_usb3(&self) -> bool {
        self.pipes.usb3
    }

    pub fn pinger(&self) -> DirectPinger {
        DirectPinger {
            pipes: self.pipes.clone(),
            remote_pen: self.remote_pen,
            can_ping: self.can_ping.clone(),
            closed: self.closed.clone(),
        }
    }

    /// Opens a stream to a service on the tablet, and returns the tablet's id for it.
    fn open(&mut self, local: u32, service: &str) -> io::Result<u32> {
        let mut name = service.as_bytes().to_vec();
        name.push(0);
        self.pipes.send(A_OPEN, local, 0, &name)?;
        loop {
            let (header, len) = read_message(&self.pipes, &mut self.buffer)?;
            match header.command {
                A_OKAY if header.arg1 == local => return Ok(header.arg0),
                A_CLSE if header.arg1 == local => return Err(io::Error::new(io::ErrorKind::ConnectionRefused, format!("the tablet refused {service}"))),
                _ => self.handle_other(header, len)?,
            }
        }
    }

    /// The next frame from the helper. Blocks until there is one.
    #[inline]
    pub fn next_frame(&mut self, frame: &mut [u8; DOWN_FRAME_LEN]) -> io::Result<()> {
        loop {
            if self.pending.len() >= DOWN_FRAME_LEN {
                frame.copy_from_slice(&self.pending[..DOWN_FRAME_LEN]);
                self.pending.drain(..DOWN_FRAME_LEN);
                return Ok(());
            }
            if self.unacknowledged {
                self.unacknowledged = false;
                self.pipes.send(A_OKAY, PEN_STREAM, self.remote_pen, &[])?;
            }
            let (header, len) = read_message(&self.pipes, &mut self.buffer)?;
            match header.command {
                A_WRTE if header.arg1 == PEN_STREAM => {
                    self.unacknowledged = true;
                    self.pending.extend_from_slice(&self.buffer[..len]);
                }
                A_OKAY if header.arg1 == PEN_STREAM => self.can_ping.store(true, Ordering::Release),
                A_CLSE if header.arg1 == PEN_STREAM => return Err(io::ErrorKind::UnexpectedEof.into()),
                _ => self.handle_other(header, len)?,
            }
        }
    }

    /// Everything that isn't pen data: the tablet app's link, and stream housekeeping.
    fn handle_other(&mut self, header: Header, len: usize) -> io::Result<()> {
        match header.command {
            A_OPEN => self.open_app_stream(header.arg0),
            A_WRTE => {
                self.pipes.send(A_OKAY, header.arg1, header.arg0, &[])?;
                if let Some(index) = self.app_streams.iter().position(|s| s.local == header.arg1) {
                    // A slow control link costs that link, not the pen.
                    if self.app_streams[index].tcp.write_all(&self.buffer[..len]).is_err() {
                        self.close_app_stream(index);
                    }
                }
                Ok(())
            }
            A_OKAY => {
                if let Some(stream) = self.app_streams.iter().find(|s| s.local == header.arg1) {
                    stream.can_send.store(true, Ordering::Release);
                }
                Ok(())
            }
            A_CLSE => {
                if let Some(index) = self.app_streams.iter().position(|s| s.local == header.arg1) {
                    self.app_streams.remove(index);
                }
                Ok(())
            }
            A_CNXN => Err(io::Error::new(io::ErrorKind::ConnectionReset, "the tablet restarted its USB connection")),
            _ => Ok(()),
        }
    }

    fn close_app_stream(&mut self, index: usize) {
        let stream = self.app_streams.remove(index);
        let _ = self.pipes.send(A_CLSE, stream.local, stream.remote, &[]);
    }

    /// The tablet app connected. Passes it to the local port it expects to reach.
    fn open_app_stream(&mut self, remote: u32) -> io::Result<()> {
        let local = self.next_app_stream;
        self.next_app_stream += 1;
        let tcp = match TcpStream::connect((Ipv4Addr::LOCALHOST, self.app_port)) {
            Ok(tcp) => tcp,
            Err(_) => return self.pipes.send(A_CLSE, 0, remote, &[]),
        };
        let _ = tcp.set_nodelay(true);
        let _ = tcp.set_write_timeout(Some(APP_WRITE_TIMEOUT));
        let mut reader = tcp.try_clone()?;
        let can_send = Arc::new(AtomicBool::new(true));
        self.pipes.send(A_OKAY, local, remote, &[])?;
        self.app_streams.push(AppStream { local, remote, tcp, can_send: can_send.clone() });

        let (pipes, closed) = (self.pipes.clone(), self.closed.clone());
        std::thread::spawn(move || {
            crate::tuning::raise_this_thread();
            let mut chunk = [0u8; 4096];
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                while !can_send.swap(false, Ordering::AcqRel) {
                    if closed.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                if pipes.send(A_WRTE, local, remote, &chunk[..n]).is_err() {
                    return;
                }
            }
            let _ = pipes.send(A_CLSE, local, remote, &[]);
        });
        Ok(())
    }

    /// Says goodbye, so the helper exits and Android gets the pen back straight away.
    pub fn close(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        for stream in self.app_streams.drain(..) {
            let _ = stream.tcp.shutdown(std::net::Shutdown::Both);
            let _ = self.pipes.send(A_CLSE, stream.local, stream.remote, &[]);
        }
        let _ = self.pipes.send(A_CLSE, PEN_STREAM, self.remote_pen, &[]);
    }
}

impl Drop for DirectLink {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        for stream in self.app_streams.drain(..) {
            let _ = stream.tcp.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Measures round trips through the helper over this direct path, to compare with `--ping-bench`.
pub fn bench(seconds: u64) -> io::Result<()> {
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
    let stop = Arc::new(AtomicBool::new(false));
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
