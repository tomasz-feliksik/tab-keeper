//! Turning an extension's random instance ID into a profile a person recognises.
//!
//! An extension cannot see its profile's name, and an account does not identify a
//! profile, since profiles can share one or have none. But `chrome.storage.local` writes
//! the instance ID under that profile's directory, so searching for it needs no extra
//! permission, and the directory then names the profile.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

const SETTINGS_DIRECTORY: &str = "Local Extension Settings";
const SHORT_INSTANCE_LEN: usize = 8;
const EXTENSION_ID_LEN: usize = 32;

/// A profile directory that holds this extension's settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownProfile {
    pub directory: String,
    pub label: Option<String>,
    pub instances: HashSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileName {
    /// Unique among connected profiles, and what tab references use.
    pub name: String,
    /// The name the person gave the profile in Chrome, when it can be read.
    pub label: Option<String>,
    pub instance: String,
}

impl ProfileName {
    pub fn answers_to(&self, wanted: &str) -> bool {
        self.name == wanted
            || self.label.as_deref() == Some(wanted)
            || (wanted.len() >= SHORT_INSTANCE_LEN && self.instance.starts_with(wanted))
    }
}

/// An extension ID is 32 letters from a to p, and it arrives from outside this program,
/// so this also keeps it from naming any other path.
fn is_extension_id(id: &str) -> bool {
    id.len() == EXTENSION_ID_LEN && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))
}

/// Every profile under these browser directories that has this extension's settings.
pub fn known_profiles(browser_dirs: &[PathBuf], extension_id: &str) -> Vec<KnownProfile> {
    if !is_extension_id(extension_id) {
        return Vec::new();
    }
    browser_dirs
        .iter()
        .flat_map(|browser| profiles_in(browser, extension_id))
        .collect()
}

fn profiles_in(browser: &Path, extension_id: &str) -> Vec<KnownProfile> {
    let labels = profile_labels(browser);
    let Ok(entries) = fs::read_dir(browser) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let directory = entry.file_name().into_string().ok()?;
            let settings = entry.path().join(SETTINGS_DIRECTORY).join(extension_id);
            settings.is_dir().then(|| KnownProfile {
                label: labels.get(&directory).cloned(),
                instances: instances_in(&settings),
                directory,
            })
        })
        .collect()
}

/// The names the person gave their profiles, from Chrome's `Local State`. A missing or
/// changed file only costs the labels, as the directory still names each profile.
fn profile_labels(browser: &Path) -> HashMap<String, String> {
    let Ok(text) = fs::read_to_string(browser.join("Local State")) else {
        return HashMap::new();
    };
    let Ok(state) = serde_json::from_str::<Value>(&text) else {
        return HashMap::new();
    };
    state["profile"]["info_cache"]
        .as_object()
        .map(|profiles| {
            profiles
                .iter()
                .filter_map(|(directory, info)| {
                    Some((directory.clone(), info["name"].as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Instance IDs stored in a `LevelDB` directory, found by looking for 32 hex digits. A
/// file that vanishes as `LevelDB` rotates it holds nothing that a newer one does not.
fn instances_in(settings: &Path) -> HashSet<String> {
    let Ok(entries) = fs::read_dir(settings) else {
        return HashSet::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read(entry.path()).ok())
        .flat_map(|bytes| hex_runs(&bytes))
        .collect()
}

fn hex_runs(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        .filter(|run| run.len() == crate::host::INSTANCE_LEN)
        .filter_map(|run| std::str::from_utf8(run).ok().map(str::to_owned))
        .collect()
}

/// Names for connected instances, in order. A profile that cannot be found goes by the
/// start of its instance ID, and two browsers' profiles with one name are told apart.
pub fn name_instances(instances: &[String], known: &[KnownProfile]) -> Vec<ProfileName> {
    let mut named: Vec<ProfileName> = instances
        .iter()
        .map(|instance| {
            let profile = known.iter().find(|p| p.instances.contains(instance));
            ProfileName {
                name: profile.map_or_else(
                    || instance[..SHORT_INSTANCE_LEN.min(instance.len())].to_owned(),
                    |p| p.directory.clone(),
                ),
                label: profile.and_then(|p| p.label.clone()),
                instance: instance.clone(),
            }
        })
        .collect();
    let mut uses: HashMap<String, usize> = HashMap::new();
    for profile in &named {
        *uses.entry(profile.name.clone()).or_default() += 1;
    }
    for profile in &mut named {
        if uses[&profile.name] > 1 {
            let short = &profile.instance[..SHORT_INSTANCE_LEN.min(profile.instance.len())];
            profile.name = format!("{}~{short}", profile.name);
        }
    }
    named
}

/// Profiles that have the extension and are not connected. These sleep when they have
/// no open window, and should be named rather than left out.
pub fn not_connected(known: &[KnownProfile], connected: &[String]) -> Vec<String> {
    let mut sleeping: Vec<String> = known
        .iter()
        .filter(|profile| !connected.iter().any(|i| profile.instances.contains(i)))
        .map(|profile| {
            profile
                .label
                .clone()
                .unwrap_or_else(|| profile.directory.clone())
        })
        .collect();
    sleeping.sort();
    sleeping.dedup();
    sleeping
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXTENSION: &str = "abcdefghijklmnopabcdefghijklmnop";
    const INSTANCE_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const INSTANCE_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn browser_with(profiles: &[(&str, &str)]) -> tempfile::TempDir {
        let browser = tempfile::tempdir().expect("a temporary directory");
        for (directory, instance) in profiles {
            let settings = browser
                .path()
                .join(directory)
                .join(SETTINGS_DIRECTORY)
                .join(EXTENSION);
            fs::create_dir_all(&settings).expect("the settings directory");
            fs::write(
                settings.join("000003.log"),
                format!("\u{0}instance\"{instance}\"\u{0}"),
            )
            .expect("the settings file");
        }
        browser
    }

    fn known(browser: &tempfile::TempDir) -> Vec<KnownProfile> {
        known_profiles(&[browser.path().to_path_buf()], EXTENSION)
    }

    #[test]
    fn two_profiles_are_told_apart_by_their_stored_instance() {
        let browser = browser_with(&[("Default", INSTANCE_A), ("Profile 1", INSTANCE_B)]);

        let named = name_instances(
            &[INSTANCE_B.to_owned(), INSTANCE_A.to_owned()],
            &known(&browser),
        );

        assert_eq!(named[0].name, "Profile 1");
        assert_eq!(named[1].name, "Default");
    }

    #[test]
    fn the_name_chosen_in_chrome_becomes_the_label() {
        let browser = browser_with(&[("Profile 1", INSTANCE_A)]);
        fs::write(
            browser.path().join("Local State"),
            r#"{"profile":{"info_cache":{"Profile 1":{"name":"Chosen"}}}}"#,
        )
        .expect("the file");

        let named = name_instances(&[INSTANCE_A.to_owned()], &known(&browser));

        assert_eq!(named[0].label.as_deref(), Some("Chosen"));
        assert!(named[0].answers_to("Chosen"));
        assert!(named[0].answers_to("Profile 1"));
        assert!(named[0].answers_to(&INSTANCE_A[..8]));
        assert!(
            !named[0].answers_to(&INSTANCE_A[..7]),
            "too short to be sure"
        );
        assert!(!named[0].answers_to("Other"));
    }

    #[test]
    fn an_unreadable_local_state_only_costs_the_label() {
        let browser = browser_with(&[("Default", INSTANCE_A)]);
        fs::write(browser.path().join("Local State"), "not json").expect("the file");

        let named = name_instances(&[INSTANCE_A.to_owned()], &known(&browser));

        assert_eq!(named[0].name, "Default");
        assert_eq!(named[0].label, None);
    }

    #[test]
    fn an_instance_nobody_holds_goes_by_its_start() {
        let named = name_instances(&[INSTANCE_A.to_owned()], &[]);

        assert_eq!(named[0].name, INSTANCE_A[..8]);
    }

    #[test]
    fn the_same_directory_in_two_browsers_gets_two_names() {
        let first = browser_with(&[("Default", INSTANCE_A)]);
        let second = browser_with(&[("Default", INSTANCE_B)]);
        let known = known_profiles(
            &[first.path().to_path_buf(), second.path().to_path_buf()],
            EXTENSION,
        );

        let named = name_instances(&[INSTANCE_A.to_owned(), INSTANCE_B.to_owned()], &known);

        assert_ne!(named[0].name, named[1].name);
        assert!(named[0].name.starts_with("Default~"));
    }

    #[test]
    fn a_profile_without_a_host_is_named_as_not_connected() {
        let browser = browser_with(&[("Default", INSTANCE_A), ("Profile 1", INSTANCE_B)]);

        let sleeping = not_connected(&known(&browser), &[INSTANCE_A.to_owned()]);

        assert_eq!(sleeping, ["Profile 1"]);
    }

    #[test]
    fn an_extension_id_cannot_name_another_path() {
        let browser = browser_with(&[("Default", INSTANCE_A)]);

        let found = known_profiles(&[browser.path().to_path_buf()], "../../../../etc");

        assert_eq!(found, []);
    }

    #[test]
    fn other_extensions_settings_are_not_searched() {
        let browser = browser_with(&[("Default", INSTANCE_A)]);

        let found = known_profiles(
            &[browser.path().to_path_buf()],
            "ponmlkjihgfedcbaponmlkjihgfedcba",
        );

        assert_eq!(found, []);
    }
}
