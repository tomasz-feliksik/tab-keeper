//! What tab-keeper can do, independent of how it was asked. The CLI and the MCP server
//! are both thin layers over this.

mod arranging;
mod bookmarks;
mod closing;
mod discarding;
mod gather;
mod listing;
mod results;
mod saved_groups;

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub use arranging::{GROUP_COLORS, GroupOptions, MoveOptions, group, move_tabs, ungroup};
pub use bookmarks::{
    BookmarkAdd, BookmarkListOptions, add_bookmarks, list_bookmarks, remove_bookmark,
    restore_bookmark,
};
pub use closing::{close, list_saved, restore};
pub use discarding::{DiscardOptions, discard, parse_duration_ms};
pub use listing::{ListOptions, list, list_groups, list_profiles};
pub use saved_groups::{delete_group, list_groups_saved, open_group, save_group};

use crate::client::AskError;
use crate::host::HostError;
use crate::policy::PolicyError;
use crate::saved::{SavedError, Store};

/// Where everything is and how long to wait. Nothing reads the environment below this.
#[derive(Clone, Debug)]
pub struct Env {
    pub runtime_dir: PathBuf,
    pub state_home: PathBuf,
    pub browser_dirs: Vec<PathBuf>,
    pub timeout: Duration,
    pub now_ms: u64,
}

impl Env {
    fn store(&self) -> Store {
        Store::new(&self.state_home)
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The answer to any operation: data for a program, text for a person, and whether
/// everything asked for really happened. A partial result is never reported as success.
#[derive(Debug)]
pub struct Report {
    pub value: Value,
    pub human: String,
    pub complete: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error(transparent)]
    Host(#[from] HostError),
    #[error(transparent)]
    Saved(#[from] SavedError),
    #[error(transparent)]
    Policy(#[from] PolicyError),
    #[error("no profile is connected: is Chrome running with the tab-keeper extension?")]
    NoProfileConnected,
    #[error("no connected profile is called {wanted:?}; connected: {known}")]
    UnknownProfile { wanted: String, known: String },
    #[error("several profiles are connected, so say which: {known}")]
    AmbiguousProfile { known: String },
    #[error("{0:?} is not a tab reference; `tab-keeper list` prints them")]
    BadReference(String),
    #[error("{0}")]
    Usage(String),
    #[error("record {0} has already been restored; pass --again to open it a second time")]
    AlreadyRestored(String),
    #[error(
        "record {id} belongs to profile {profile:?}, which is not connected, so nothing was opened"
    )]
    ProfileMissing { id: String, profile: String },
    #[error("profile {profile:?} did not answer: {source}")]
    Asking {
        profile: String,
        #[source]
        source: AskError,
    },
    #[error("profile {0:?} gave an answer that was not understood")]
    BadAnswer(String),
}
