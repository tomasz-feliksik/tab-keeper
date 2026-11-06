//! The relay, in process: Unix socket pairs stand in for Chrome's pipes, and short
//! limits stand in for the real timeouts.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::{HostError, Limits, run, runtime_dir};
use crate::frame::{FrameError, MAX_FROM_EXTENSION, MAX_TO_EXTENSION, read_frame, write_frame};

/// Long enough never to fire on a healthy run, short enough that a broken relay
/// fails its test rather than hanging it.
const PATIENCE: Duration = Duration::from_secs(10);
const INSTANCE: &str = "0123456789abcdef0123456789abcdef";
const OTHER_INSTANCE: &str = "fedcba9876543210fedcba9876543210";
const NO_ENTRIES: [&str; 0] = [];

fn patient_limits() -> Limits {
    Limits {
        read_timeout: PATIENCE,
        answer_timeout: PATIENCE,
        write_timeout: PATIENCE,
        max_connections: 8,
    }
}

/// A short path under /tmp: a socket path is limited to about 108 bytes.
fn runtime() -> TempDir {
    tempfile::tempdir_in("/tmp").expect("a temporary directory")
}

fn socket_path(runtime: &Path) -> PathBuf {
    runtime.join("tab-keeper").join(format!("{INSTANCE}.sock"))
}

fn pipe() -> (UnixStream, UnixStream) {
    let (ours, theirs) = UnixStream::pair().expect("a socket pair");
    ours.set_read_timeout(Some(PATIENCE))
        .expect("a read timeout");
    (ours, theirs)
}

/// A running relay and the extension end of its pipes.
struct Extension {
    to_host: UnixStream,
    from_host: UnixStream,
    host: JoinHandle<Result<(), HostError>>,
}

impl Extension {
    fn start(runtime: &Path, limits: Limits) -> Self {
        let extension = Self::spawn(runtime, limits);
        extension.say(&json!({"type": "hello", "instance": INSTANCE}));
        extension
    }

    /// Starts a host, then waits for this host's socket to take the path.
    fn start_ready(runtime: &Path, limits: Limits) -> Self {
        let before = identity(&socket_path(runtime));
        let extension = Self::start(runtime, limits);
        let deadline = Instant::now() + PATIENCE;
        loop {
            let now = identity(&socket_path(runtime));
            if now.is_some() && now != before {
                return extension;
            }
            assert!(
                Instant::now() < deadline && !extension.host.is_finished(),
                "the host never took the socket path"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn spawn(runtime: &Path, limits: Limits) -> Self {
        let (to_host, host_stdin) = pipe();
        let (from_host, host_stdout) = pipe();
        let runtime = runtime.to_path_buf();
        let host = thread::spawn(move || run(&runtime, limits, host_stdin, host_stdout));
        Self {
            to_host,
            from_host,
            host,
        }
    }

    fn say(&self, message: &Value) {
        self.send_raw(&serde_json::to_vec(message).expect("JSON"));
    }

    fn send_raw(&self, body: &[u8]) {
        let mut stream = &self.to_host;
        write_frame(&mut stream, body, u32::MAX).expect("the host's stdin is open");
    }

    fn next_request(&self) -> Value {
        let mut stream = &self.from_host;
        let body = read_frame(&mut stream, u32::MAX)
            .expect("a request from the host")
            .expect("the host's stdout is open");
        serde_json::from_slice(&body).expect("the host sends JSON")
    }

    fn answer(&self, request: &Value, result: &Value) {
        self.say(&json!({"id": request["id"], "ok": true, "result": result}));
    }

    /// Closes stdin as Chrome would, and returns how the host finished.
    fn close(self) -> Result<(), HostError> {
        drop(self.to_host);
        self.host.join().expect("the host does not panic")
    }
}

fn identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

fn connect(path: &Path) -> UnixStream {
    let client = UnixStream::connect(path).expect("the socket accepts connections");
    client
        .set_read_timeout(Some(PATIENCE))
        .expect("a read timeout");
    client
        .set_write_timeout(Some(PATIENCE))
        .expect("a write timeout");
    client
}

fn request(client: &mut UnixStream, message: &Value) {
    let body = serde_json::to_vec(message).expect("JSON");
    write_frame(client, &body, u32::MAX).expect("the request is sent");
}

fn response_bytes(client: &mut UnixStream) -> Vec<u8> {
    read_frame(client, u32::MAX)
        .expect("a response frame")
        .expect("the host answered before closing")
}

fn response(client: &mut UnixStream) -> Value {
    serde_json::from_slice(&response_bytes(client)).expect("the host sends JSON")
}

fn assert_failure(answer: &Value) {
    assert_eq!(
        answer["ok"],
        json!(false),
        "expected a failure, got {answer}"
    );
    assert!(answer["error"].is_string(), "a failure says why: {answer}");
}

/// A full request through the relay, proving it still works.
fn assert_relay_works(extension: &Extension, path: &Path) {
    let mut client = connect(path);
    request(&mut client, &json!({"op": "ping"}));
    let seen = extension.next_request();
    assert_eq!(seen["op"], json!("ping"));
    extension.answer(&seen, &json!("pong"));
    assert_eq!(response(&mut client)["result"], json!("pong"));
}

fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .map(|entries| {
            entries
                .map(|entry| {
                    entry
                        .expect("an entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

// AC4, the Chrome limit and our own bound, through the relay.

/// A request whose JSON is exactly `size` bytes once the host stamps `"id":1` on it.
fn request_filling(size: usize) -> Value {
    let overhead = br#"{"id":1,"pad":""}"#.len();
    json!({"pad": "x".repeat(size - overhead)})
}

#[test]
fn request_filling_the_chrome_limit_reaches_the_extension() {
    let runtime = runtime();
    let extension = Extension::start_ready(runtime.path(), patient_limits());
    let mut client = connect(&socket_path(runtime.path()));

    request(&mut client, &request_filling(MAX_TO_EXTENSION as usize));

    let seen = extension.next_request();
    assert_eq!(
        serde_json::to_vec(&seen).expect("JSON").len(),
        MAX_TO_EXTENSION as usize
    );
    assert_eq!(seen["id"], json!(1));
}

#[test]
fn request_over_the_chrome_limit_once_stamped_is_refused() {
    let runtime = runtime();
    let path = socket_path(runtime.path());
    let extension = Extension::start_ready(runtime.path(), patient_limits());
    let mut client = connect(&path);

    // Within the limit as the client sent it, and one byte over with the id added.
    request(&mut client, &request_filling(MAX_TO_EXTENSION as usize + 1));

    assert_failure(&response(&mut client));
    assert_relay_works(&extension, &path);
}

#[test]
fn large_answer_reaches_the_client() {
    let runtime = runtime();
    let extension = Extension::start_ready(runtime.path(), patient_limits());
    let mut client = connect(&socket_path(runtime.path()));
    request(&mut client, &json!({"op": "list"}));
    let seen = extension.next_request();
    let result = "x".repeat(8 << 20);

    extension.answer(&seen, &json!(result));

    assert_eq!(response(&mut client)["result"], json!(result));
}

#[test]
fn answer_over_the_limit_stops_the_host() {
    let runtime = runtime();
    let path = socket_path(runtime.path());
    let extension = Extension::start_ready(runtime.path(), patient_limits());

    let mut stdin = &extension.to_host;
    stdin
        .write_all(&(MAX_FROM_EXTENSION + 1).to_ne_bytes())
        .expect("the host's stdin is open");

    let outcome = extension.close();
    assert!(matches!(
        outcome,
        Err(HostError::FromExtension(FrameError::TooLarge { .. }))
    ));
    assert_eq!(identity(&path), None);
}

// AC5: the hello.

#[test]
fn bad_hello_exits_without_a_socket() {
    let bad_hellos: [&[u8]; 7] = [
        b"not json",
        br#"{"type":"hello"}"#,
        br#"{"type":"hello","instance":"../../../../tmp/evil0000000000000"}"#,
        br#"{"type":"hello","instance":"0123456789abcdef0123456789abcde"}"#,
        br#"{"type":"hello","instance":"0123456789ABCDEF0123456789ABCDEF"}"#,
        br#"{"type":"hello","instance":"0123456789abcdef0123456789abcdef0"}"#,
        br#"{"type":"other","instance":"0123456789abcdef0123456789abcdef"}"#,
    ];
    for hello in bad_hellos {
        let runtime = runtime();
        let extension = Extension::spawn(runtime.path(), patient_limits());

        extension.send_raw(hello);

        let outcome = extension.close();
        assert!(
            matches!(outcome, Err(HostError::BadHello)),
            "{}: {outcome:?}",
            String::from_utf8_lossy(hello)
        );
        assert_eq!(entries(runtime.path()), NO_ENTRIES, "nothing is created");
    }
}

#[test]
fn eof_before_hello_is_an_error() {
    let runtime = runtime();
    let extension = Extension::spawn(runtime.path(), patient_limits());

    let outcome = extension.close();

    assert!(matches!(outcome, Err(HostError::NoHello)));
    assert_eq!(entries(runtime.path()), NO_ENTRIES);
}

// AC7: the directory.

#[test]
fn socket_lives_in_a_private_directory() {
    let runtime = runtime();
    let path = socket_path(runtime.path());

    let extension = Extension::start_ready(runtime.path(), patient_limits());

    let directory = path.parent().expect("a parent");
    let mode = fs::symlink_metadata(directory)
        .expect("the directory")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    assert_relay_works(&extension, &path);
}

#[test]
fn loose_directory_is_refused() {
    let runtime = runtime();
    let directory = runtime.path().join("tab-keeper");
    fs::create_dir(&directory).expect("the directory");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).expect("the mode");

    let extension = Extension::start(runtime.path(), patient_limits());

    let outcome = extension.close();
    assert!(matches!(outcome, Err(HostError::UnsafeDirectory(_))));
    assert_eq!(entries(&directory), NO_ENTRIES, "nothing is bound");
    let mode = fs::metadata(&directory)
        .expect("the directory")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755, "the directory is left as it was");
}

#[test]
fn symlinked_directory_is_refused() {
    let runtime = runtime();
    let elsewhere = runtime.path().join("elsewhere");
    fs::create_dir(&elsewhere).expect("the target");
    fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o700)).expect("the mode");
    symlink(&elsewhere, runtime.path().join("tab-keeper")).expect("the symlink");

    let extension = Extension::start(runtime.path(), patient_limits());

    let outcome = extension.close();
    assert!(matches!(outcome, Err(HostError::UnsafeDirectory(_))));
    assert_eq!(entries(&elsewhere), NO_ENTRIES, "nothing is bound");
}

// AC8: the runtime directory's value.

#[test]
fn runtime_dir_must_be_absolute() {
    for bad in [None, Some(""), Some("relative/path")] {
        let outcome = runtime_dir(bad.map(Into::into));

        assert!(matches!(outcome, Err(HostError::NoRuntimeDir)), "{bad:?}");
    }
    let good = runtime_dir(Some("/run/user/1000".into())).expect("an absolute path");
    assert_eq!(good, Path::new("/run/user/1000"));
}

// AC9, AC10, AC11: replacing a socket.

fn prepare_private_directory(runtime: &Path) -> PathBuf {
    let directory = runtime.join("tab-keeper");
    fs::create_dir(&directory).expect("the directory");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).expect("the mode");
    directory
}

#[test]
fn stale_socket_is_replaced_without_leftovers() {
    let runtime = runtime();
    let directory = prepare_private_directory(runtime.path());
    let path = socket_path(runtime.path());
    drop(UnixListener::bind(&path).expect("a socket from another run"));

    let extension = Extension::start_ready(runtime.path(), patient_limits());

    assert_relay_works(&extension, &path);
    assert_eq!(entries(&directory), [format!("{INSTANCE}.sock")]);
}

#[test]
fn regular_file_at_socket_path_is_left_alone() {
    let runtime = runtime();
    prepare_private_directory(runtime.path());
    let path = socket_path(runtime.path());
    fs::write(&path, "precious").expect("a regular file");

    let extension = Extension::start(runtime.path(), patient_limits());

    assert!(matches!(extension.close(), Err(HostError::PathOccupied(_))));
    assert_eq!(fs::read_to_string(&path).expect("the file"), "precious");
}

#[test]
fn symlink_at_socket_path_is_left_alone() {
    let runtime = runtime();
    prepare_private_directory(runtime.path());
    let path = socket_path(runtime.path());
    let target = runtime.path().join("target");
    fs::write(&target, "precious").expect("the target");
    symlink(&target, &path).expect("the symlink");

    let extension = Extension::start(runtime.path(), patient_limits());

    assert!(matches!(extension.close(), Err(HostError::PathOccupied(_))));
    assert!(
        fs::symlink_metadata(&path)
            .expect("the link")
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).expect("the target"), "precious");
}

#[test]
fn old_host_exiting_keeps_the_replacements_socket() {
    let runtime = runtime();
    let path = socket_path(runtime.path());
    let old = Extension::start_ready(runtime.path(), patient_limits());
    let new = Extension::start_ready(runtime.path(), patient_limits());

    old.close().expect("the old host exits cleanly");

    assert!(
        identity(&path).is_some(),
        "the replacement's socket is still there"
    );
    assert_relay_works(&new, &path);
}

#[test]
fn hosts_for_different_instances_keep_separate_sockets() {
    let runtime = runtime();
    let first = Extension::start_ready(runtime.path(), patient_limits());
    let second = Extension::spawn(runtime.path(), patient_limits());
    let second_path = runtime
        .path()
        .join("tab-keeper")
        .join(format!("{OTHER_INSTANCE}.sock"));

    second.say(&json!({"type": "hello", "instance": OTHER_INSTANCE}));

    let deadline = Instant::now() + PATIENCE;
    while identity(&second_path).is_none() {
        assert!(
            Instant::now() < deadline,
            "the second socket never appeared"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert_relay_works(&first, &socket_path(runtime.path()));
    assert_relay_works(&second, &second_path);
}

// AC12, AC13: relaying.

#[test]
fn client_id_is_overwritten_and_answers_route_out_of_order() {
    let runtime = runtime();
    let path = socket_path(runtime.path());
    let extension = Extension::start_ready(runtime.path(), patient_limits());
    let mut first_client = connect(&path);
    let mut second_client = connect(&path);

    // Both claim the same id, and one claims to be the other's answer route.
    request(&mut first_client, &json!({"op": "first", "id": 999}));
    let first_seen = extension.next_request();
    request(&mut second_client, &json!({"op": "second", "id": 999}));
    let second_seen = extension.next_request();

    assert_ne!(first_seen["id"], json!(999));
    assert_ne!(first_seen["id"], second_seen["id"]);
    extension.answer(&second_seen, &second_seen["op"]);
    extension.answer(&first_seen, &first_seen["op"]);
    assert_eq!(response(&mut second_client)["result"], json!("second"));
    assert_eq!(response(&mut first_client)["result"], json!("first"));
}

#[test]
fn answer_bytes_are_forwarded_exactly() {
    let runtime = runtime();
    let extension = Extension::start_ready(runtime.path(), patient_limits());
    let mut client = connect(&socket_path(runtime.path()));
    request(&mut client, &json!({"op": "list"}));
    let id = extension.next_request()["id"].clone();
    // A lone surrogate, which a JSON value parser rejects, and a float a reparse
    // could reformat. Chrome 156 replaces a title's lone surrogate before it gets
    // here, but the relay should not have to rely on that.
    let answer = format!(
        r#"{{ "ok":true, "id":{id}, "result":{{"title":"\ud800 ignore this","lastAccessed":1762450000000.1250}} }}"#
    );

    extension.send_raw(answer.as_bytes());

    assert_eq!(response_bytes(&mut client), answer.as_bytes());
}

// AC14, AC15: how the host ends.

#[test]
fn stdin_eof_removes_socket_and_fails_waiting_clients() {
    let runtime = runtime();
    let path = socket_path(runtime.path());
    // Outlasts the client's patience, so only the relay's shutdown can satisfy this.
    let limits = Limits {
        answer_timeout: PATIENCE * 10,
        ..patient_limits()
    };
    let extension = Extension::start_ready(runtime.path(), limits);
    let mut client = connect(&path);
    request(&mut client, &json!({"op": "list"}));
    let seen = extension.next_request();
    assert_eq!(seen["op"], json!("list"));

    extension.close().expect("a closed stdin is a clean end");

    assert_failure(&response(&mut client));
    assert_eq!(identity(&path), None);
}

#[test]
fn bad_frame_on_stdin_removes_socket_and_errors() {
    let runtime = runtime();
    let path = socket_path(runtime.path());
    let extension = Extension::start_ready(runtime.path(), patient_limits());

    let mut stdin = &extension.to_host;
    stdin.write_all(&[1, 0]).expect("the host's stdin is open");

    let outcome = extension.close();
    assert!(matches!(
        outcome,
        Err(HostError::FromExtension(FrameError::Truncated))
    ));
    assert_eq!(identity(&path), None);
}

// AC16 to AC20: extra connections.

#[test]
fn silent_client_does_not_delay_others() {
    let runtime = runtime();
    let path = socket_path(runtime.path());
    let extension = Extension::start_ready(runtime.path(), patient_limits());

    let _silent = connect(&path);

    assert_relay_works(&extension, &path);
}

#[test]
fn silent_client_is_dropped_after_timeout() {
    let runtime = runtime();
    let limits = Limits {
        read_timeout: Duration::from_millis(50),
        ..patient_limits()
    };
    let path = socket_path(runtime.path());
    let extension = Extension::start_ready(runtime.path(), limits);
    let mut silent = connect(&path);

    let answer = response(&mut silent);

    assert_failure(&answer);
    assert_relay_works(&extension, &path);
}

#[test]
fn client_leaving_before_its_answer_leaves_relay_working() {
    let runtime = runtime();
    let path = socket_path(runtime.path());
    let extension = Extension::start_ready(runtime.path(), patient_limits());
    let mut client = connect(&path);
    request(&mut client, &json!({"op": "list"}));
    let seen = extension.next_request();

    drop(client);
    extension.answer(&seen, &json!("nobody is listening"));

    assert_relay_works(&extension, &path);
}

#[test]
fn answer_without_a_waiting_client_is_ignored() {
    let runtime = runtime();
    let path = socket_path(runtime.path());
    let extension = Extension::start_ready(runtime.path(), patient_limits());

    extension.say(&json!({"id": 424_242, "ok": true, "result": "unknown id"}));
    extension.say(&json!({"ok": true, "result": "no id at all"}));
    extension.send_raw(b"not json");

    assert_relay_works(&extension, &path);
}

#[test]
fn bad_requests_fail_only_that_client() {
    let runtime = runtime();
    let path = socket_path(runtime.path());
    let extension = Extension::start_ready(runtime.path(), patient_limits());
    let bad_requests: [&[u8]; 3] = [b"not json", b"[1,2,3]", b"\"a string\""];

    for bad in bad_requests {
        let mut client = connect(&path);
        write_frame(&mut client, bad, u32::MAX).expect("the request is sent");
        assert_failure(&response(&mut client));
    }
    let mut oversized = connect(&path);
    oversized
        .write_all(&(MAX_TO_EXTENSION + 1).to_ne_bytes())
        .expect("the header is sent");
    assert_failure(&response(&mut oversized));

    // The first thing the extension sees is this request: nothing bad got through.
    assert_relay_works(&extension, &path);
}

#[test]
fn unanswered_request_fails_and_late_answer_is_ignored() {
    let runtime = runtime();
    let limits = Limits {
        answer_timeout: Duration::from_millis(50),
        ..patient_limits()
    };
    let path = socket_path(runtime.path());
    let extension = Extension::start_ready(runtime.path(), limits);
    let mut client = connect(&path);
    request(&mut client, &json!({"op": "list"}));
    let seen = extension.next_request();

    assert_failure(&response(&mut client));

    extension.answer(&seen, &json!("too late"));
    assert_relay_works(&extension, &path);
}

#[test]
fn connection_over_the_limit_is_refused_until_a_slot_frees() {
    let runtime = runtime();
    let limits = Limits {
        max_connections: 2,
        ..patient_limits()
    };
    let path = socket_path(runtime.path());
    let extension = Extension::start_ready(runtime.path(), limits);
    let mut holders = vec![connect(&path), connect(&path)];
    // A request parked at the extension proves each holder has been admitted.
    let mut parked = Vec::new();
    for holder in &mut holders {
        request(holder, &json!({"op": "hold"}));
        parked.push(extension.next_request());
    }

    let mut refused = connect(&path);
    assert_failure(&response(&mut refused));

    for (holder, seen) in holders.iter_mut().zip(&parked) {
        extension.answer(seen, &json!("released"));
        assert_eq!(response(holder)["result"], json!("released"));
    }
    drop(holders);
    // A refused client never reaches the extension, so retry until one does.
    extension
        .from_host
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("a read timeout");
    let deadline = Instant::now() + PATIENCE;
    let mut clients = Vec::new();
    loop {
        let mut client = connect(&path);
        let body = serde_json::to_vec(&json!({"op": "after"})).expect("JSON");
        if write_frame(&mut client, &body, u32::MAX).is_ok() {
            clients.push(client);
            let mut stream = &extension.from_host;
            if let Ok(Some(seen)) = read_frame(&mut stream, u32::MAX) {
                let seen: Value = serde_json::from_slice(&seen).expect("JSON");
                assert_eq!(seen["op"], json!("after"));
                return;
            }
        }
        assert!(Instant::now() < deadline, "no slot ever freed");
    }
}
