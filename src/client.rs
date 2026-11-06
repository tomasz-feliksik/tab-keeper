//! Asking the hosts: finding their sockets and exchanging one request with each.

use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::frame::{FrameError, MAX_FROM_EXTENSION, MAX_TO_EXTENSION, read_frame, write_frame};
use crate::host::{self, HostError};

/// A socket a host is, or was, listening on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Socket {
    pub instance: String,
    pub path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("its host is no longer running")]
    HostGone,
    #[error("it did not answer in time")]
    TimedOut,
    #[error("the connection ended without an answer")]
    NoAnswer,
    #[error("the connection failed")]
    Connection(#[source] io::Error),
    #[error("the answer was cut off or too large")]
    BadFrame(#[source] FrameError),
    #[error("the answer was not understood")]
    BadAnswer(#[source] serde_json::Error),
    #[error("it refused: {0}")]
    Refused(String),
}

/// Every socket in the private directory. A directory that is not private is refused,
/// because anyone able to write there could stand in for a profile.
pub fn sockets(runtime_dir: &Path) -> Result<Vec<Socket>, HostError> {
    let Some(directory) = host::existing_socket_directory(runtime_dir)? else {
        return Ok(Vec::new());
    };
    let entries = fs::read_dir(&directory).map_err(|source| HostError::Socket {
        path: directory.clone(),
        source,
    })?;
    let mut found: Vec<Socket> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let instance = path
                .file_name()?
                .to_str()?
                .strip_suffix(".sock")?
                .to_owned();
            host::is_instance_id(&instance).then_some(Socket { instance, path })
        })
        .collect();
    found.sort_by(|a, b| a.instance.cmp(&b.instance));
    Ok(found)
}

/// One request and its answer. Only a complete `ok: true` frame is a result: anything
/// else is an error, so a failure can never look like an empty one.
pub fn ask(socket: &Path, request: &Value, timeout: Duration) -> Result<Value, AskError> {
    let mut stream = UnixStream::connect(socket).map_err(|error| match error.kind() {
        // Nothing listens there: the host died without cleaning up.
        ErrorKind::ConnectionRefused | ErrorKind::NotFound => AskError::HostGone,
        _ => AskError::Connection(error),
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(AskError::Connection)?;
    let body = serde_json::to_vec(request).map_err(AskError::BadAnswer)?;
    write_frame(&mut stream, &body, MAX_TO_EXTENSION).map_err(frame_error)?;
    let answer = read_frame(&mut stream, MAX_FROM_EXTENSION)
        .map_err(frame_error)?
        .ok_or(AskError::NoAnswer)?;
    let mut answer: Value = serde_json::from_slice(&answer).map_err(AskError::BadAnswer)?;
    if answer["ok"] == Value::Bool(true) {
        Ok(answer["result"].take())
    } else {
        Err(AskError::Refused(
            answer["error"]
                .as_str()
                .unwrap_or("no reason given")
                .to_owned(),
        ))
    }
}

fn frame_error(error: FrameError) -> AskError {
    match error {
        FrameError::Io(error)
            if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
        {
            AskError::TimedOut
        }
        // The host refused, and closed, before reading all of what was sent.
        FrameError::Io(error) if error.kind() == ErrorKind::BrokenPipe => AskError::NoAnswer,
        other => AskError::BadFrame(other),
    }
}

/// Asks every socket at once, so one stuck profile costs only its own timeout. A socket
/// nothing listens on is removed, having been reported this once.
pub fn ask_all(
    sockets: &[Socket],
    request: &Value,
    timeout: Duration,
) -> Vec<Result<Value, AskError>> {
    thread::scope(|scope| {
        let asking: Vec<_> = sockets
            .iter()
            .map(|socket| scope.spawn(move || ask(&socket.path, request, timeout)))
            .collect();
        asking
            .into_iter()
            .zip(sockets)
            .map(|(asked, socket)| {
                let answer = asked.join().unwrap_or(Err(AskError::NoAnswer));
                if matches!(answer, Err(AskError::HostGone)) {
                    remove_stale(&socket.path);
                }
                answer
            })
            .collect()
    })
}

fn remove_stale(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != ErrorKind::NotFound
    {
        eprintln!("tab-keeper: removing {}: {error}", path.display());
    }
}
