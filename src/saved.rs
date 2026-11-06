//! Saved records: what is written before anything is closed, so it can be undone.
//!
//! A record holds URLs and titles, which can be private, so the directory is 0700 and
//! each file 0600. Writing goes through a temporary file, a flush to disk and a rename,
//! so a record is either complete and durable or not there at all.

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const FORMAT_VERSION: u32 = 1;
const DIRECTORY: &str = "tab-keeper";
const PRIVATE_DIRECTORY: u32 = 0o700;
const PRIVATE_FILE: u32 = 0o600;

#[derive(Debug, thiserror::Error)]
pub enum SavedError {
    #[error("neither XDG_STATE_HOME nor HOME gives an absolute path to save to")]
    NoStateDir,
    #[error("{0} is not private: it must be a real directory with mode 0700")]
    UnsafeDirectory(PathBuf),
    #[error("writing or reading {path}")]
    File {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not a record this version understands")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("no saved record is called {0}")]
    NotFound(String),
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Closed,
    Group,
    Bookmark,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SavedTab {
    pub url: String,
    pub title: String,
    pub window: i64,
    pub index: i64,
    pub pinned: bool,
    pub group: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SavedGroup {
    pub key: String,
    pub title: String,
    pub color: String,
    pub collapsed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Record {
    pub version: u32,
    pub id: String,
    pub kind: RecordKind,
    pub name: Option<String>,
    pub created_ms: u64,
    pub restored_ms: Option<u64>,
    pub instance: String,
    pub profile: String,
    pub tabs: Vec<SavedTab>,
    pub groups: Vec<SavedGroup>,
    /// A removed bookmark or folder, exactly as Chrome described it.
    pub bookmark: Option<Value>,
}

impl Record {
    pub fn new(kind: RecordKind, instance: &str, profile: &str, created_ms: u64) -> Self {
        Self {
            version: FORMAT_VERSION,
            id: new_id(created_ms),
            kind,
            name: None,
            created_ms,
            restored_ms: None,
            instance: instance.to_owned(),
            profile: profile.to_owned(),
            tabs: Vec::new(),
            groups: Vec::new(),
            bookmark: None,
        }
    }
}

/// Sorts by time, and is unique among this process's records and other processes'.
fn new_id(created_ms: u64) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{created_ms}-{:x}-{sequence:x}", std::process::id())
}

/// An ID becomes a file name, and one typed by a person can hold anything.
fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// `$XDG_STATE_HOME`, or `$HOME/.local/state` as the specification says. A relative
/// value is ignored, also as it says.
pub fn state_home(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, SavedError> {
    let absolute = |value: Option<OsString>| value.map(PathBuf::from).filter(|p| p.is_absolute());
    absolute(xdg_state_home)
        .or_else(|| absolute(home).map(|home| home.join(".local/state")))
        .ok_or(SavedError::NoStateDir)
}

/// The directory that holds saved records.
#[derive(Clone, Debug)]
pub struct Store {
    directory: PathBuf,
}

impl Store {
    pub fn new(state_home: &Path) -> Self {
        Self {
            directory: state_home.join(DIRECTORY),
        }
    }

    fn file_error(path: &Path) -> impl FnOnce(io::Error) -> SavedError {
        let path = path.to_path_buf();
        move |source| SavedError::File { path, source }
    }

    fn ready_directory(&self) -> Result<&Path, SavedError> {
        let directory = &self.directory;
        match DirBuilder::new()
            .recursive(true)
            .mode(PRIVATE_DIRECTORY)
            .create(directory)
        {
            Err(error) if error.kind() != ErrorKind::AlreadyExists => {
                return Err(Self::file_error(directory)(error));
            }
            _ => {}
        }
        let metadata = fs::symlink_metadata(directory).map_err(Self::file_error(directory))?;
        let private =
            metadata.is_dir() && metadata.permissions().mode() & 0o7777 == PRIVATE_DIRECTORY;
        private
            .then_some(directory.as_path())
            .ok_or_else(|| SavedError::UnsafeDirectory(directory.clone()))
    }

    fn path_of(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }

    /// Returns only once the record is on disk and will survive a crash.
    pub fn write(&self, record: &Record) -> Result<(), SavedError> {
        let directory = self.ready_directory()?;
        let path = self.path_of(&record.id);
        let temporary = directory.join(format!(".{}.tmp", record.id));
        let outcome = write_durably(&temporary, record)
            .and_then(|()| fs::rename(&temporary, &path))
            .and_then(|()| File::open(directory)?.sync_all());
        if outcome.is_err() && temporary.exists() {
            // Best effort: the failed write is what gets reported, and a leftover dotfile
            // is ignored by `list`.
            fs::remove_file(&temporary).ok();
        }
        outcome.map_err(Self::file_error(&path))
    }

    pub fn load(&self, id: &str) -> Result<Record, SavedError> {
        if !is_valid_id(id) {
            return Err(SavedError::NotFound(id.to_owned()));
        }
        let path = self.path_of(id);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(SavedError::NotFound(id.to_owned()));
            }
            Err(error) => return Err(Self::file_error(&path)(error)),
        };
        serde_json::from_str(&text).map_err(|source| SavedError::Unreadable { path, source })
    }

    /// Every record, newest first. One that cannot be read fails the list rather than
    /// vanishing from it, since that would look as if it had never been saved.
    pub fn list(&self) -> Result<Vec<Record>, SavedError> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Self::file_error(&self.directory)(error)),
        };
        let mut records = Vec::new();
        for entry in entries {
            let path = entry.map_err(Self::file_error(&self.directory))?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if let Some(id) = name.strip_suffix(".json")
                && !name.starts_with('.')
            {
                records.push(self.load(id)?);
            }
        }
        records.sort_by(|a, b| (b.created_ms, &b.id).cmp(&(a.created_ms, &a.id)));
        Ok(records)
    }

    pub fn delete(&self, id: &str) -> Result<(), SavedError> {
        if !is_valid_id(id) {
            return Err(SavedError::NotFound(id.to_owned()));
        }
        let path = self.path_of(id);
        match fs::remove_file(&path) {
            Err(error) if error.kind() == ErrorKind::NotFound => {
                Err(SavedError::NotFound(id.to_owned()))
            }
            other => other.map_err(Self::file_error(&path)),
        }
    }
}

fn write_durably(path: &Path, record: &Record) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(PRIVATE_FILE)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(record)?)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let state = tempfile::tempdir().expect("a temporary directory");
        let store = Store::new(state.path());
        (state, store)
    }

    fn record(created_ms: u64) -> Record {
        let mut record = Record::new(RecordKind::Closed, "instance", "Default", created_ms);
        record.tabs.push(SavedTab {
            url: "https://a/".to_owned(),
            title: "A".to_owned(),
            window: 1,
            index: 2,
            pinned: true,
            group: Some("g".to_owned()),
        });
        record
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path)
            .expect("the path")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn a_record_round_trips_with_everything_a_restore_needs() {
        let (_state, store) = store();
        let written = record(5);

        store.write(&written).expect("written");

        assert_eq!(store.load(&written.id).expect("loaded"), written);
    }

    #[test]
    fn records_are_readable_only_by_this_user() {
        let (state, store) = store();
        let written = record(5);

        store.write(&written).expect("written");

        let directory = state.path().join(DIRECTORY);
        assert_eq!(mode(&directory), 0o700);
        assert_eq!(mode(&directory.join(format!("{}.json", written.id))), 0o600);
    }

    #[test]
    fn writing_to_an_unwritable_directory_fails_loudly() {
        let (state, store) = store();
        let directory = state.path().join(DIRECTORY);
        fs::create_dir(&directory).expect("the directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).expect("the mode");

        let outcome = store.write(&record(5));

        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).expect("the mode");
        assert!(matches!(outcome, Err(SavedError::UnsafeDirectory(_))));
    }

    #[test]
    fn a_failed_write_leaves_nothing_behind() {
        let (state, store) = store();
        let directory = state.path().join(DIRECTORY);
        fs::create_dir(&directory).expect("the directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).expect("the mode");
        let written = record(5);
        // Something in the way of the rename: a directory where the record belongs.
        fs::create_dir(directory.join(format!("{}.json", written.id))).expect("the obstacle");

        let outcome = store.write(&written);

        assert!(matches!(outcome, Err(SavedError::File { .. })));
        let names: Vec<_> = fs::read_dir(&directory)
            .expect("the directory")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        assert_eq!(names.len(), 1, "only the obstacle: {names:?}");
    }

    #[test]
    fn a_loose_or_symlinked_directory_is_refused() {
        let (state, store) = store();
        let directory = state.path().join(DIRECTORY);
        fs::create_dir(&directory).expect("the directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).expect("the mode");
        assert!(matches!(
            store.write(&record(5)),
            Err(SavedError::UnsafeDirectory(_))
        ));

        fs::remove_dir(&directory).expect("removed");
        let elsewhere = state.path().join("elsewhere");
        fs::create_dir(&elsewhere).expect("the target");
        std::os::unix::fs::symlink(&elsewhere, &directory).expect("the link");
        assert!(matches!(
            store.write(&record(5)),
            Err(SavedError::UnsafeDirectory(_))
        ));
    }

    #[test]
    fn rewriting_a_record_replaces_it() {
        let (_state, store) = store();
        let mut written = record(5);
        store.write(&written).expect("written");

        written.tabs.clear();
        store.write(&written).expect("rewritten");

        assert_eq!(store.load(&written.id).expect("loaded").tabs, []);
    }

    #[test]
    fn records_list_newest_first_and_ids_never_collide() {
        let (_state, store) = store();
        let (old, new_a, new_b) = (record(1), record(9), record(9));
        assert_ne!(new_a.id, new_b.id);
        for record in [&old, &new_a, &new_b] {
            store.write(record).expect("written");
        }

        let listed = store.list().expect("listed");

        assert_eq!(listed.len(), 3);
        assert_eq!(listed[2], old);
    }

    #[test]
    fn nothing_saved_yet_is_an_empty_list() {
        let (_state, store) = store();

        assert_eq!(store.list().expect("listed"), []);
    }

    #[test]
    fn a_damaged_record_fails_the_list_instead_of_vanishing() {
        let (state, store) = store();
        store.write(&record(5)).expect("written");
        fs::write(state.path().join(DIRECTORY).join("9-dead.json"), "{").expect("damage");

        assert!(matches!(store.list(), Err(SavedError::Unreadable { .. })));
    }

    #[test]
    fn an_id_cannot_reach_outside_the_directory() {
        let (state, store) = store();
        store.write(&record(5)).expect("written");
        let outside = state.path().join("outside.json");
        fs::write(&outside, "{}").expect("a file");

        for id in ["../outside", "..", "a/b", ""] {
            assert!(
                matches!(store.load(id), Err(SavedError::NotFound(_))),
                "{id}"
            );
            assert!(
                matches!(store.delete(id), Err(SavedError::NotFound(_))),
                "{id}"
            );
        }
        assert!(outside.exists());
    }

    #[test]
    fn deleting_removes_the_record_and_a_missing_one_says_so() {
        let (_state, store) = store();
        let written = record(5);
        store.write(&written).expect("written");

        store.delete(&written.id).expect("deleted");

        assert!(matches!(
            store.load(&written.id),
            Err(SavedError::NotFound(_))
        ));
        assert!(matches!(
            store.delete(&written.id),
            Err(SavedError::NotFound(_))
        ));
    }

    #[test]
    fn the_state_home_follows_the_specification() {
        let path = |text: &str| Some(OsString::from(text));

        assert_eq!(state_home(path("/x"), path("/h")).unwrap(), Path::new("/x"));
        assert_eq!(
            state_home(path("relative"), path("/h")).unwrap(),
            Path::new("/h/.local/state")
        );
        assert_eq!(
            state_home(None, path("/h")).unwrap(),
            Path::new("/h/.local/state")
        );
        assert!(matches!(
            state_home(None, path("rel")),
            Err(SavedError::NoStateDir)
        ));
        assert!(matches!(
            state_home(None, None),
            Err(SavedError::NoStateDir)
        ));
    }
}
