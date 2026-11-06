//! What a profile reports about its tabs, and how a caller names one tab.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Chrome's `tabGroups.TAB_GROUP_ID_NONE`.
pub const NO_GROUP: i64 = -1;

#[allow(clippy::struct_excessive_bools)] // Each one mirrors a field Chrome reports.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tab {
    pub id: i64,
    pub window_id: i64,
    pub index: i64,
    pub title: String,
    pub url: String,
    pub active: bool,
    pub pinned: bool,
    pub audible: bool,
    pub discarded: bool,
    pub group_id: i64,
    pub last_accessed: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: i64,
    pub title: String,
    pub color: String,
    pub collapsed: bool,
    pub window_id: i64,
}

/// A profile's answer to `list`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub extension_id: String,
    pub tabs: Vec<Tab>,
    pub groups: Vec<Group>,
}

/// Names one tab: the profile, Chrome's ID, which means nothing outside that profile
/// and session, and a hash of the URL the caller saw. A tab whose URL has changed no
/// longer matches, which is how a plan made from a stale list fails safely. It keeps
/// references short, since a URL can be megabytes, and the full URL is still compared
/// inside the extension before anything changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabRef {
    pub profile: String,
    pub id: i64,
    pub url_hash: String,
}

const URL_HASH_LEN: usize = 8;

impl TabRef {
    pub fn of(profile: &str, tab: &Tab) -> Self {
        Self {
            profile: profile.to_owned(),
            id: tab.id,
            url_hash: url_hash(&tab.url),
        }
    }

    /// `profile:id:hash`. A profile directory may hold a colon, so this reads from the
    /// right, where the two other parts cannot.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.rsplitn(3, ':');
        let url_hash = parts.next()?;
        let id = parts.next()?.parse().ok()?;
        let profile = parts.next()?;
        let valid_hash =
            url_hash.len() == URL_HASH_LEN && url_hash.bytes().all(|b| b.is_ascii_hexdigit());
        (valid_hash && !profile.is_empty()).then(|| Self {
            profile: profile.to_owned(),
            id,
            url_hash: url_hash.to_ascii_lowercase(),
        })
    }

    pub fn matches_url(&self, url: &str) -> bool {
        self.url_hash == url_hash(url)
    }
}

impl fmt::Display for TabRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}:{}", self.profile, self.id, self.url_hash)
    }
}

/// FNV-1a, folded to 32 bits. This detects a changed URL rather than resisting an
/// attacker: the caller already holds the same power the check protects against.
fn url_hash(url: &str) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = url.bytes().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    });
    format!("{:08x}", (hash >> 32) ^ (hash & 0xffff_ffff))
}

/// The host of a URL, lowercased, without its user information or port.
pub fn url_host(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_and_port = authority.rsplit('@').next()?;
    let host = if let Some(bracketed) = host_and_port.strip_prefix('[') {
        bracketed.split(']').next()?
    } else {
        host_and_port.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: i64, url: &str) -> Tab {
        Tab {
            id,
            window_id: 1,
            index: 0,
            title: String::new(),
            url: url.to_owned(),
            active: false,
            pinned: false,
            audible: false,
            discarded: false,
            group_id: NO_GROUP,
            last_accessed: None,
        }
    }

    #[test]
    fn a_reference_round_trips_through_its_text() {
        let reference = TabRef::of("Profile 1", &tab(42, "https://a/"));

        let parsed = TabRef::parse(&reference.to_string());

        assert_eq!(parsed, Some(reference));
    }

    #[test]
    fn a_profile_may_contain_a_colon() {
        let reference = TabRef::of("odd:name", &tab(42, "https://a/"));

        let parsed = TabRef::parse(&reference.to_string()).expect("a valid reference");

        assert_eq!(parsed.profile, "odd:name");
        assert_eq!(parsed.id, 42);
    }

    #[test]
    fn a_reference_matches_only_the_url_it_was_made_from() {
        let reference = TabRef::of("P", &tab(1, "https://a/"));

        assert!(reference.matches_url("https://a/"));
        assert!(!reference.matches_url("https://a/other"));
    }

    #[test]
    fn nonsense_is_not_a_reference() {
        for text in [
            "",
            "P",
            "P:1",
            ":1:abcd1234",
            "P:x:abcd1234",
            "P:1:short",
            "P:1:abcd12345",
            "P:1:nothexxx",
        ] {
            assert_eq!(TabRef::parse(text), None, "{text}");
        }
    }

    #[test]
    fn the_host_is_found_past_user_information_and_ports() {
        for (url, host) in [
            ("https://Example.com/a", Some("example.com")),
            ("https://example.com:8080/a", Some("example.com")),
            ("https://meet.google.com@evil.com/", Some("evil.com")),
            ("https://evil.com/meet.google.com", Some("evil.com")),
            (
                "https://evil.com?x=https://meet.google.com/",
                Some("evil.com"),
            ),
            ("http://[::1]:80/", Some("::1")),
            ("about:blank", None),
            ("file:///etc/hosts", None),
        ] {
            assert_eq!(url_host(url).as_deref(), host, "{url}");
        }
    }

    #[test]
    fn urls_that_differ_anywhere_get_different_references() {
        let first = TabRef::of("Default", &tab(1, "https://example.com/a"));
        assert!(first.matches_url("https://example.com/a"));
        for other in [
            "https://example.com/b",
            "https://example.com/",
            "https://example.org/a",
            "c",
        ] {
            assert!(!first.matches_url(other), "{other}");
        }
    }
}
