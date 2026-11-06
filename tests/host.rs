//! `tab-keeper host` as Chrome runs it: a real process, with real pipes.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const PATIENCE: Duration = Duration::from_secs(10);
const INSTANCE: &str = "0123456789abcdef0123456789abcdef";

fn host(runtime: &Path, arguments: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_tab-keeper"))
        .arg("host")
        .args(arguments)
        .env("XDG_RUNTIME_DIR", runtime)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary runs")
}

fn frame(body: &[u8]) -> Vec<u8> {
    let length = u32::try_from(body.len()).expect("a small body");
    [&length.to_ne_bytes()[..], body].concat()
}

fn read_frame(stream: &mut impl Read) -> Vec<u8> {
    let mut header = [0; 4];
    stream.read_exact(&mut header).expect("a frame header");
    let mut body = vec![0; u32::from_ne_bytes(header) as usize];
    stream.read_exact(&mut body).expect("a frame body");
    body
}

/// A pipe has no read timeout, so the read gets its own thread. A host that never
/// writes then fails the test instead of hanging it.
fn next_frame_within_patience(mut stdout: ChildStdout) -> Vec<u8> {
    let (sender, frame) = mpsc::channel();
    thread::spawn(move || sender.send(read_frame(&mut stdout)));
    frame
        .recv_timeout(PATIENCE)
        .expect("the host writes a frame to stdout")
}

fn say_hello(child: &mut Child) {
    let hello = format!(r#"{{"type":"hello","instance":"{INSTANCE}"}}"#);
    let stdin = child.stdin.as_mut().expect("stdin is piped");
    stdin
        .write_all(&frame(hello.as_bytes()))
        .expect("the hello is sent");
    stdin.flush().expect("the hello is flushed");
}

fn socket_path(runtime: &Path) -> PathBuf {
    runtime.join("tab-keeper").join(format!("{INSTANCE}.sock"))
}

fn wait_for_socket(path: &Path) {
    let deadline = Instant::now() + PATIENCE;
    while !path.exists() {
        assert!(Instant::now() < deadline, "the socket never appeared");
        thread::sleep(Duration::from_millis(2));
    }
}

/// Waits for the process to end, killing it rather than hanging the test.
fn exit_status(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(status) = child.try_wait().expect("the status is readable") {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("the host can be killed");
            panic!("the host did not exit");
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn short_runtime() -> tempfile::TempDir {
    tempfile::tempdir_in("/tmp").expect("a temporary directory")
}

#[test]
fn accepts_chromes_origin_argument_and_relays_over_real_pipes() {
    let runtime = short_runtime();
    let mut child = host(runtime.path(), &["chrome-extension://abcdefghijklmnop/"]);
    say_hello(&mut child);
    let path = socket_path(runtime.path());
    wait_for_socket(&path);

    let mut client = UnixStream::connect(&path).expect("the socket accepts connections");
    client
        .set_read_timeout(Some(PATIENCE))
        .expect("a read timeout");
    client
        .write_all(&frame(br#"{"op":"list"}"#))
        .expect("the request is sent");
    let stdout = child.stdout.take().expect("stdout is piped");
    let seen = next_frame_within_patience(stdout);
    assert_eq!(seen, br#"{"id":1,"op":"list"}"#);
    let answer = br#"{"id":1,"ok":true,"result":[]}"#;
    let stdin = child.stdin.as_mut().expect("stdin is piped");
    stdin.write_all(&frame(answer)).expect("the answer is sent");
    stdin.flush().expect("the answer is flushed");

    assert_eq!(read_frame(&mut client), answer);
    child.kill().expect("the host can be killed");
    child.wait().expect("the host is reaped");
}

#[test]
fn exits_zero_and_removes_socket_when_chrome_closes_stdin() {
    let runtime = short_runtime();
    let mut child = host(runtime.path(), &[]);
    say_hello(&mut child);
    let path = socket_path(runtime.path());
    wait_for_socket(&path);

    drop(child.stdin.take());

    assert!(exit_status(&mut child).success());
    assert!(!path.exists());
}

#[test]
fn exits_non_zero_and_removes_socket_on_a_bad_frame() {
    let runtime = short_runtime();
    let mut child = host(runtime.path(), &[]);
    say_hello(&mut child);
    let path = socket_path(runtime.path());
    wait_for_socket(&path);

    let stdin = child.stdin.as_mut().expect("stdin is piped");
    stdin
        .write_all(&u32::MAX.to_ne_bytes())
        .expect("the header is sent");
    stdin.flush().expect("the header is flushed");

    assert!(!exit_status(&mut child).success());
    assert!(!path.exists());
}

#[test]
fn refuses_to_start_without_a_runtime_dir() {
    let output = Command::new(env!("CARGO_BIN_EXE_tab-keeper"))
        .arg("host")
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::null())
        .output()
        .expect("the binary runs");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "stdout belongs to Chrome");
}
