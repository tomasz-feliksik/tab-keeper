//! `group save`, `open`, `list` and `delete`: a group closed to a named record. Chrome has
//! no API for its own saved groups (measured), so these appear only here.

use serde_json::{Value, json};

use super::closing::{record_for, reopen, settle_record};
use super::gather::gather;
use super::results::{TabResult, all_failed, pair_results, summarize, targets};
use super::{Env, OpError, Report};
use crate::display;
use crate::saved::{Record, RecordKind, Store};
use crate::tabs::Tab;

fn find(store: &Store, name: &str) -> Result<Option<Record>, OpError> {
    Ok(store
        .list()?
        .into_iter()
        .find(|r| r.kind == RecordKind::Group && r.name.as_deref() == Some(name)))
}

pub fn save_group(
    env: &Env,
    name: &str,
    profile: Option<&str>,
    group_title: &str,
    allow: &[String],
) -> Result<Report, OpError> {
    let store = env.store();
    if find(&store, name)?.is_some() {
        return Err(OpError::Usage(format!(
            "a saved group is already called {name:?}; delete it first"
        )));
    }
    let gathered = gather(env)?;
    let live = gathered.one(profile)?;
    let group_ids: Vec<i64> = live
        .snapshot
        .groups
        .iter()
        .filter(|group| group.title == group_title)
        .map(|group| group.id)
        .collect();
    let mut tabs: Vec<&Tab> = live
        .snapshot
        .tabs
        .iter()
        .filter(|tab| group_ids.contains(&tab.group_id))
        .collect();
    tabs.sort_by_key(|tab| (tab.window_id, tab.index));
    if tabs.is_empty() {
        return Err(OpError::Usage(format!(
            "no group in {} is titled {group_title:?}",
            live.name.name
        )));
    }

    let record = record_for(live, &tabs, RecordKind::Group, Some(name), env.now_ms);
    store.write(&record)?;
    let request = json!({"op": "close", "targets": targets(&tabs), "allow": allow});
    let outcome = live
        .ask(env, &request)
        .map(|answer| pair_results(live, &tabs, &answer));
    let results: Vec<TabResult> = match &outcome {
        Ok(results) => results.clone(),
        Err(error) => all_failed(live, &tabs, &error.to_string()),
    };
    let record_id = settle_record(env, record, &outcome)?;
    Ok(Report {
        human: summarize("saved and closed", &results),
        complete: results.iter().all(TabResult::is_done),
        value: json!({"name": name, "record": record_id, "results": results}),
    })
}

/// Opening leaves the record in place: a saved group can be opened any number of times.
pub fn open_group(env: &Env, name: &str) -> Result<Report, OpError> {
    let store = env.store();
    let record = find(&store, name)?
        .ok_or_else(|| OpError::Usage(format!("no saved group is called {name:?}")))?;
    let gathered = gather(env)?;
    reopen(env, &gathered, record)
}

pub fn list_groups_saved(env: &Env) -> Result<Report, OpError> {
    let records = env.store().list()?;
    let groups: Vec<Value> = records
        .iter()
        .filter(|record| record.kind == RecordKind::Group)
        .map(|record| {
            json!({
                "name": record.name,
                "profile": record.profile,
                "tabs": record.tabs.len(),
                "title": record.groups.first().map(|group| group.title.as_str()),
                "color": record.groups.first().map(|group| group.color.as_str()),
                "created_ms": record.created_ms,
            })
        })
        .collect();
    let lines: Vec<String> = groups
        .iter()
        .map(|group| {
            format!(
                "{}  {}  {} tabs",
                display::clean(group["name"].as_str().unwrap_or_default(), 60),
                display::clean(group["profile"].as_str().unwrap_or_default(), 60),
                group["tabs"],
            )
        })
        .collect();
    Ok(Report {
        value: json!({"groups": groups}),
        human: lines.join("\n"),
        complete: true,
    })
}

pub fn delete_group(env: &Env, name: &str) -> Result<Report, OpError> {
    let store = env.store();
    let record = find(&store, name)?
        .ok_or_else(|| OpError::Usage(format!("no saved group is called {name:?}")))?;
    store.delete(&record.id)?;
    Ok(Report {
        value: json!({"deleted": name}),
        human: format!("deleted saved group {}", display::clean(name, 60)),
        complete: true,
    })
}
