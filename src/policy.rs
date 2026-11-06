//! `TabDiscardingExceptions`: the hosts Chrome's policy says must never be unloaded.
//! tab-keeper only reads this. Setting it belongs to the machine's own configuration.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::tabs::url_host;

const POLICY_KEY: &str = "TabDiscardingExceptions";

/// Where Chrome and Chromium read machine-wide policy on Linux.
pub const DEFAULT_POLICY_DIRS: [&str; 2] = [
    "/etc/opt/chrome/policies/managed",
    "/etc/chromium/policies/managed",
];

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("cannot read the policy file {path}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the policy file {path} is not valid JSON")]
    Invalid {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{POLICY_KEY} in {0} is not a list of strings")]
    WrongShape(PathBuf),
    #[error(
        "no policy file sets {POLICY_KEY}, so tab-keeper cannot tell which tabs must stay \
         loaded. Set it, to an empty list if nothing is exempt"
    )]
    NotSet,
}

/// The exception list. Holding one proves the policy was read.
#[derive(Debug, PartialEq, Eq)]
pub struct Exceptions(Vec<Pattern>);

#[derive(Debug, PartialEq, Eq)]
enum Pattern {
    Everything,
    /// A host and its subdomains.
    Domain(String),
    /// A leading dot in Chrome's format: this host only.
    ExactHost(String),
}

impl Pattern {
    /// Chrome's URL pattern format, reduced to its host. A pattern's scheme, port and
    /// path are ignored, which can only protect more tabs than the policy names, never
    /// fewer: that is the right way to be wrong for a list of what must stay loaded.
    fn parse(text: &str) -> Self {
        let text = text.trim();
        let without_scheme = text.split_once("://").map_or(text, |(_, rest)| rest);
        let host_part = without_scheme.split('/').next().unwrap_or_default();
        let host_part = host_part.split(':').next().unwrap_or_default();
        let lowered = host_part.to_ascii_lowercase();
        if lowered.is_empty() || lowered == "*" {
            return Self::Everything;
        }
        if let Some(exact) = lowered.strip_prefix('.') {
            return Self::ExactHost(exact.to_owned());
        }
        let domain = lowered
            .strip_prefix("[*.]")
            .or_else(|| lowered.strip_prefix("*."))
            .unwrap_or(&lowered);
        Self::Domain(domain.to_owned())
    }

    fn covers(&self, host: Option<&str>) -> bool {
        match (self, host) {
            (Self::Everything, _) => true,
            (Self::ExactHost(exact), Some(host)) => host == exact,
            (Self::Domain(domain), Some(host)) => {
                host == domain
                    || host
                        .strip_suffix(domain.as_str())
                        .is_some_and(|prefix| prefix.ends_with('.'))
            }
            (_, None) => false,
        }
    }
}

impl Exceptions {
    pub fn covers(&self, url: &str) -> bool {
        let host = url_host(url);
        self.0.iter().any(|p| p.covers(host.as_deref()))
    }
}

/// Reads every policy file in these directories, the way Chrome merges them. Any file
/// that cannot be read stops this, as it may be the one holding the list.
pub fn read_exceptions(directories: &[PathBuf]) -> Result<Exceptions, PolicyError> {
    let mut patterns = Vec::new();
    let mut found = false;
    for directory in directories {
        for path in policy_files(directory)? {
            if let Some(listed) = read_file(&path)? {
                found = true;
                patterns.extend(listed.iter().map(|text| Pattern::parse(text)));
            }
        }
    }
    found
        .then_some(Exceptions(patterns))
        .ok_or(PolicyError::NotSet)
}

fn policy_files(directory: &Path) -> Result<Vec<PathBuf>, PolicyError> {
    let unreadable = |source| PolicyError::Unreadable {
        path: directory.to_path_buf(),
        source,
    };
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(unreadable(error)),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.map_err(unreadable)?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn read_file(path: &Path) -> Result<Option<Vec<String>>, PolicyError> {
    let text = fs::read_to_string(path).map_err(|source| PolicyError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;
    let policy: Value = serde_json::from_str(&text).map_err(|source| PolicyError::Invalid {
        path: path.to_path_buf(),
        source,
    })?;
    let Some(listed) = policy.get(POLICY_KEY) else {
        return Ok(None);
    };
    let wrong_shape = || PolicyError::WrongShape(path.to_path_buf());
    listed
        .as_array()
        .ok_or_else(wrong_shape)?
        .iter()
        .map(|entry| entry.as_str().map(str::to_owned).ok_or_else(wrong_shape))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exceptions(patterns: &[&str]) -> Exceptions {
        Exceptions(patterns.iter().map(|text| Pattern::parse(text)).collect())
    }

    fn policy_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("a temporary directory");
        for (name, text) in files {
            fs::write(directory.path().join(name), text).expect("the policy file");
        }
        directory
    }

    fn read(directory: &tempfile::TempDir) -> Result<Exceptions, PolicyError> {
        read_exceptions(&[directory.path().to_path_buf()])
    }

    #[test]
    fn a_host_and_its_subdomains_are_covered() {
        let list = exceptions(&["meet.google.com", "teams.live.com"]);

        assert!(list.covers("https://meet.google.com/abc"));
        assert!(list.covers("https://eu.teams.live.com/x"));
    }

    #[test]
    fn lookalikes_are_not_covered() {
        let list = exceptions(&["meet.google.com"]);

        assert!(!list.covers("https://notmeet.google.com/"));
        assert!(!list.covers("https://evil.com/meet.google.com"));
        assert!(!list.covers("https://meet.google.com.evil.com/"));
        assert!(!list.covers("https://meet.google.com@evil.com/"));
        assert!(!list.covers("about:blank"));
    }

    #[test]
    fn chromes_other_pattern_forms_are_understood() {
        for pattern in [
            "[*.]example.com",
            "*.example.com",
            "https://example.com",
            "https://example.com:443/some/path",
            "EXAMPLE.com",
            " example.com ",
        ] {
            let list = exceptions(&[pattern]);
            assert!(list.covers("https://example.com/"), "{pattern}");
            assert!(list.covers("https://a.example.com/"), "{pattern}");
        }
        let exact = exceptions(&[".example.com"]);
        assert!(exact.covers("https://example.com/"));
        assert!(!exact.covers("https://a.example.com/"));
    }

    #[test]
    fn a_star_covers_every_tab_even_one_with_no_host() {
        let list = exceptions(&["*"]);

        assert!(list.covers("https://a.example/"));
        assert!(list.covers("about:blank"));
    }

    #[test]
    fn the_list_is_read_and_merged_across_files() {
        let directory = policy_dir(&[
            ("a.json", r#"{"TabDiscardingExceptions":["a.example"]}"#),
            (
                "b.json",
                r#"{"TabDiscardingExceptions":["b.example"],"Other":1}"#,
            ),
            ("c.json", r#"{"Other":1}"#),
            ("ignored.txt", "not json"),
        ]);

        let list = read(&directory).expect("the policy");

        assert!(list.covers("https://a.example/"));
        assert!(list.covers("https://b.example/"));
    }

    #[test]
    fn an_empty_list_is_a_real_answer() {
        let directory = policy_dir(&[("a.json", r#"{"TabDiscardingExceptions":[]}"#)]);

        let list = read(&directory).expect("the policy");

        assert!(!list.covers("https://a.example/"));
    }

    #[test]
    fn a_missing_directory_or_key_refuses() {
        let missing = read_exceptions(&[PathBuf::from("/nonexistent/tab-keeper")]);
        assert!(matches!(missing, Err(PolicyError::NotSet)));

        let without_key = policy_dir(&[("a.json", r#"{"Other":1}"#)]);
        assert!(matches!(read(&without_key), Err(PolicyError::NotSet)));
    }

    #[test]
    fn a_broken_file_refuses_even_when_another_sets_the_list() {
        let directory = policy_dir(&[
            ("a.json", r#"{"TabDiscardingExceptions":["a.example"]}"#),
            ("b.json", "not json"),
        ]);

        assert!(matches!(read(&directory), Err(PolicyError::Invalid { .. })));
    }

    #[test]
    fn a_list_of_the_wrong_shape_refuses() {
        for text in [
            r#"{"TabDiscardingExceptions":"a.example"}"#,
            r#"{"TabDiscardingExceptions":[1]}"#,
        ] {
            let directory = policy_dir(&[("a.json", text)]);
            assert!(
                matches!(read(&directory), Err(PolicyError::WrongShape(_))),
                "{text}"
            );
        }
    }

    #[test]
    fn an_unreadable_file_refuses() {
        let directory = policy_dir(&[]);
        fs::create_dir(directory.path().join("a.json")).expect("a directory named like a file");

        assert!(matches!(
            read(&directory),
            Err(PolicyError::Unreadable { .. })
        ));
    }

    #[test]
    fn a_directory_that_cannot_be_read_refuses_even_when_another_sets_the_list() {
        use std::os::unix::fs::PermissionsExt;
        let readable = policy_dir(&[("a.json", r#"{"TabDiscardingExceptions": ["a.test"]}"#)]);
        let closed = policy_dir(&[]);
        fs::set_permissions(closed.path(), fs::Permissions::from_mode(0o000)).expect("closed");

        let result = read_exceptions(&[readable.path().to_path_buf(), closed.path().to_path_buf()]);

        fs::set_permissions(closed.path(), fs::Permissions::from_mode(0o700)).expect("opened");
        assert!(
            matches!(result, Err(PolicyError::Unreadable { .. })),
            "{result:?}"
        );
    }
}
