use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;
use std::thread;
use std::time::Duration;

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use sha2::{Digest, Sha256};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
};
use windows::Win32::Storage::FileSystem::{PIPE_ACCESS_INBOUND, ReadFile};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::{Error as WindowsError, HSTRING};

const PIPE_BUFFER_SIZE: u32 = 16 * 1024;
const CLIENT_RETRY_COUNT: usize = 100;
const CLIENT_RETRY_DELAY: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InstanceRequest {
    Activate,
    OpenPath(String),
}

impl InstanceRequest {
    fn from_payload(payload: &str) -> Self {
        let payload = payload.trim_end_matches(['\r', '\n']);
        if payload.is_empty() {
            Self::Activate
        } else {
            Self::OpenPath(payload.to_string())
        }
    }

    fn to_payload(&self) -> String {
        match self {
            Self::Activate => "\n".to_string(),
            Self::OpenPath(path) => format!("{path}\n"),
        }
    }
}

pub(crate) enum InstanceStart {
    Primary(SingleInstance),
    Secondary,
}

pub(crate) struct SingleInstance {
    _mutex: OwnedHandle,
    receiver: Option<UnboundedReceiver<InstanceRequest>>,
}

impl SingleInstance {
    pub(crate) fn start() -> Result<InstanceStart, String> {
        let key = instance_key();
        let mutex_name = HSTRING::from(format!("Local\\Kite.SingleInstance.{key}"));
        let mutex = unsafe { CreateMutexW(None, false, &mutex_name) }
            .map_err(|error| format!("无法创建 Kite 单实例锁：{error}"))?;
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                let _ = CloseHandle(mutex);
            }
            return Ok(InstanceStart::Secondary);
        }

        let (sender, receiver) = unbounded();
        spawn_pipe_server(pipe_name_for_key(&key), sender)?;
        Ok(InstanceStart::Primary(Self {
            _mutex: OwnedHandle(mutex),
            receiver: Some(receiver),
        }))
    }

    pub(crate) fn take_receiver(&mut self) -> Option<UnboundedReceiver<InstanceRequest>> {
        self.receiver.take()
    }

    pub(crate) fn forward(path: Option<&Path>) -> Result<(), String> {
        let request = path
            .map(|path| InstanceRequest::OpenPath(path.to_string_lossy().into_owned()))
            .unwrap_or(InstanceRequest::Activate);
        forward_to_pipe(&pipe_name(), &request)
    }
}

fn instance_key() -> String {
    let mut hasher = Sha256::new();
    for name in ["USERDOMAIN", "USERNAME", "SESSIONNAME", "USERPROFILE"] {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        if let Some(value) = std::env::var_os(name) {
            hasher.update(value.to_string_lossy().as_bytes());
        }
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())[..16].to_string()
}

fn pipe_name() -> String {
    pipe_name_for_key(&instance_key())
}

fn pipe_name_for_key(key: &str) -> String {
    format!(r"\\.\pipe\Kite.SingleInstance.{key}")
}

fn forward_to_pipe(pipe_name: &str, request: &InstanceRequest) -> Result<(), String> {
    let payload = request.to_payload();
    let mut last_error = None;
    for _ in 0..CLIENT_RETRY_COUNT {
        let result = (|| -> std::io::Result<()> {
            let mut pipe = OpenOptions::new().write(true).open(pipe_name)?;
            pipe.write_all(payload.as_bytes())?;
            pipe.flush()?;
            Ok(())
        })();
        match result {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        thread::sleep(CLIENT_RETRY_DELAY);
    }
    Err(format!(
        "无法连接已运行的 Kite 进程：{}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "命名管道不可用".to_string())
    ))
}

fn spawn_pipe_server(
    pipe_name: String,
    sender: UnboundedSender<InstanceRequest>,
) -> Result<(), String> {
    thread::Builder::new()
        .name("kite-instance-server".to_string())
        .spawn(move || {
            while !sender.is_closed() {
                let pipe = match create_pipe(&pipe_name) {
                    Ok(pipe) => pipe,
                    Err(_) => {
                        thread::sleep(CLIENT_RETRY_DELAY);
                        continue;
                    }
                };
                let connected = unsafe { ConnectNamedPipe(pipe.0, None) };
                let connect_error = unsafe { GetLastError() };
                if connected.is_err() && connect_error != ERROR_PIPE_CONNECTED {
                    thread::sleep(CLIENT_RETRY_DELAY);
                    continue;
                }
                let payload = read_pipe(pipe.0).unwrap_or_default();
                let request = InstanceRequest::from_payload(&payload);
                if sender.unbounded_send(request).is_err() {
                    break;
                }
                unsafe {
                    let _ = DisconnectNamedPipe(pipe.0);
                }
            }
        })
        .map(|_| ())
        .map_err(|error| format!("无法启动 Kite 单实例服务：{error}"))
}

fn create_pipe(pipe_name: &str) -> Result<OwnedHandle, String> {
    let pipe_name = HSTRING::from(pipe_name);
    let pipe = unsafe {
        CreateNamedPipeW(
            &pipe_name,
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            0,
            PIPE_BUFFER_SIZE,
            0,
            None,
        )
    };
    if pipe.is_invalid() {
        Err(WindowsError::from_thread().to_string())
    } else {
        Ok(OwnedHandle(pipe))
    }
}

fn read_pipe(pipe: HANDLE) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let mut read = 0u32;
        let result = unsafe { ReadFile(pipe, Some(&mut buffer), Some(&mut read), None) };
        if result.is_err() {
            if bytes.is_empty() {
                return Err(result
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "无法读取 Kite 实例请求".to_string()));
            }
            break;
        }
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read as usize]);
        if bytes.len() > PIPE_BUFFER_SIZE as usize {
            return Err("Kite 实例请求过长".to_string());
        }
    }
    String::from_utf8(bytes).map_err(|error| format!("Kite 实例请求不是 UTF-8：{error}"))
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_channel::mpsc::TryRecvError;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    #[test]
    fn request_payload_round_trips() {
        let path = r"D:\temp\Kite Folder";
        let request = InstanceRequest::OpenPath(path.to_string());
        assert_eq!(
            InstanceRequest::from_payload(&request.to_payload()),
            request
        );
        assert_eq!(
            InstanceRequest::from_payload(&InstanceRequest::Activate.to_payload()),
            InstanceRequest::Activate
        );
    }

    #[test]
    fn named_pipe_forwards_open_request() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pipe_name = pipe_name_for_key(&format!("test-{}-{unique}", std::process::id()));
        let (sender, mut receiver) = unbounded();
        spawn_pipe_server(pipe_name.clone(), sender).expect("start pipe server");
        let request = InstanceRequest::OpenPath(r"D:\temp\Kite Folder".to_string());

        forward_to_pipe(&pipe_name, &request).expect("forward request");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match receiver.try_recv() {
                Ok(received) => {
                    assert_eq!(received, request);
                    break;
                }
                Err(TryRecvError::Empty) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TryRecvError::Empty) => panic!("pipe request timed out"),
                Err(TryRecvError::Closed) => panic!("pipe request channel closed"),
            }
        }
    }
}
