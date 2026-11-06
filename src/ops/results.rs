//! Every tab in a batch gets its own result, however the batch went.

use serde::Serialize;
use serde_json::{Map, Value};

use super::gather::{Gathered, LiveProfile};
use crate::display;
use crate::tabs::{Tab, TabRef};

pub const DONE: &str = "done";
pub const REFUSED: &str = "refused";
pub const FAILED: &str = "failed";
const NO_RESULT: &str = "no_result_from_extension";
const HUMAN_TITLE_CHARS: usize = 60;

#[derive(Clone, Debug, Serialize)]
pub struct TabResult {
    #[serde(rename = "ref")]
    pub reference: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What Chrome reported, such as where a moved tab really ended up.
    #[serde(flatten)]
    pub details: Map<String, Value>,
}

impl TabResult {
    pub fn refused(reference: String, reason: &str) -> Self {
        Self::without_details(reference, REFUSED, reason)
    }

    pub fn failed(reference: String, reason: &str) -> Self {
        Self::without_details(reference, FAILED, reason)
    }

    fn without_details(reference: String, status: &str, reason: &str) -> Self {
        Self {
            reference,
            status: status.to_owned(),
            reason: Some(reason.to_owned()),
            details: Map::new(),
        }
    }

    pub fn is_done(&self) -> bool {
        self.status == DONE
    }

    /// Whether a tab asked to close might be gone. Only an answer naming it can say no.
    pub fn may_have_closed(&self) -> bool {
        self.is_done() || self.reason.as_deref() == Some(NO_RESULT)
    }
}

/// References sorted into the profiles and tabs they still name, and refusals for the
/// rest. This is the first of two URL checks; the extension makes the last.
pub struct Resolved<'a> {
    pub accepted: Vec<(&'a LiveProfile, Vec<&'a Tab>)>,
    pub refused: Vec<TabResult>,
}

pub fn resolve<'a>(refs: &[TabRef], gathered: &'a Gathered) -> Resolved<'a> {
    let mut accepted: Vec<(&'a LiveProfile, Vec<&'a Tab>)> = Vec::new();
    let mut refused = Vec::new();
    for reference in refs {
        let text = reference.to_string();
        let Some(profile) = gathered
            .live
            .iter()
            .find(|p| p.name.name == reference.profile)
        else {
            let reason = if gathered
                .failed
                .iter()
                .any(|f| f.profile == reference.profile)
            {
                "profile_did_not_answer"
            } else {
                "profile_not_connected"
            };
            refused.push(TabResult::refused(text, reason));
            continue;
        };
        let Some(tab) = profile.tab(reference.id) else {
            refused.push(TabResult::refused(text, "gone"));
            continue;
        };
        if !reference.matches_url(&tab.url) {
            refused.push(TabResult::refused(text, "url_changed"));
            continue;
        }
        match accepted.iter_mut().find(|(p, _)| std::ptr::eq(*p, profile)) {
            Some((_, tabs)) => tabs.push(tab),
            None => accepted.push((profile, vec![tab])),
        }
    }
    Resolved { accepted, refused }
}

/// The `targets` a request names. The URL goes along so the extension can check it again
/// at the moment of acting.
pub fn targets(tabs: &[&Tab]) -> Value {
    tabs.iter()
        .map(|tab| serde_json::json!({"id": tab.id, "url": tab.url}))
        .collect()
}

/// Pairs the extension's results with the tabs that were sent. A tab it did not mention
/// is a failure, so nothing can go missing from a batch unnoticed.
pub fn pair_results(profile: &LiveProfile, tabs: &[&Tab], answer: &Value) -> Vec<TabResult> {
    let empty = Vec::new();
    let reported = answer["results"].as_array().unwrap_or(&empty);
    tabs.iter()
        .map(|tab| {
            let reference = TabRef::of(&profile.name.name, tab).to_string();
            let Some(result) = reported.iter().find(|r| r["id"].as_i64() == Some(tab.id)) else {
                return TabResult::failed(reference, NO_RESULT);
            };
            let mut details = result.as_object().cloned().unwrap_or_default();
            details.remove("id");
            let status = details.remove("status");
            let reason = details.remove("reason");
            TabResult {
                reference,
                status: status
                    .and_then(|s| s.as_str().map(str::to_owned))
                    .unwrap_or_else(|| FAILED.to_owned()),
                reason: reason.and_then(|r| r.as_str().map(str::to_owned)),
                details,
            }
        })
        .collect()
}

/// When a whole request fails, its outcome is unknown, and each tab says so.
pub fn all_failed(profile: &LiveProfile, tabs: &[&Tab], reason: &str) -> Vec<TabResult> {
    tabs.iter()
        .map(|tab| TabResult::failed(TabRef::of(&profile.name.name, tab).to_string(), reason))
        .collect()
}

pub fn summarize(verb: &str, results: &[TabResult]) -> String {
    let done = results.iter().filter(|r| r.is_done()).count();
    let mut lines = vec![format!("{verb} {done} of {} tabs", results.len())];
    for result in results.iter().filter(|r| !r.is_done()) {
        lines.push(format!(
            "  {} {}: {}",
            result.status,
            display::clean(&result.reference, HUMAN_TITLE_CHARS * 2),
            display::clean(
                result.reason.as_deref().unwrap_or("no reason given"),
                HUMAN_TITLE_CHARS * 2
            ),
        ));
    }
    lines.join("\n")
}

/// Lines naming profiles that could not be asked, for any report that depended on them.
pub fn failed_profile_lines(gathered: &Gathered) -> Vec<String> {
    gathered
        .failed
        .iter()
        .map(|f| {
            format!(
                "profile {} did not answer: {}",
                display::clean(&f.profile, 80),
                f.error
            )
        })
        .collect()
}
