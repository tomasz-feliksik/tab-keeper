//! A stand-in for a profile: a socket that answers as the extension would, recording every
//! request, driven through the real binary.
#![allow(dead_code)] // Each test binary uses part of this.

use std::fs::{self, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

pub const INSTANCE_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const INSTANCE_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub type Handler = Box<dyn Fn(&Value) -> Value + Send + Sync>;

pub struct World {
    pub root: tempfile::TempDir,
}

impl World {
    pub fn new() -> Self {
        // A socket path has to fit 107 bytes, so this is short on purpose.
        let root = tempfile::Builder::new()
            .prefix("tk")
            .tempdir_in("/tmp")
            .expect("a temporary directory");
        let world = Self { root };
        fs::create_dir_all(world.socket_dir()).expect("socket directory");
        fs::set_permissions(world.socket_dir(), Permissions::from_mode(0o700)).expect("private");
        fs::create_dir_all(world.policy_dir()).expect("policy directory");
        world.set_policy(&json!({"TabDiscardingExceptions": []}));
        world
    }

    pub fn socket_dir(&self) -> PathBuf {
        self.root.path().join("tab-keeper")
    }

    pub fn state_home(&self) -> PathBuf {
        self.root.path().join("state")
    }

    pub fn saved_dir(&self) -> PathBuf {
        self.state_home().join("tab-keeper")
    }

    pub fn policy_dir(&self) -> PathBuf {
        self.root.path().join("policy")
    }

    pub fn set_policy(&self, policy: &Value) {
        fs::write(self.policy_dir().join("policy.json"), policy.to_string()).expect("policy");
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tab-keeper"));
        command
            .env("XDG_RUNTIME_DIR", self.root.path())
            .env("XDG_STATE_HOME", self.state_home())
            .env(
                "TAB_KEEPER_BROWSER_DIRS",
                self.root.path().join("no-browser"),
            );
        command
    }

    pub fn run(&self, args: &[&str]) -> Output {
        let mut full: Vec<&str> = vec!["--json", "--timeout", "5"];
        full.extend_from_slice(args);
        self.command()
            .args(&full)
            .output()
            .expect("the binary runs")
    }

    /// Starts a stand-in. It answers every request with what `handler` returns as `result`.
    pub fn profile(&self, instance: &str, handler: Handler) -> FakeProfile {
        let path = self.socket_dir().join(format!("{instance}.sock"));
        let listener = UnixListener::bind(&path).expect("bind");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let Ok(request) = read_message(&mut stream) else {
                    continue;
                };
                seen.lock().expect("not poisoned").push(request.clone());
                let answer = handler(&request);
                if answer == Value::Null {
                    continue; // Hang up without answering.
                }
                write_message(&mut stream, &answer).ok();
            }
        });
        FakeProfile { requests }
    }
}

pub struct FakeProfile {
    requests: Arc<Mutex<Vec<Value>>>,
}

impl FakeProfile {
    pub fn ops(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("not poisoned")
            .iter()
            .map(|request| request["op"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    pub fn requests_for(&self, op: &str) -> Vec<Value> {
        self.requests
            .lock()
            .expect("not poisoned")
            .iter()
            .filter(|request| request["op"] == op)
            .cloned()
            .collect()
    }
}

fn read_message(stream: &mut impl Read) -> io::Result<Value> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let mut bytes = vec![0; u32::from_ne_bytes(length) as usize];
    stream.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn write_message(stream: &mut impl Write, value: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    stream.write_all(&length.to_ne_bytes())?;
    stream.write_all(&bytes)
}

pub fn tab(id: i64, url: &str) -> Value {
    json!({
        "id": id,
        "windowId": 1,
        "index": id,
        "title": format!("title {id}"),
        "url": url,
        "active": false,
        "pinned": false,
        "audible": false,
        "discarded": false,
        "groupId": -1,
        // Long ago, so everything is idle unless a test says otherwise.
        "lastAccessed": 1000.0,
    })
}

pub fn snapshot(tabs: &[Value]) -> Value {
    json!({"extensionId": "niglgkgdkfompjmcbpldgepicfnjlfnc", "tabs": tabs, "groups": []})
}

/// Answers like the extension: `list` from `tabs`, and every target `done`.
pub fn obliging(tabs: Vec<Value>) -> Handler {
    Box::new(move |request| answer_obligingly(request, &tabs))
}

pub fn answer_obligingly(request: &Value, tabs: &[Value]) -> Value {
    let result = if request["op"] == "list" {
        snapshot(tabs)
    } else {
        let results: Vec<Value> = request["targets"]
            .as_array()
            .map(|targets| {
                targets
                    .iter()
                    .map(|target| json!({"id": target["id"], "status": "done"}))
                    .collect()
            })
            .unwrap_or_default();
        json!({"results": results})
    };
    json!({"id": request["id"], "ok": true, "result": result})
}

pub fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("exited, not killed")
}

pub fn answer_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON ({error}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

pub fn make_read_only(path: &Path) {
    fs::set_permissions(path, Permissions::from_mode(0o500)).expect("read-only");
}
