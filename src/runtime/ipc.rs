//! A process-local message pump owns all application state. The named pipe
//! carries one bounded UTF-8 request/reply per connection to that pump.
use crate::cli_error;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::{
    Foundation::*,
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, PSID, SECURITY_ATTRIBUTES, TOKEN_GROUPS, TOKEN_INFORMATION_CLASS,
        TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel, TokenLogonSid,
    },
    Storage::FileSystem::*,
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Pipes::*,
        RemoteDesktop::ProcessIdToSessionId,
        Threading::*,
    },
    UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostThreadMessageW, WM_APP,
    },
};
use windows_sys::core::w;

pub(super) const WAKE: u32 = WM_APP + 31;
const MAX_COMMAND: usize = 4096;
const MAX_REPLY: usize = 512 * 1024;
const CLIENT_ACCESS: u32 = FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const STARTUP_WAIT: Duration = Duration::from_secs(4);
const COMMAND_WAIT: Duration = Duration::from_secs(10);
const IO_WAIT: Duration = Duration::from_secs(2);
const STOPPED: &str = "Rustile exited before the command completed.";
const TIMED_OUT: &str = "Timed out waiting for Rustile to complete the command.";
const PERMISSION: &str = "Use the same Windows account as Rustile. If Rustile is running as administrator, run your terminal as administrator too.";

type Reply = Result<String, String>;

pub(super) struct Request {
    pub(super) command: String,
    deadline: Instant,
    connection: Weak<()>,
    reply: mpsc::Sender<Reply>,
}

impl Request {
    pub(super) fn expired(&self) -> bool {
        Instant::now() >= self.deadline || self.connection.strong_count() == 0
    }

    pub(super) fn respond(self, reply: Reply) {
        let _ = self.reply.send(reply);
    }
}

pub(super) struct Server {
    requests: mpsc::Receiver<Request>,
    stop: Arc<OwnedHandle>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    pub(super) fn start() -> Result<Self, String> {
        let thread = unsafe { GetCurrentThreadId() };
        Self::listen(thread, std::process::id()).map_err(|err| {
            format!(
                "Could not open the Rustile command pipe: {}",
                cli_error::io(err)
            )
        })
    }

    fn listen(owner: u32, pid: u32) -> io::Result<Self> {
        let stop = Arc::new(event()?);
        let listener = Channel::new(create_pipe(&pipe_name(pid))?)?;
        let session = session_id()?;
        let worker_stop = stop.clone();
        let (tx, requests) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("rustile-cli".into())
            .spawn(move || {
                let stop = worker_stop.as_raw_handle();
                while unsafe { WaitForSingleObject(stop, 0) } == WAIT_TIMEOUT {
                    if listener.accept(stop).is_ok() {
                        let mut peer_session = 0;
                        if unsafe {
                            GetNamedPipeClientSessionId(listener.handle(), &mut peer_session)
                        } != 0
                            && peer_session == session
                        {
                            let _ = serve_one(&listener, stop, owner, &tx);
                        }
                    }
                    unsafe { DisconnectNamedPipe(listener.handle()) };
                }
            })?;
        Ok(Self {
            requests,
            stop,
            worker: Some(worker),
        })
    }

    pub(super) fn try_recv(&self) -> Option<Request> {
        while let Ok(request) = self.requests.try_recv() {
            if !request.expired() {
                return Some(request);
            }
        }
        None
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        unsafe { SetEvent(self.stop.as_raw_handle()) };
        if let Some(thread) = self.worker.take() {
            let _ = thread.join();
        }
    }
}

fn serve_one(
    pipe: &Channel,
    stop: HANDLE,
    owner: u32,
    sender: &mpsc::SyncSender<Request>,
) -> io::Result<()> {
    let mut buf = [0; MAX_COMMAND];
    let n = pipe.read(&mut buf, IO_WAIT, stop)?;
    let reply = match std::str::from_utf8(&buf[..n]) {
        Ok(command) if !command.is_empty() => {
            let (reply, rx) = mpsc::channel();
            let deadline = Instant::now() + COMMAND_WAIT;
            let connection = Arc::new(());
            let request = Request {
                command: command.to_owned(),
                deadline,
                connection: Arc::downgrade(&connection),
                reply,
            };
            if sender.try_send(request).is_err()
                || unsafe { PostThreadMessageW(owner, WAKE, 0, 0) } == 0
            {
                return Err(io::Error::other(STOPPED));
            }
            loop {
                match rx.recv_timeout(Duration::from_millis(60)) {
                    Ok(result) => break result,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break Err(STOPPED.into()),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if Instant::now() >= deadline {
                            break Err(TIMED_OUT.into());
                        }
                        if unsafe { WaitForSingleObject(stop, 0) } == WAIT_OBJECT_0 {
                            break rx.try_recv().unwrap_or_else(|_| Err(STOPPED.into()));
                        }
                        // If the caller has gone away, a still-queued command
                        // must expire before the UI thread eventually sees it.
                        if unsafe {
                            PeekNamedPipe(
                                pipe.handle(),
                                null_mut(),
                                0,
                                null_mut(),
                                null_mut(),
                                null_mut(),
                            )
                        } == 0
                        {
                            return Err(io::Error::last_os_error());
                        }
                    }
                }
            }
        }
        _ => Err("Invalid Rustile command.".into()),
    };
    let (status, text) = match reply {
        Ok(text) if text.len() < MAX_REPLY => (0, text),
        Err(text) if text.len() < MAX_REPLY => (1, text),
        _ => (1, "The reply exceeds the 512 KiB limit.".to_owned()),
    };
    let mut message = Vec::with_capacity(5 + text.len());
    message.extend_from_slice(&(1 + text.len() as u32).to_le_bytes());
    message.push(status);
    message.extend_from_slice(text.as_bytes());
    pipe.write(&message, IO_WAIT, null_mut())?;
    // DisconnectNamedPipe discards unread data. An acknowledgement preserves
    // even the final quit reply, without an unbounded FlushFileBuffers.
    let mut ack = [0u8; 1];
    pipe.read(&mut ack, IO_WAIT, null_mut())?;
    Ok(())
}

fn pipe_name(pid: u32) -> Vec<u16> {
    format!(r"\\.\pipe\Rustile.CLI.v1.{pid}")
        .encode_utf16()
        .chain([0])
        .collect()
}

fn session_id() -> io::Result<u32> {
    let mut session = 0;
    if unsafe { ProcessIdToSessionId(std::process::id(), &mut session) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(session)
}

fn token_info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<usize>> {
    let mut bytes = 0;
    unsafe { GetTokenInformation(token, class, null_mut(), 0, &mut bytes) };
    if bytes == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
    if unsafe { GetTokenInformation(token, class, buffer.as_mut_ptr().cast(), bytes, &mut bytes) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

fn sid_text(sid: PSID) -> io::Result<String> {
    let mut raw = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe {
        let mut n = 0;
        while *raw.add(n) != 0 {
            n += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(raw, n));
        LocalFree(raw.cast());
        Ok(text)
    }
}

fn create_pipe(name: &[u16]) -> io::Result<OwnedHandle> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let logon = token_info(token.as_raw_handle(), TokenLogonSid)?;
    let groups = unsafe { &*logon.as_ptr().cast::<TOKEN_GROUPS>() };
    if groups.GroupCount != 1 {
        return Err(io::Error::other(
            "Could not identify the Windows logon session.",
        ));
    }
    let sid = sid_text(groups.Groups[0].Sid)?;
    let level = token_info(token.as_raw_handle(), TokenIntegrityLevel)?;
    let label = sid_text(unsafe { (*level.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid })?;
    // Give the interactive logon only the specific client rights (not
    // FILE_CREATE_PIPE_INSTANCE), and enforce the server's integrity level.
    let sddl: Vec<u16> = format!("D:P(A;;0x{CLIENT_ACCESS:x};;;{sid})S:(ML;;NW;;;{label})")
        .encode_utf16()
        .chain([0])
        .collect();
    let mut sd = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let pipe = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            8192,
            MAX_COMMAND as u32,
            0,
            &attributes,
        )
    };
    let result = owned_file(pipe);
    unsafe { LocalFree(sd) };
    result
}

fn owned_file(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

fn event() -> io::Result<OwnedHandle> {
    let h = unsafe { CreateEventW(null(), 1, 0, null()) };
    if h.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(h) })
    }
}

// No operation outlives its OVERLAPPED or a borrowed read/write buffer. The
// shutdown event interrupts idle accepts without polling.
struct Channel {
    file: OwnedHandle,
    event: OwnedHandle,
}

impl Channel {
    fn new(file: OwnedHandle) -> io::Result<Self> {
        Ok(Self {
            file,
            event: event()?,
        })
    }

    fn handle(&self) -> HANDLE {
        self.file.as_raw_handle()
    }

    fn operation(
        &self,
        timeout: u32,
        stop: HANDLE,
        begin: impl FnOnce(*mut OVERLAPPED) -> i32,
    ) -> io::Result<usize> {
        let event = self.event.as_raw_handle();
        unsafe { ResetEvent(event) };
        let mut ov = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        if begin(&raw mut ov) == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
            let events = [event, stop];
            let count = if stop.is_null() { 1 } else { 2 };
            let wait = unsafe { WaitForMultipleObjects(count, events.as_ptr(), 0, timeout) };
            if wait != WAIT_OBJECT_0 {
                let error = match wait {
                    WAIT_TIMEOUT => io::Error::new(io::ErrorKind::TimedOut, TIMED_OUT),
                    WAIT_FAILED => io::Error::last_os_error(),
                    _ => io::Error::new(io::ErrorKind::Interrupted, STOPPED),
                };
                let mut ignored = 0;
                unsafe {
                    CancelIoEx(self.handle(), &ov);
                    GetOverlappedResult(self.handle(), &ov, &mut ignored, 1);
                }
                return Err(error);
            }
        }
        let mut transferred = 0;
        if unsafe { GetOverlappedResult(self.handle(), &ov, &mut transferred, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(transferred as usize)
    }

    fn accept(&self, stop: HANDLE) -> io::Result<()> {
        match self.operation(INFINITE, stop, |ov| unsafe {
            ConnectNamedPipe(self.handle(), ov)
        }) {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) => Ok(()),
            result => result.map(|_| ()),
        }
    }

    fn read(&self, buf: &mut [u8], timeout: Duration, stop: HANDLE) -> io::Result<usize> {
        self.operation(timeout.as_millis() as u32, stop, |ov| unsafe {
            ReadFile(
                self.handle(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                null_mut(),
                ov,
            )
        })
    }

    fn write(&self, buf: &[u8], timeout: Duration, stop: HANDLE) -> io::Result<()> {
        let n = self.operation(timeout.as_millis() as u32, stop, |ov| unsafe {
            WriteFile(
                self.handle(),
                buf.as_ptr(),
                buf.len() as u32,
                null_mut(),
                ov,
            )
        })?;
        if n != buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "The command pipe received only part of the message.",
            ));
        }
        Ok(())
    }

    fn read_exact(&self, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            let n = self.read(
                bytes,
                deadline.saturating_duration_since(Instant::now()),
                null_mut(),
            )?;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            bytes = &mut bytes[n..];
        }
        Ok(())
    }
}

pub(super) fn send(command: &str, grant_foreground: bool, wait_for_exit: bool) -> Reply {
    if command.is_empty() || command.len() > MAX_COMMAND {
        return Err("A Rustile command must contain between 1 and 4096 bytes.".into());
    }
    if !super::instance::Instance::running()? {
        return Err("Rustile is not running.".into());
    }
    let start = Instant::now();
    loop {
        // FindWindow is scoped to the caller's Win32 desktop, unlike named
        // pipes. Explorer's virtual desktops all share this same owner.
        let tray = unsafe { FindWindowW(w!("Rustile.Tray"), w!("Rustile")) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(tray, &mut pid) };
        if pid != 0 {
            let name = pipe_name(pid);
            match connect(pid, &name) {
                Ok(pipe) => {
                    let process = if wait_for_exit {
                        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
                        if handle.is_null() {
                            return Err(format!(
                                "Could not wait for Rustile to exit: {}",
                                cli_error::io(io::Error::last_os_error())
                            ));
                        }
                        Some(unsafe { OwnedHandle::from_raw_handle(handle) })
                    } else {
                        None
                    };
                    if grant_foreground {
                        unsafe { AllowSetForegroundWindow(pid) };
                    }
                    let reply = transact(&pipe, command)
                        .map_err(|err| format!("Rustile command pipe: {}", cli_error::io(err)))??;
                    drop(pipe);
                    if let Some(process) = process {
                        match unsafe { WaitForSingleObject(process.as_raw_handle(), 8000) } {
                            WAIT_OBJECT_0 => {}
                            WAIT_TIMEOUT => {
                                return Err(
                                    "Rustile accepted the quit command and is still shutting down."
                                        .into(),
                                );
                            }
                            _ => {
                                return Err(format!(
                                    "Could not wait for Rustile to exit: {}",
                                    cli_error::io(io::Error::last_os_error())
                                ));
                            }
                        }
                    }
                    return Ok(reply);
                }
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    if start.elapsed() >= STARTUP_WAIT + COMMAND_WAIT + IO_WAIT {
                        return Err(
                            "Rustile is processing another command. Try again shortly.".into()
                        );
                    }
                    unsafe { WaitNamedPipeW(name.as_ptr(), 100) };
                    continue;
                }
                Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    return Err(format!(
                        "Rustile command pipe: {}. {PERMISSION}",
                        cli_error::io(e)
                    ));
                }
                Err(e) => return Err(format!("Rustile command pipe: {}", cli_error::io(e))),
            }
        } else {
            thread::sleep(Duration::from_millis(20));
        }
        if start.elapsed() >= STARTUP_WAIT {
            return Err("The running Rustile instance did not open its command pipe. Check that it supports the CLI.".into());
        }
    }
}

fn connect(pid: u32, name: &[u16]) -> io::Result<Channel> {
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            CLIENT_ACCESS,
            0,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            null_mut(),
        )
    };
    let pipe = Channel::new(owned_file(handle)?)?;
    let mut peer = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.handle(), &mut peer) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if peer != pid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The Rustile command pipe belongs to an unexpected process",
        ));
    }
    Ok(pipe)
}

fn transact(pipe: &Channel, command: &str) -> io::Result<Reply> {
    pipe.write(command.as_bytes(), IO_WAIT, null_mut())?;
    let deadline = Instant::now() + COMMAND_WAIT + IO_WAIT;
    let mut length = [0; 4];
    pipe.read_exact(&mut length, deadline)?;
    let n = u32::from_le_bytes(length) as usize;
    if !(1..=MAX_REPLY).contains(&n) {
        return Err(io::Error::other("Invalid Rustile reply size."));
    }
    let mut buf = vec![0; n];
    pipe.read_exact(&mut buf, deadline)?;
    let (status, bytes) = (buf[0], &buf[1..]);
    let text = std::str::from_utf8(bytes)
        .map_err(io::Error::other)?
        .to_owned();
    // The primary may already be closing. Receiving the entire message is
    // sufficient to know the result; an acknowledgement itself isn't replayed.
    let _ = pipe.write(&[1], IO_WAIT, null_mut());
    match status {
        0 => Ok(Ok(text)),
        1 => Ok(Err(text)),
        _ => Err(io::Error::other("Unknown Rustile reply status.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MSG, PM_NOREMOVE, PeekMessageW};

    #[test]
    fn native_pipe_round_trips_a_large_reply_and_delivers_the_last_reply_on_shutdown() {
        let pid = std::process::id();
        let owner = unsafe { GetCurrentThreadId() };
        unsafe { PeekMessageW(&mut MSG::default(), null_mut(), 0, 0, PM_NOREMOVE) };
        let server = Server::listen(owner, pid).unwrap();
        let reply = "совпадение".repeat(4000);
        let request = || {
            let until = Instant::now() + IO_WAIT;
            loop {
                if let Some(r) = server.try_recv() {
                    return r;
                }
                assert!(Instant::now() < until, "No request reached the primary");
                thread::yield_now();
            }
        };
        let client = thread::spawn(move || {
            let pipe = connect(pid, &pipe_name(pid)).unwrap();
            transact(&pipe, "status").unwrap().unwrap()
        });
        let received = request();
        assert_eq!(received.command, "status");
        received.respond(Ok(reply.clone()));
        assert_eq!(client.join().unwrap(), reply);

        // A control request can remain in the desktop queue after its
        // terminal is closed. It must expire before a subsequent shell move.
        let name = pipe_name(pid);
        unsafe { WaitNamedPipeW(name.as_ptr(), IO_WAIT.as_millis() as u32) };
        let abandoned = connect(pid, &name).unwrap();
        abandoned
            .write(b"desktop next", IO_WAIT, null_mut())
            .unwrap();
        let received = request();
        assert!(!received.expired());
        drop(abandoned);
        let until = Instant::now() + IO_WAIT;
        while !received.expired() {
            assert!(
                Instant::now() < until,
                "Disconnected client remained active"
            );
            thread::sleep(Duration::from_millis(5));
        }

        let client = thread::spawn(move || {
            let name = pipe_name(pid);
            unsafe { WaitNamedPipeW(name.as_ptr(), IO_WAIT.as_millis() as u32) };
            let pipe = connect(pid, &name).unwrap();
            transact(&pipe, "quit").unwrap()
        });
        let received = request();
        assert_eq!(received.command, "quit");
        received.respond(Ok("bye".into()));
        drop(server);
        assert_eq!(client.join().unwrap(), Ok("bye".into()));
    }
}
