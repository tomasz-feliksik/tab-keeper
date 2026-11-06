//! `list`: what is open, in bounded pages.

use serde::Serialize;
use serde_json::json;

use super::gather::{Gathered, LiveProfile, gather};
use super::results::failed_profile_lines;
use super::{Env, OpError, Report};
use crate::display::{self, compact};
use crate::tabs::{NO_GROUP, Tab, TabRef, url_host};

pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 1000;
pub const TITLE_CHARS: usize = 120;
pub const URL_CHARS: usize = 200;
const HUMAN_TITLE_CHARS: usize = 70;

#[derive(Clone, Debug, Default)]
pub struct ListOptions {
    pub profile: Option<String>,
    pub limit: Option<usize>,
    pub offset: usize,
    pub host: Option<String>,
    pub search: Option<String>,
    pub idle_over_ms: Option<u64>,
    pub full: bool,
}

#[allow(clippy::struct_excessive_bools)] // Each one mirrors a field Chrome reports.
#[derive(Debug, Serialize)]
struct TabEntry {
    #[serde(rename = "ref")]
    reference: String,
    profile: String,
    window: i64,
    index: i64,
    title: String,
    url: String,
    #[serde(skip_serializing_if = "is_false")]
    truncated: bool,
    #[serde(skip_serializing_if = "is_false")]
    active: bool,
    #[serde(skip_serializing_if = "is_false")]
    pinned: bool,
    #[serde(skip_serializing_if = "is_false")]
    audible: bool,
    #[serde(skip_serializing_if = "is_false")]
    discarded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idle_ms: Option<u64>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // The signature serde requires.
fn is_false(value: &bool) -> bool {
    !value
}

fn idle_ms(tab: &Tab, now_ms: u64) -> Option<u64> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Times, in range.
    tab.last_accessed
        .map(|accessed| now_ms.saturating_sub(accessed.max(0.0) as u64))
}

fn matches(tab: &Tab, options: &ListOptions, now_ms: u64) -> bool {
    let host_ok = options.host.as_deref().is_none_or(|wanted| {
        let wanted = wanted.to_ascii_lowercase();
        url_host(&tab.url).is_some_and(|host| {
            host == wanted
                || host
                    .strip_suffix(wanted.as_str())
                    .is_some_and(|prefix| prefix.ends_with('.'))
        })
    });
    let search_ok = options.search.as_deref().is_none_or(|wanted| {
        let wanted = wanted.to_lowercase();
        tab.title.to_lowercase().contains(&wanted) || tab.url.to_lowercase().contains(&wanted)
    });
    let idle_ok = options
        .idle_over_ms
        .is_none_or(|minimum| idle_ms(tab, now_ms).is_some_and(|idle| idle >= minimum));
    host_ok && search_ok && idle_ok
}

fn entry(profile: &LiveProfile, tab: &Tab, full: bool, now_ms: u64) -> TabEntry {
    let (title, title_cut) = if full {
        (tab.title.clone(), false)
    } else {
        compact(&tab.title, TITLE_CHARS)
    };
    let (url, url_cut) = if full {
        (tab.url.clone(), false)
    } else {
        compact(&tab.url, URL_CHARS)
    };
    let group = (tab.group_id != NO_GROUP)
        .then(|| {
            profile
                .snapshot
                .groups
                .iter()
                .find(|group| group.id == tab.group_id)
                .map(|group| group.title.clone())
        })
        .flatten();
    TabEntry {
        reference: TabRef::of(&profile.name.name, tab).to_string(),
        profile: profile.name.name.clone(),
        window: tab.window_id,
        index: tab.index,
        title,
        url,
        truncated: title_cut || url_cut,
        active: tab.active,
        pinned: tab.pinned,
        audible: tab.audible,
        discarded: tab.discarded,
        group,
        idle_ms: idle_ms(tab, now_ms),
    }
}

fn format_idle(idle: Option<u64>) -> String {
    let Some(idle) = idle else {
        return "?".to_owned();
    };
    let minutes = idle / 60_000;
    match minutes {
        0..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

fn format_flags(entry: &TabEntry) -> String {
    [
        (entry.active, 'A'),
        (entry.pinned, 'P'),
        (entry.audible, 'S'),
        (entry.discarded, 'D'),
    ]
    .iter()
    .map(|(on, flag)| if *on { *flag } else { '-' })
    .collect()
}

pub fn list(env: &Env, options: &ListOptions) -> Result<Report, OpError> {
    let gathered = gather(env)?;
    let selected = gathered.select(options.profile.as_deref())?;
    let limit = options.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let mut matching: Vec<(&LiveProfile, &Tab)> = selected
        .iter()
        .flat_map(|profile| profile.snapshot.tabs.iter().map(move |tab| (*profile, tab)))
        .filter(|(_, tab)| matches(tab, options, env.now_ms))
        .collect();
    matching.sort_by_key(|(profile, tab)| (profile.name.name.clone(), tab.window_id, tab.index));
    let total = matching.len();
    let page: Vec<TabEntry> = matching
        .iter()
        .skip(options.offset)
        .take(limit)
        .map(|(profile, tab)| entry(profile, tab, options.full, env.now_ms))
        .collect();
    let next_offset = (options.offset + page.len() < total).then_some(options.offset + page.len());

    let human = human_list(
        &gathered,
        &selected,
        &page,
        total,
        options.offset,
        next_offset,
    );
    let partial = !gathered.failed.is_empty();
    let value = json!({
        "partial": partial,
        "failed": gathered.failed,
        "not_connected": gathered.not_connected,
        "profiles": selected.iter().map(|p| json!({
            "name": p.name.name,
            "label": p.name.label,
            "tabs": p.snapshot.tabs.len(),
            "unloaded": p.snapshot.tabs.iter().filter(|t| t.discarded).count(),
        })).collect::<Vec<_>>(),
        "total": total,
        "offset": options.offset,
        "limit": limit,
        "next_offset": next_offset,
        "tabs": page,
    });
    Ok(Report {
        value,
        human,
        complete: !partial,
    })
}

fn human_list(
    gathered: &Gathered,
    selected: &[&LiveProfile],
    page: &[TabEntry],
    total: usize,
    offset: usize,
    next_offset: Option<usize>,
) -> String {
    let mut lines = Vec::new();
    for profile in selected {
        let unloaded = profile.snapshot.tabs.iter().filter(|t| t.discarded).count();
        lines.push(format!(
            "{}: {} tabs, {unloaded} unloaded",
            display::clean(&profile.name.name, 80),
            profile.snapshot.tabs.len(),
        ));
    }
    lines.extend(page.iter().map(|tab| {
        format!(
            "  {} {:>4}  {}  {}",
            format_flags(tab),
            format_idle(tab.idle_ms),
            display::clean(&tab.reference, 120),
            display::clean(&tab.title, HUMAN_TITLE_CHARS),
        )
    }));
    if let Some(next) = next_offset {
        lines.push(format!(
            "showing {}-{} of {total}; next page: --offset {next}",
            offset + 1,
            offset + page.len(),
        ));
    }
    for sleeping in &gathered.not_connected {
        lines.push(format!("not connected: {}", display::clean(sleeping, 80)));
    }
    lines.extend(failed_profile_lines(gathered));
    lines.join("\n")
}

/// Connected, failed and sleeping profiles, without any tab data.
pub fn list_profiles(env: &Env) -> Result<Report, OpError> {
    let gathered = gather(env)?;
    let partial = !gathered.failed.is_empty();
    let value = json!({
        "partial": partial,
        "profiles": gathered.live.iter().map(|p| json!({
            "name": p.name.name,
            "label": p.name.label,
            "tabs": p.snapshot.tabs.len(),
            "groups": p.snapshot.groups.len(),
        })).collect::<Vec<_>>(),
        "failed": gathered.failed,
        "not_connected": gathered.not_connected,
    });
    let mut lines: Vec<String> = gathered
        .live
        .iter()
        .map(|p| {
            format!(
                "{}: {} tabs, {} groups",
                display::clean(&p.name.name, 80),
                p.snapshot.tabs.len(),
                p.snapshot.groups.len()
            )
        })
        .collect();
    lines.extend(
        gathered
            .not_connected
            .iter()
            .map(|n| format!("not connected: {}", display::clean(n, 80))),
    );
    lines.extend(failed_profile_lines(&gathered));
    Ok(Report {
        value,
        human: lines.join("\n"),
        complete: !partial,
    })
}

/// Every tab group, with how many tabs each holds.
pub fn list_groups(env: &Env, profile: Option<&str>) -> Result<Report, OpError> {
    let gathered = gather(env)?;
    let selected = gathered.select(profile)?;
    let partial = !gathered.failed.is_empty();
    let groups: Vec<_> = selected
        .iter()
        .flat_map(|p| {
            p.snapshot.groups.iter().map(move |group| {
                json!({
                    "profile": p.name.name,
                    "id": group.id,
                    "window": group.window_id,
                    "title": compact(&group.title, TITLE_CHARS).0,
                    "color": group.color,
                    "collapsed": group.collapsed,
                    "tabs": p.snapshot.tabs.iter().filter(|t| t.group_id == group.id).count(),
                })
            })
        })
        .collect();
    let mut lines: Vec<String> = groups
        .iter()
        .map(|g| {
            format!(
                "{} {}: {} tabs, {}",
                display::clean(g["profile"].as_str().unwrap_or_default(), 80),
                display::clean(g["title"].as_str().unwrap_or_default(), 60),
                g["tabs"],
                display::clean(g["color"].as_str().unwrap_or_default(), 20),
            )
        })
        .collect();
    lines.extend(failed_profile_lines(&gathered));
    Ok(Report {
        value: json!({"partial": partial, "failed": gathered.failed, "groups": groups}),
        human: lines.join("\n"),
        complete: !partial,
    })
}
