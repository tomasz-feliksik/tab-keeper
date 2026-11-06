//! Finding out what every connected profile has open, right now.

use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};

use super::{Env, OpError};
use crate::client::{self, AskError};
use crate::profiles::{self, ProfileName};
use crate::tabs::{Snapshot, Tab, TabRef};

#[derive(Debug)]
pub struct LiveProfile {
    pub name: ProfileName,
    pub socket: PathBuf,
    pub snapshot: Snapshot,
}

impl LiveProfile {
    pub fn tab(&self, id: i64) -> Option<&Tab> {
        self.snapshot.tabs.iter().find(|tab| tab.id == id)
    }

    /// One request to this profile. Its timeout is its own.
    pub fn ask(&self, env: &Env, request: &Value) -> Result<Value, OpError> {
        client::ask(&self.socket, request, env.timeout).map_err(|source| OpError::Asking {
            profile: self.name.name.clone(),
            source,
        })
    }
}

/// A profile whose host exists and which gave no usable answer.
#[derive(Debug, Serialize)]
pub struct FailedProfile {
    pub profile: String,
    pub instance: String,
    pub error: String,
}

#[derive(Debug)]
pub struct Gathered {
    pub live: Vec<LiveProfile>,
    pub failed: Vec<FailedProfile>,
    pub not_connected: Vec<String>,
}

/// Asks every profile. No sockets at all is an error, because an empty answer would read
/// as a browser with no tabs. A profile that fails is kept, named, in `failed`.
pub fn gather(env: &Env) -> Result<Gathered, OpError> {
    let sockets = client::sockets(&env.runtime_dir)?;
    if sockets.is_empty() {
        return Err(OpError::NoProfileConnected);
    }
    let answers = client::ask_all(&sockets, &json!({"op": "list"}), env.timeout);
    let parsed: Vec<Result<Snapshot, String>> = answers
        .into_iter()
        .map(|answer| {
            answer
                .and_then(|value| serde_json::from_value(value).map_err(AskError::BadAnswer))
                .map_err(|error| error.to_string())
        })
        .collect();

    let extension_id = parsed
        .iter()
        .find_map(|answer| answer.as_ref().ok())
        .map(|snapshot| snapshot.extension_id.clone());
    let known = extension_id
        .as_deref()
        .map(|id| profiles::known_profiles(&env.browser_dirs, id))
        .unwrap_or_default();
    let instances: Vec<String> = sockets.iter().map(|s| s.instance.clone()).collect();
    let names = profiles::name_instances(&instances, &known);
    let not_connected = profiles::not_connected(&known, &instances);

    let mut live = Vec::new();
    let mut failed = Vec::new();
    for ((answer, socket), name) in parsed.into_iter().zip(sockets).zip(names) {
        match answer {
            Ok(snapshot) => live.push(LiveProfile {
                name,
                socket: socket.path,
                snapshot,
            }),
            Err(error) => failed.push(FailedProfile {
                profile: name.name,
                instance: name.instance,
                error,
            }),
        }
    }
    Ok(Gathered {
        live,
        failed,
        not_connected,
    })
}

impl Gathered {
    pub fn known_names(&self) -> String {
        let mut names: Vec<&str> = self.live.iter().map(|p| p.name.name.as_str()).collect();
        names.sort_unstable();
        names.join(", ")
    }

    /// The connected profiles a `--profile` filter leaves, which must be at least one.
    pub fn select(&self, wanted: Option<&str>) -> Result<Vec<&LiveProfile>, OpError> {
        let Some(wanted) = wanted else {
            return Ok(self.live.iter().collect());
        };
        let selected: Vec<&LiveProfile> = self
            .live
            .iter()
            .filter(|p| p.name.answers_to(wanted))
            .collect();
        if selected.is_empty() {
            return Err(OpError::UnknownProfile {
                wanted: wanted.to_owned(),
                known: self.known_names(),
            });
        }
        Ok(selected)
    }

    /// The single profile an operation applies to, named or the only one connected.
    pub fn one(&self, wanted: Option<&str>) -> Result<&LiveProfile, OpError> {
        let selected = self.select(wanted)?;
        match selected.as_slice() {
            [only] => Ok(only),
            [] => Err(OpError::NoProfileConnected),
            _ => Err(OpError::AmbiguousProfile {
                known: self.known_names(),
            }),
        }
    }
}

/// Parses references, reporting the first that is not one.
pub fn parse_refs(texts: &[String]) -> Result<Vec<TabRef>, OpError> {
    let mut refs: Vec<TabRef> = Vec::with_capacity(texts.len());
    for text in texts {
        let parsed = TabRef::parse(text).ok_or_else(|| OpError::BadReference(text.clone()))?;
        if !refs.contains(&parsed) {
            refs.push(parsed);
        }
    }
    Ok(refs)
}
