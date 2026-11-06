//! `close` and `restore`: nothing is closed before it is saved.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::gather::{Gathered, LiveProfile, gather, parse_refs};
use super::listing::{TITLE_CHARS, URL_CHARS};
use super::results::{
    TabResult, all_failed, failed_profile_lines, pair_results, resolve, summarize, targets,
};
use super::{Env, OpError, Report};
use crate::display::{self, compact};
use crate::saved::{Record, RecordKind, SavedGroup, SavedTab};
use crate::tabs::{NO_GROUP, Tab};

/// Everything needed to put these tabs back, so far as a tab can be put back.
pub fn record_for(
    profile: &LiveProfile,
    tabs: &[&Tab],
    kind: RecordKind,
    name: Option<&str>,
    now_ms: u64,
) -> Record {
    let mut record = Record::new(kind, &profile.name.instance, &profile.name.name, now_ms);
    record.name = name.map(str::to_owned);
    let mut groups = BTreeMap::new();
    for tab in tabs {
        let group = (tab.group_id != NO_GROUP).then(|| tab.group_id.to_string());
        if let Some(key) = &group
            && let Some(found) = profile
                .snapshot
                .groups
                .iter()
                .find(|g| g.id == tab.group_id)
        {
            groups.insert(
                key.clone(),
                SavedGroup {
                    key: key.clone(),
                    title: found.title.clone(),
                    color: found.color.clone(),
                    collapsed: found.collapsed,
                },
            );
        }
        record.tabs.push(SavedTab {
            url: tab.url.clone(),
            title: tab.title.clone(),
            window: tab.window_id,
            index: tab.index,
            pinned: tab.pinned,
            group,
        });
    }
    record.groups = groups.into_values().collect();
    record
}

/// The record now says what actually closed. Any tab whose fate is unknown stays in it,
/// because a restore that opens a tab twice beats one that loses it.
pub fn settle_record(
    env: &Env,
    mut record: Record,
    outcome: &Result<Vec<TabResult>, OpError>,
) -> Result<Option<String>, OpError> {
    let Ok(results) = outcome else {
        return Ok(Some(record.id));
    };
    let mut results = results.iter();
    record
        .tabs
        .retain(|_| results.next().is_none_or(TabResult::may_have_closed));
    let store = env.store();
    if record.tabs.is_empty() {
        store.delete(&record.id)?;
        return Ok(None);
    }
    let used: Vec<String> = record.tabs.iter().filter_map(|t| t.group.clone()).collect();
    record.groups.retain(|group| used.contains(&group.key));
    store.write(&record)?;
    Ok(Some(record.id))
}

pub fn close(env: &Env, refs: &[String], allow: &[String]) -> Result<Report, OpError> {
    let refs = parse_refs(refs)?;
    if refs.is_empty() {
        return Err(OpError::Usage("say which tabs to close".to_owned()));
    }
    let gathered = gather(env)?;
    let resolved = resolve(&refs, &gathered);
    let mut results = resolved.refused;

    // Every record is on disk before any tab anywhere closes.
    let store = env.store();
    let mut planned = Vec::new();
    for (profile, tabs) in &resolved.accepted {
        let record = record_for(profile, tabs, RecordKind::Closed, None, env.now_ms);
        store.write(&record)?;
        planned.push((*profile, tabs, record));
    }

    let mut records = Vec::new();
    for (profile, tabs, record) in planned {
        let request = json!({"op": "close", "targets": targets(tabs), "allow": allow});
        let outcome = profile
            .ask(env, &request)
            .map(|answer| pair_results(profile, tabs, &answer));
        match &outcome {
            Ok(tab_results) => results.extend(tab_results.iter().cloned()),
            Err(error) => results.extend(all_failed(profile, tabs, &error.to_string())),
        }
        if let Some(id) = settle_record(env, record, &outcome)? {
            records.push(id);
        }
    }

    let complete = gathered.failed.is_empty() && results.iter().all(TabResult::is_done);
    let mut lines = vec![summarize("closed", &results)];
    if !records.is_empty() {
        lines.push(format!(
            "undo with: tab-keeper restore {}",
            records.join(" ")
        ));
    }
    lines.extend(failed_profile_lines(&gathered));
    Ok(Report {
        value: json!({
            "partial": !gathered.failed.is_empty(),
            "failed": gathered.failed,
            "records": records,
            "results": results,
        }),
        human: lines.join("\n"),
        complete,
    })
}

/// The profile a record belongs to. Its instance is exact; the directory name covers an
/// extension that was reinstalled. Any other profile would be the wrong place to open it.
pub fn profile_for<'a>(
    gathered: &'a Gathered,
    record: &Record,
) -> Result<&'a LiveProfile, OpError> {
    gathered
        .live
        .iter()
        .find(|p| p.name.instance == record.instance)
        .or_else(|| gathered.live.iter().find(|p| p.name.name == record.profile))
        .ok_or_else(|| OpError::ProfileMissing {
            id: record.id.clone(),
            profile: record.profile.clone(),
        })
}

pub fn reopen(env: &Env, gathered: &Gathered, mut record: Record) -> Result<Report, OpError> {
    let profile = profile_for(gathered, &record)?;
    let tabs: Vec<Value> = record
        .tabs
        .iter()
        .map(|tab| {
            json!({
                "url": tab.url,
                "window": tab.window,
                "index": tab.index,
                "pinned": tab.pinned,
                "group": tab.group,
            })
        })
        .collect();
    let request = json!({"op": "restore", "tabs": tabs, "groups": record.groups});
    let answer = profile.ask(env, &request)?;

    let empty = Vec::new();
    let reported = answer["results"].as_array().unwrap_or(&empty);
    let results: Vec<Value> = record
        .tabs
        .iter()
        .enumerate()
        .map(|(position, tab)| {
            let mut result = reported
                .iter()
                .find(|r| r["position"].as_u64() == u64::try_from(position).ok())
                .cloned()
                .unwrap_or_else(
                    || json!({"status": "failed", "reason": "no_result_from_extension"}),
                );
            result["url"] = json!(compact(&tab.url, URL_CHARS).0);
            result["title"] = json!(compact(&tab.title, TITLE_CHARS).0);
            result
        })
        .collect();
    let group_problems = answer["groupProblems"].clone();
    let opened = results.iter().filter(|r| r["status"] == "done").count();
    let complete = opened == results.len() && group_problems.as_array().is_none_or(Vec::is_empty);

    if opened > 0 {
        record.restored_ms = Some(env.now_ms);
        env.store().write(&record)?;
    }

    let mut lines = vec![format!(
        "reopened {opened} of {} tabs in {}. Page state, scroll position and form contents \
         cannot be restored",
        results.len(),
        display::clean(&profile.name.name, 80),
    )];
    for result in results.iter().filter(|r| r["status"] != "done") {
        lines.push(format!(
            "  {}: {}",
            display::clean(result["reason"].as_str().unwrap_or("no reason given"), 80),
            display::clean(result["title"].as_str().unwrap_or_default(), 70),
        ));
    }
    Ok(Report {
        value: json!({
            "record": record.id,
            "profile": profile.name.name,
            "results": results,
            "group_problems": group_problems,
        }),
        human: lines.join("\n"),
        complete,
    })
}

pub fn restore(env: &Env, id: &str, again: bool) -> Result<Report, OpError> {
    let record = env.store().load(id)?;
    if record.kind == RecordKind::Bookmark {
        return Err(OpError::Usage(format!(
            "{id} holds a bookmark: use `tab-keeper bookmark restore`"
        )));
    }
    if record.restored_ms.is_some() && !again {
        return Err(OpError::AlreadyRestored(id.to_owned()));
    }
    let gathered = gather(env)?;
    reopen(env, &gathered, record)
}

/// What is saved, without the URLs or titles: those are read one record at a time.
pub fn list_saved(env: &Env, limit: usize, offset: usize) -> Result<Report, OpError> {
    let records = env.store().list()?;
    let total = records.len();
    let page: Vec<Value> = records
        .iter()
        .skip(offset)
        .take(limit)
        .map(|record| {
            json!({
                "id": record.id,
                "kind": record.kind,
                "name": record.name.as_deref().map(|name| compact(name, TITLE_CHARS).0),
                "profile": record.profile,
                "created_ms": record.created_ms,
                "restored_ms": record.restored_ms,
                "tabs": record.tabs.len(),
            })
        })
        .collect();
    let lines: Vec<String> = page
        .iter()
        .map(|record| {
            format!(
                "{}  {}  {} tabs  {}{}",
                record["id"].as_str().unwrap_or_default(),
                display::clean(record["profile"].as_str().unwrap_or_default(), 60),
                record["tabs"],
                display::clean(record["name"].as_str().unwrap_or_default(), 60),
                if record["restored_ms"].is_null() {
                    ""
                } else {
                    "  (restored)"
                },
            )
        })
        .collect();
    let next_offset = (offset + page.len() < total).then_some(offset + page.len());
    Ok(Report {
        value: json!({"total": total, "offset": offset, "next_offset": next_offset, "records": page}),
        human: lines.join("\n"),
        complete: true,
    })
}
