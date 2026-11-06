//! Where a host's socket lives, and how it takes over from an earlier run.
//!
//! `XDG_RUNTIME_DIR` is the user's own by specification, and a mode 0700 directory
//! can only be entered by its owner, so a successful bind in one means it is ours.

use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use super::{HostError, InstanceId};

const SOCKET_DIRECTORY: &str = "tab-keeper";
const PRIVATE_MODE: u32 = 0o700;

/// A bound socket's path. Dropping it removes the socket, unless a newer host has
/// replaced it.
#[derive(Debug)]
pub struct BoundSocket {
    path: PathBuf,
    identity: Identity,
}

/// Which file a path holds: a replacement has the same path and another identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl From<&fs::Metadata> for Identity {
    fn from(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

/// Binds this instance's socket, replacing one left by another run in a single
/// step: a client finds the old socket or the new one, never a gap.
pub fn bind(
    runtime_dir: &Path,
    instance: &InstanceId,
) -> Result<(BoundSocket, UnixListener), HostError> {
    let directory = private_directory(runtime_dir)?;
    let path = directory.join(format!("{}.sock", instance.as_str()));
    refuse_anything_but_a_socket(&path)?;

    // A dot and no `.sock`, so a client looking for sockets never finds this one.
    let temporary = directory.join(format!(".{}.{}.tmp", instance.as_str(), std::process::id()));
    let socket_error = |path: &Path| {
        let path = path.to_path_buf();
        move |source| HostError::Socket { path, source }
    };
    match fs::remove_file(&temporary) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            return Err(socket_error(&temporary)(error));
        }
        _ => {}
    }
    let listener = UnixListener::bind(&temporary).map_err(socket_error(&temporary))?;
    let identity = fs::symlink_metadata(&temporary)
        .map(|metadata| Identity::from(&metadata))
        .and_then(|identity| fs::rename(&temporary, &path).map(|()| identity));
    match identity {
        Ok(identity) => Ok((BoundSocket { path, identity }, listener)),
        Err(error) => {
            remove_logging_failure(&temporary);
            Err(socket_error(&path)(error))
        }
    }
}

fn private_directory(runtime_dir: &Path) -> Result<PathBuf, HostError> {
    let directory = runtime_dir.join(SOCKET_DIRECTORY);
    let socket_error = |source| HostError::Socket {
        path: directory.clone(),
        source,
    };
    match DirBuilder::new().mode(PRIVATE_MODE).create(&directory) {
        Err(error) if error.kind() != ErrorKind::AlreadyExists => return Err(socket_error(error)),
        _ => {}
    }
    let metadata = fs::symlink_metadata(&directory).map_err(socket_error)?;
    let private = metadata.is_dir() && metadata.permissions().mode() & 0o7777 == PRIVATE_MODE;
    if private {
        Ok(directory)
    } else {
        Err(HostError::UnsafeDirectory(directory))
    }
}

fn refuse_anything_but_a_socket(path: &Path) -> Result<(), HostError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => Ok(()),
        Ok(_) => Err(HostError::PathOccupied(path.to_path_buf())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(HostError::Socket {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn remove_logging_failure(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        eprintln!("tab-keeper: removing {}: {error}", path.display());
    }
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        // A newer host for this profile may hold the path by now. Only hosts of
        // this user can act in the directory, so the gap between the two calls
        // matters only if two hosts of one profile start and stop at that moment.
        let still_ours = fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| Identity::from(&metadata) == self.identity);
        if still_ours {
            remove_logging_failure(&self.path);
        }
    }
}
