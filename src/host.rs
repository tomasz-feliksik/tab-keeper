//! `tab-keeper host`: relays requests between a Unix socket and one extension.
//!
//! Each client connects, sends one framed JSON object and gets one framed answer.
//! A client that sees the connection end without an answer has no answer: that is
//! a failure, never an empty result. The relay reads only an answer's `id`, because
//! the rest holds titles and URLs written by websites, and logs none of it.
//!
//! Stdout belongs to Chrome, so nothing here prints to it.

mod socket;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::frame::{
    FrameError, MAX_FROM_EXTENSION, MAX_TO_EXTENSION, encode_frame, read_frame, write_frame,
};

/// Stops a persistent `accept` failure, such as running out of file descriptors,
/// from spinning.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

pub const INSTANCE_LEN: usize = 32;

/// How long the relay waits on the things that can stall it.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// For a client to send its request.
    pub read_timeout: Duration,
    /// For the extension to answer.
    pub answer_timeout: Duration,
    /// For one write to a client that is not reading.
    pub write_timeout: Duration,
    /// Clients served at once. Each one holds a thread.
    pub max_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            read_timeout: Duration::from_secs(5),
            answer_timeout: Duration::from_secs(60),
            write_timeout: Duration::from_secs(5),
            max_connections: 64,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("XDG_RUNTIME_DIR must be set to an absolute path")]
    NoRuntimeDir,
    #[error("the extension closed the connection before saying hello")]
    NoHello,
    #[error("the extension's first message is not a valid hello")]
    BadHello,
    #[error("{0} is not a private directory: it must be a real directory with mode 0700")]
    UnsafeDirectory(PathBuf),
    #[error("{0} exists and is not a socket, so it was left alone")]
    PathOccupied(PathBuf),
    #[error("setting up the socket at {path}")]
    Socket {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("starting a relay thread")]
    Thread(#[source] std::io::Error),
    #[error("reading from the extension")]
    FromExtension(#[source] FrameError),
}

/// Why a client gets no answer from the extension. Sent to that client as data.
#[derive(Debug, thiserror::Error)]
enum Refusal {
    #[error("the request did not arrive in time or was cut off")]
    UnreadableRequest,
    #[error("the request is too large")]
    TooLarge,
    #[error("the request is not a JSON object")]
    NotAnObject,
    #[error("the extension did not answer in time")]
    NoAnswer,
    #[error("the extension is not connected")]
    Disconnected,
    #[error("too many clients are connected")]
    TooManyClients,
}

impl Refusal {
    fn to_frame_body(&self) -> Vec<u8> {
        json!({"ok": false, "error": self.to_string()})
            .to_string()
            .into_bytes()
    }
}

/// An extension's random instance id, safe to use in a file name because it can
/// only be hex digits.
#[derive(Debug)]
pub struct InstanceId(String);

impl InstanceId {
    fn parse(value: String) -> Option<Self> {
        is_instance_id(&value).then_some(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn is_instance_id(value: &str) -> bool {
    value.len() == INSTANCE_LEN
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The socket directory, if there is one yet, as long as it is private.
pub fn existing_socket_directory(runtime_dir: &Path) -> Result<Option<PathBuf>, HostError> {
    socket::existing_private_directory(runtime_dir)
}

#[derive(Deserialize)]
struct Hello {
    #[serde(rename = "type")]
    kind: String,
    instance: String,
}

fn parse_hello(body: &[u8]) -> Result<InstanceId, HostError> {
    let hello: Hello = serde_json::from_slice(body).map_err(|_| HostError::BadHello)?;
    if hello.kind != "hello" {
        return Err(HostError::BadHello);
    }
    InstanceId::parse(hello.instance).ok_or(HostError::BadHello)
}

/// The one field the relay reads from an answer. Everything else is skipped rather
/// than decoded, so routing costs no memory for a large list and does not depend on
/// what the titles and URLs in it contain.
#[derive(Deserialize)]
struct AnswerRoute {
    id: u64,
}

/// `$XDG_RUNTIME_DIR`, which the specification requires to be absolute. There is no
/// fallback: this path decides who can reach the extension, so it is never guessed.
pub fn runtime_dir(value: Option<OsString>) -> Result<PathBuf, HostError> {
    value
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(HostError::NoRuntimeDir)
}

pub fn run_from_environment() -> Result<(), HostError> {
    let runtime_dir = runtime_dir(std::env::var_os("XDG_RUNTIME_DIR"))?;
    run(
        &runtime_dir,
        Limits::default(),
        std::io::stdin().lock(),
        std::io::stdout(),
    )
}

/// Relays until the extension's side closes. Returning leaves the accept thread and
/// any clients' threads behind, which is fine because the process is about to exit.
pub fn run(
    runtime_dir: &Path,
    limits: Limits,
    mut from_extension: impl Read,
    to_extension: impl Write + Send + 'static,
) -> Result<(), HostError> {
    let hello = read_frame(&mut from_extension, MAX_FROM_EXTENSION)
        .map_err(HostError::FromExtension)?
        .ok_or(HostError::NoHello)?;
    let instance = parse_hello(&hello)?;
    let (bound_socket, listener) = socket::bind(runtime_dir, &instance)?;

    let (frames, outgoing) = mpsc::channel();
    let relay = Arc::new(Relay {
        limits,
        waiting: Mutex::default(),
        next_id: AtomicU64::new(1),
        connections: AtomicUsize::new(0),
        to_extension: frames,
    });
    start_thread("writer", move || write_frames(&outgoing, to_extension))?;
    let acceptor = Arc::clone(&relay);
    start_thread("acceptor", move || acceptor.accept_clients(&listener))?;

    let outcome = relay.route_answers(&mut from_extension);
    relay.fail_waiting_clients();
    drop(bound_socket);
    outcome
}

fn start_thread(name: &str, work: impl FnOnce() + Send + 'static) -> Result<(), HostError> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
        .map(drop)
        .map_err(HostError::Thread)
}

/// The only thing that writes to the extension, so frames never interleave and a
/// stuck Chrome cannot hold up a client's own timeout.
fn write_frames(outgoing: &mpsc::Receiver<Vec<u8>>, mut to_extension: impl Write) {
    for frame in outgoing {
        let written = to_extension
            .write_all(&frame)
            .and_then(|()| to_extension.flush());
        if let Err(error) = written {
            // Dropping the receiver tells every later client it is not connected.
            eprintln!("tab-keeper: writing to the extension: {error}");
            return;
        }
    }
}

type AnswerSlot = Sender<Vec<u8>>;

struct Relay {
    limits: Limits,
    /// One entry per request the extension has not answered yet.
    waiting: Mutex<HashMap<u64, AnswerSlot>>,
    /// Only has to be unique, so it needs no ordering with anything else.
    next_id: AtomicU64,
    /// A count and nothing more, so it needs no ordering with anything else.
    connections: AtomicUsize,
    to_extension: Sender<Vec<u8>>,
}

/// Frees a connection slot however the connection's thread ends.
struct ConnectionSlot(Arc<Relay>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Relay {
    /// Each map operation is one insert or remove, which a panic elsewhere cannot
    /// leave half done, so a poisoned lock still holds a sound map.
    fn waiting(&self) -> MutexGuard<'_, HashMap<u64, AnswerSlot>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn accept_clients(self: &Arc<Self>, listener: &UnixListener) {
        for client in listener.incoming() {
            match client {
                Ok(client) => self.admit(client),
                Err(error) => {
                    eprintln!("tab-keeper: accepting a client: {error}");
                    thread::sleep(ACCEPT_RETRY_DELAY);
                }
            }
        }
    }

    /// Never blocks on the client: a silent one must not hold up the next.
    fn admit(self: &Arc<Self>, client: UnixStream) {
        let admitted =
            self.connections.fetch_add(1, Ordering::Relaxed) < self.limits.max_connections;
        // Created before the check's outcome is used, so a refusal gives the slot back too.
        let slot = ConnectionSlot(Arc::clone(self));
        if !admitted {
            self.refuse(client);
            return;
        }
        let spawned = thread::Builder::new()
            .name("client".to_owned())
            .spawn(move || slot.0.serve(client));
        if let Err(error) = spawned {
            eprintln!("tab-keeper: starting a client thread: {error}");
        }
    }

    fn refuse(&self, mut client: UnixStream) {
        let sent = client
            .set_write_timeout(Some(self.limits.write_timeout))
            .map_err(FrameError::from)
            .and_then(|()| {
                write_frame(
                    &mut client,
                    &Refusal::TooManyClients.to_frame_body(),
                    MAX_FROM_EXTENSION,
                )
            });
        log_client_write(sent);
    }

    fn serve(&self, mut client: UnixStream) {
        let reply = match self.answer_for(&mut client) {
            Ok(Some(answer)) => answer,
            Ok(None) => return,
            Err(refusal) => refusal.to_frame_body(),
        };
        log_client_write(write_frame(&mut client, &reply, MAX_FROM_EXTENSION));
    }

    /// The extension's answer to this client's request, or `None` if the client
    /// hung up without asking anything.
    fn answer_for(&self, client: &mut UnixStream) -> Result<Option<Vec<u8>>, Refusal> {
        client
            .set_read_timeout(Some(self.limits.read_timeout))
            .and_then(|()| client.set_write_timeout(Some(self.limits.write_timeout)))
            .map_err(|_| Refusal::UnreadableRequest)?;
        let request = match read_frame(client, MAX_TO_EXTENSION) {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(None),
            Err(FrameError::TooLarge { .. }) => return Err(Refusal::TooLarge),
            Err(FrameError::Truncated | FrameError::Io(_)) => {
                return Err(Refusal::UnreadableRequest);
            }
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = stamped_frame(&request, id)?;

        let (slot, answer) = mpsc::channel();
        self.waiting().insert(id, slot);
        if self.to_extension.send(frame).is_err() {
            self.waiting().remove(&id);
            return Err(Refusal::Disconnected);
        }
        match answer.recv_timeout(self.limits.answer_timeout) {
            Ok(answer) => Ok(Some(answer)),
            Err(RecvTimeoutError::Timeout) => {
                self.waiting().remove(&id);
                Err(Refusal::NoAnswer)
            }
            Err(RecvTimeoutError::Disconnected) => Err(Refusal::Disconnected),
        }
    }

    /// Hands each answer to the client waiting for it, until the extension closes.
    /// A frame error leaves no way to find the next frame, so it ends the relay. A
    /// frame that cannot be routed is only that frame's problem: its client times out.
    fn route_answers(&self, from_extension: &mut impl Read) -> Result<(), HostError> {
        while let Some(answer) =
            read_frame(from_extension, MAX_FROM_EXTENSION).map_err(HostError::FromExtension)?
        {
            let Ok(route) = serde_json::from_slice::<AnswerRoute>(&answer) else {
                eprintln!("tab-keeper: ignoring a message with no usable id");
                continue;
            };
            let slot = self.waiting().remove(&route.id);
            let delivered = slot.is_some_and(|slot| slot.send(answer).is_ok());
            if !delivered {
                eprintln!("tab-keeper: no client is waiting for answer {}", route.id);
            }
        }
        Ok(())
    }

    /// Dropping the slots tells every waiting client the extension has gone.
    fn fail_waiting_clients(&self) {
        self.waiting().clear();
    }
}

/// The request with the relay's own id, which replaces any id the client sent, so
/// a client cannot claim another client's answer.
fn stamped_frame(request: &[u8], id: u64) -> Result<Vec<u8>, Refusal> {
    let mut request: Value = serde_json::from_slice(request).map_err(|_| Refusal::NotAnObject)?;
    request
        .as_object_mut()
        .ok_or(Refusal::NotAnObject)?
        .insert("id".to_owned(), id.into());
    // Checked here, with the id included: over the limit, Chrome closes the port.
    encode_frame(request.to_string().as_bytes(), MAX_TO_EXTENSION).map_err(|_| Refusal::TooLarge)
}

/// A client that left has nobody to tell, and the relay carries on regardless.
fn log_client_write(written: Result<(), FrameError>) {
    if let Err(error) = written {
        eprintln!("tab-keeper: answering a client: {error}");
    }
}
