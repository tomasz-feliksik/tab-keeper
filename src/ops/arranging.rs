//! `move`, `group` and `ungroup`. Chrome adjusts what it is asked to do, keeping pinned
//! tabs in front and pulling grouped tabs into one window, so every result says where a
//! tab really ended up.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::gather::{Gathered, LiveProfile, gather, parse_refs};
use super::results::{
    Resolved, TabResult, all_failed, failed_profile_lines, pair_results, resolve, summarize,
    targets,
};
use super::{Env, OpError, Report};
use crate::tabs::{Tab, TabRef};

pub const GROUP_COLORS: [&str; 9] = [
    "grey", "blue", "red", "yellow", "green", "pink", "purple", "cyan", "orange",
];

#[derive(Clone, Debug, Default)]
pub struct MoveOptions {
    pub refs: Vec<String>,
    pub window: Option<i64>,
    pub index: i64,
}

#[derive(Clone, Debug, Default)]
pub struct GroupOptions {
    pub refs: Vec<String>,
    pub title: Option<String>,
    pub color: Option<String>,
    pub allow: Vec<String>,
}

/// Tabs cannot leave their profile, so a request applies to the first tab's profile, and
/// tabs from any other are refused one by one rather than failing the rest.
fn single_profile(resolved: Resolved<'_>) -> (Option<(&LiveProfile, Vec<&Tab>)>, Vec<TabResult>) {
    let mut refused = resolved.refused;
    let mut accepted = resolved.accepted.into_iter();
    let first = accepted.next();
    for (profile, tabs) in accepted {
        for tab in tabs {
            let reference = TabRef::of(&profile.name.name, tab).to_string();
            refused.push(TabResult::refused(reference, "other_profile"));
        }
    }
    (first, refused)
}

fn report(verb: &str, gathered: &Gathered, results: &[TabResult], extra: Value) -> Report {
    let complete = gathered.failed.is_empty() && results.iter().all(TabResult::is_done);
    let mut lines = vec![summarize(verb, results)];
    lines.extend(results.iter().filter(|r| r.is_done()).map(|result| {
        format!(
            "  {} is at window {} position {}",
            crate::display::clean(&result.reference, 120),
            result.details.get("windowId").unwrap_or(&Value::Null),
            result.details.get("index").unwrap_or(&Value::Null),
        )
    }));
    lines.extend(failed_profile_lines(gathered));
    let mut value = json!({
        "partial": !gathered.failed.is_empty(),
        "failed": gathered.failed,
        "results": results,
    });
    if let (Some(value), Value::Object(extra)) = (value.as_object_mut(), extra) {
        value.extend(extra);
    }
    Report {
        value,
        human: lines.join("\n"),
        complete,
    }
}

pub fn move_tabs(env: &Env, options: &MoveOptions) -> Result<Report, OpError> {
    let refs = parse_refs(&options.refs)?;
    let gathered = gather(env)?;
    let (accepted, mut results) = single_profile(resolve(&refs, &gathered));
    if let Some((profile, tabs)) = accepted {
        let mut request = json!({"op": "move", "targets": targets(&tabs), "index": options.index});
        if let Some(window) = options.window {
            request["windowId"] = json!(window);
        }
        match profile.ask(env, &request) {
            Ok(answer) => results.extend(pair_results(profile, &tabs, &answer)),
            Err(error) => results.extend(all_failed(profile, &tabs, &error.to_string())),
        }
    }
    Ok(report("moved", &gathered, &results, json!({})))
}

pub fn group(env: &Env, options: &GroupOptions) -> Result<Report, OpError> {
    if let Some(color) = &options.color
        && !GROUP_COLORS.contains(&color.as_str())
    {
        return Err(OpError::Usage(format!(
            "unknown colour {color:?}; choose from {}",
            GROUP_COLORS.join(", ")
        )));
    }
    let refs = parse_refs(&options.refs)?;
    let gathered = gather(env)?;
    let (accepted, mut results) = single_profile(resolve(&refs, &gathered));
    let mut extra = json!({});
    if let Some((profile, tabs)) = accepted {
        let mut request = json!({"op": "group", "targets": targets(&tabs), "allow": options.allow});
        if let Some(title) = &options.title {
            request["title"] = json!(title);
            // The same title in this profile means extending that group.
            let existing = profile.snapshot.groups.iter().find(|g| &g.title == title);
            if let Some(existing) = existing {
                request["groupId"] = json!(existing.id);
            }
        }
        if let Some(color) = &options.color {
            request["color"] = json!(color);
        }
        match profile.ask(env, &request) {
            Ok(answer) => {
                let tab_results = pair_results(profile, &tabs, &answer);
                extra = json!({
                    "group_id": answer["groupId"],
                    "closed_windows": emptied_windows(profile, &tab_results),
                });
                results.extend(tab_results);
            }
            Err(error) => results.extend(all_failed(profile, &tabs, &error.to_string())),
        }
    }
    Ok(report("grouped", &gathered, &results, extra))
}

/// Windows that lost every tab to this group, which Chrome closes.
fn emptied_windows(profile: &LiveProfile, results: &[TabResult]) -> Vec<i64> {
    let window_of = |result: &TabResult, key: &str| result.details.get(key).and_then(Value::as_i64);
    let moved: Vec<&TabResult> = results
        .iter()
        .filter(|r| r.is_done() && window_of(r, "fromWindowId") != window_of(r, "windowId"))
        .collect();
    let ids: Vec<i64> = moved
        .iter()
        .filter_map(|r| TabRef::parse(&r.reference).map(|reference| reference.id))
        .collect();
    let from: BTreeSet<i64> = moved
        .iter()
        .filter_map(|r| window_of(r, "fromWindowId"))
        .collect();
    from.into_iter()
        .filter(|window| {
            profile
                .snapshot
                .tabs
                .iter()
                .filter(|tab| tab.window_id == *window)
                .all(|tab| ids.contains(&tab.id))
        })
        .collect()
}

pub fn ungroup(env: &Env, refs: &[String]) -> Result<Report, OpError> {
    let refs = parse_refs(refs)?;
    let gathered = gather(env)?;
    let (accepted, mut results) = single_profile(resolve(&refs, &gathered));
    if let Some((profile, tabs)) = accepted {
        let request = json!({"op": "ungroup", "targets": targets(&tabs)});
        match profile.ask(env, &request) {
            Ok(answer) => results.extend(pair_results(profile, &tabs, &answer)),
            Err(error) => results.extend(all_failed(profile, &tabs, &error.to_string())),
        }
    }
    Ok(report("ungrouped", &gathered, &results, json!({})))
}
