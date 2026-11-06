//! `bookmark`: a removal is saved first, so it can be undone like a closed tab.

use serde_json::{Value, json};

use super::closing::profile_for;
use super::gather::{gather, parse_refs};
use super::listing::{TITLE_CHARS, URL_CHARS};
use super::results::{TabResult, resolve};
use super::{Env, OpError, Report};
use crate::display::{self, compact};
use crate::saved::{Record, RecordKind};

const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, Default)]
pub struct BookmarkListOptions {
    pub profile: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Clone, Debug, Default)]
pub struct BookmarkAdd {
    pub profile: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub refs: Vec<String>,
    pub folder: Option<String>,
    pub parent: Option<String>,
}

fn flatten(node: &Value, depth: usize, profile: &str, into: &mut Vec<Value>) {
    if depth > MAX_DEPTH {
        return;
    }
    // Chrome's invisible root has no title and is not something to act on.
    if depth > 0 {
        into.push(json!({
            "profile": profile,
            "id": node["id"],
            "title": node["title"].as_str().map(|title| compact(title, TITLE_CHARS).0),
            "url": node["url"].as_str().map(|url| compact(url, URL_CHARS).0),
            "depth": depth - 1,
            "folder": node["url"].is_null(),
        }));
    }
    for child in node["children"].as_array().map_or(&[][..], Vec::as_slice) {
        flatten(child, depth + 1, profile, into);
    }
}

pub fn list_bookmarks(env: &Env, options: &BookmarkListOptions) -> Result<Report, OpError> {
    let gathered = gather(env)?;
    let mut entries = Vec::new();
    let mut failed: Vec<Value> = gathered
        .failed
        .iter()
        .map(|f| json!({"profile": f.profile, "error": f.error}))
        .collect();
    for profile in gathered.select(options.profile.as_deref())? {
        match profile.ask(env, &json!({"op": "bookmarks_list"})) {
            Ok(answer) => {
                for root in answer["tree"].as_array().map_or(&[][..], Vec::as_slice) {
                    flatten(root, 0, &profile.name.name, &mut entries);
                }
            }
            Err(error) => {
                failed.push(json!({"profile": profile.name.name, "error": error.to_string()}));
            }
        }
    }
    let total = entries.len();
    let page: Vec<Value> = entries
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect();
    let next_offset = (options.offset + page.len() < total).then_some(options.offset + page.len());
    let mut lines: Vec<String> = page
        .iter()
        .map(|entry| {
            let indent =
                "  ".repeat(usize::try_from(entry["depth"].as_u64().unwrap_or(0)).unwrap_or(0));
            format!(
                "{indent}{}  {}",
                display::clean(entry["id"].as_str().unwrap_or_default(), 20),
                display::clean(entry["title"].as_str().unwrap_or_default(), 70),
            )
        })
        .collect();
    if let Some(next) = next_offset {
        lines.push(format!(
            "showing {} of {total}; next page: --offset {next}",
            page.len()
        ));
    }
    for entry in &failed {
        lines.push(format!(
            "profile {} did not answer: {}",
            display::clean(entry["profile"].as_str().unwrap_or_default(), 80),
            display::clean(entry["error"].as_str().unwrap_or_default(), 200),
        ));
    }
    let complete = failed.is_empty();
    Ok(Report {
        value: json!({
            "partial": !complete,
            "failed": failed,
            "total": total,
            "offset": options.offset,
            "next_offset": next_offset,
            "bookmarks": page,
        }),
        human: lines.join("\n"),
        complete,
    })
}

pub fn add_bookmarks(env: &Env, options: &BookmarkAdd) -> Result<Report, OpError> {
    let gathered = gather(env)?;
    let mut refused = Vec::new();
    let (profile, items): (_, Vec<Value>) = if options.refs.is_empty() {
        let (Some(title), Some(url)) = (&options.title, &options.url) else {
            return Err(OpError::Usage(
                "give --title and --url, or tab references".to_owned(),
            ));
        };
        (
            gathered.one(options.profile.as_deref())?,
            vec![json!({"title": title, "url": url})],
        )
    } else {
        let refs = parse_refs(&options.refs)?;
        let resolved = resolve(&refs, &gathered);
        refused = resolved.refused;
        let mut accepted = resolved.accepted.into_iter();
        let Some((profile, tabs)) = accepted.next() else {
            return Ok(refused_report(&refused));
        };
        for (other, tabs) in accepted {
            for tab in tabs {
                refused.push(TabResult::refused(
                    crate::tabs::TabRef::of(&other.name.name, tab).to_string(),
                    "other_profile",
                ));
            }
        }
        let items = tabs
            .iter()
            .map(|tab| json!({"title": tab.title, "url": tab.url}))
            .collect();
        (profile, items)
    };

    let mut request = json!({"op": "bookmarks_add", "items": items});
    if let Some(folder) = &options.folder {
        request["folder"] = json!(folder);
    }
    if let Some(parent) = &options.parent {
        request["parentId"] = json!(parent);
    }
    let answer = profile.ask(env, &request)?;
    let empty = Vec::new();
    let results = answer["results"].as_array().unwrap_or(&empty);
    let added = results.iter().filter(|r| r["status"] == "done").count();
    let complete = refused.is_empty() && added == items_len(&request) && gathered.failed.is_empty();
    Ok(Report {
        human: format!(
            "bookmarked {added} of {}",
            items_len(&request) + refused.len()
        ),
        value: json!({
            "folder_id": answer["folderId"],
            "results": results,
            "refused": refused,
        }),
        complete,
    })
}

fn items_len(request: &Value) -> usize {
    request["items"].as_array().map_or(0, Vec::len)
}

fn refused_report(refused: &[TabResult]) -> Report {
    Report {
        human: format!("bookmarked 0 of {}", refused.len()),
        value: json!({"results": [], "refused": refused}),
        complete: false,
    }
}

/// Saved before it is removed. If the removal then fails, the record is withdrawn, since
/// there is nothing to undo.
pub fn remove_bookmark(env: &Env, profile: Option<&str>, id: &str) -> Result<Report, OpError> {
    let gathered = gather(env)?;
    let live = gathered.one(profile)?;
    let answer = live.ask(env, &json!({"op": "bookmarks_list", "nodeId": id}))?;
    let node = answer["tree"][0].clone();
    if node.is_null() {
        return Err(OpError::BadAnswer(live.name.name.clone()));
    }
    let mut record = Record::new(
        RecordKind::Bookmark,
        &live.name.instance,
        &live.name.name,
        env.now_ms,
    );
    record.name = node["title"].as_str().map(str::to_owned);
    record.bookmark = Some(node.clone());
    let store = env.store();
    store.write(&record)?;
    let removal = live.ask(
        env,
        &json!({"op": "bookmarks_remove", "nodeId": id, "expectedTitle": node["title"]}),
    );
    if let Err(error) = removal {
        store.delete(&record.id)?;
        return Err(error);
    }
    Ok(Report {
        human: format!(
            "removed {}; undo with: tab-keeper bookmark restore {}",
            display::clean(node["title"].as_str().unwrap_or_default(), 70),
            record.id
        ),
        value: json!({"removed": id, "record": record.id}),
        complete: true,
    })
}

pub fn restore_bookmark(env: &Env, id: &str) -> Result<Report, OpError> {
    let store = env.store();
    let mut record = store.load(id)?;
    let Some(node) = record.bookmark.clone() else {
        return Err(OpError::Usage(format!("{id} does not hold a bookmark")));
    };
    let gathered = gather(env)?;
    let profile = profile_for(&gathered, &record)?;
    let answer = profile.ask(
        env,
        &json!({
            "op": "bookmarks_restore",
            "node": node,
            "parentId": node["parentId"],
            "index": node["index"],
        }),
    )?;
    record.restored_ms = Some(env.now_ms);
    store.write(&record)?;
    Ok(Report {
        human: format!(
            "restored {}",
            display::clean(node["title"].as_str().unwrap_or_default(), 70)
        ),
        value: json!({"record": id, "bookmark_id": answer["bookmarkId"]}),
        complete: true,
    })
}
