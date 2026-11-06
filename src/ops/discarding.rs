//! `discard`: unload idle tabs to free memory, leaving them in the tab strip.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::gather::{Gathered, LiveProfile, gather, parse_refs};
use super::results::{
    TabResult, all_failed, failed_profile_lines, pair_results, resolve, summarize, targets,
};
use super::{Env, OpError, Report};
use crate::display;
use crate::policy::{Exceptions, read_exceptions};
use crate::tabs::{Tab, TabRef};

const DAY_MS: u64 = 86_400_000;
const HUMAN_TITLE_CHARS: usize = 70;

#[derive(Clone, Debug, Default)]
pub struct DiscardOptions {
    pub idle_ms: Option<u64>,
    pub refs: Vec<String>,
    pub profile: Option<String>,
    pub dry_run: bool,
    pub allow: Vec<String>,
    pub policy_dirs: Vec<PathBuf>,
}

/// `90m`, `2h`, `30s`, `7d`.
pub fn parse_duration_ms(text: &str) -> Result<u64, String> {
    let bad = || format!("bad duration {text:?}: use a number and s, m, h or d, like 2h");
    let split = text.len().checked_sub(1).ok_or_else(bad)?;
    if !text.is_char_boundary(split) {
        return Err(bad());
    }
    let (number, unit) = text.split_at(split);
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(bad());
    }
    let unit_ms: u64 = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => DAY_MS,
        _ => return Err(bad()),
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|count| count.checked_mul(unit_ms))
        .ok_or_else(bad)
}

/// Why idle selection leaves a tab alone, or `None` if it is a candidate. A restart resets
/// `lastAccessed` to about the restore time, which only makes a tab look recently used,
/// so the mistake it allows is keeping a tab, never unloading one in use.
fn kept_because(
    tab: &Tab,
    idle_ms: u64,
    now_ms: u64,
    exceptions: &Exceptions,
    allow: &[String],
) -> Option<&'static str> {
    let allows = |protection: &str| allow.iter().any(|name| name == protection);
    if tab.discarded {
        return Some("already_discarded");
    }
    if tab.active && !allows("active") {
        return Some("active");
    }
    if tab.pinned && !allows("pinned") {
        return Some("pinned");
    }
    if tab.audible && !allows("audible") {
        return Some("audible");
    }
    if exceptions.covers(&tab.url) {
        return Some("policy_exempt");
    }
    #[allow(clippy::cast_precision_loss)] // Milliseconds since 1970 fit an f64 exactly.
    let idle_enough = tab
        .last_accessed
        .is_some_and(|accessed| now_ms as f64 - accessed >= idle_ms as f64);
    (!idle_enough).then_some("recently_used")
}

/// Reads the policy before asking anything, so a browser is never touched without it.
pub fn discard(env: &Env, options: &DiscardOptions) -> Result<Report, OpError> {
    if options.idle_ms.is_none() && options.refs.is_empty() {
        return Err(OpError::Usage(
            "say which tabs: --idle, or tab references".to_owned(),
        ));
    }
    let exceptions = read_exceptions(&options.policy_dirs)?;
    let gathered = gather(env)?;

    let mut plan: Vec<(&LiveProfile, Vec<&Tab>)> = Vec::new();
    let mut results: Vec<TabResult> = Vec::new();
    let mut kept: BTreeMap<&str, usize> = BTreeMap::new();
    if options.refs.is_empty() {
        let idle_ms = options.idle_ms.unwrap_or(u64::MAX);
        for profile in gathered.select(options.profile.as_deref())? {
            let mut chosen: Vec<&Tab> = Vec::new();
            for tab in &profile.snapshot.tabs {
                match kept_because(tab, idle_ms, env.now_ms, &exceptions, &options.allow) {
                    Some(reason) => *kept.entry(reason).or_default() += 1,
                    None => chosen.push(tab),
                }
            }
            plan.push((profile, chosen));
        }
    } else {
        let refs = parse_refs(&options.refs)?;
        let resolved = resolve(&refs, &gathered);
        results.extend(resolved.refused);
        for (profile, tabs) in resolved.accepted {
            let (exempt, allowed): (Vec<&Tab>, Vec<&Tab>) = tabs
                .into_iter()
                .partition(|tab| exceptions.covers(&tab.url));
            for tab in exempt {
                let reference = TabRef::of(&profile.name.name, tab).to_string();
                results.push(TabResult::refused(reference, "policy_exempt"));
            }
            plan.push((profile, allowed));
        }
    }

    if options.dry_run {
        return Ok(dry_run_report(&gathered, &plan, &results, &kept));
    }

    for (profile, tabs) in &plan {
        if tabs.is_empty() {
            continue;
        }
        let request = json!({"op": "discard", "targets": targets(tabs), "allow": options.allow});
        match profile.ask(env, &request) {
            Ok(answer) => results.extend(pair_results(profile, tabs, &answer)),
            Err(error) => results.extend(all_failed(profile, tabs, &error.to_string())),
        }
    }

    let complete = gathered.failed.is_empty() && results.iter().all(TabResult::is_done);
    let mut human = vec![summarize("unloaded", &results)];
    human.extend(failed_profile_lines(&gathered));
    Ok(Report {
        value: json!({
            "partial": !gathered.failed.is_empty(),
            "failed": gathered.failed,
            "results": results,
        }),
        human: human.join("\n"),
        complete,
    })
}

fn dry_run_report(
    gathered: &Gathered,
    plan: &[(&LiveProfile, Vec<&Tab>)],
    refused: &[TabResult],
    kept: &BTreeMap<&str, usize>,
) -> Report {
    let mut lines = Vec::new();
    let mut would = Vec::new();
    for (profile, tabs) in plan {
        lines.push(format!(
            "{}: would unload {} tabs",
            display::clean(&profile.name.name, 80),
            tabs.len()
        ));
        for tab in tabs {
            let reference = TabRef::of(&profile.name.name, tab).to_string();
            lines.push(format!(
                "  {}  {}",
                display::clean(&reference, 120),
                display::clean(&tab.title, HUMAN_TITLE_CHARS)
            ));
            would.push(json!({"ref": reference, "title": tab.title, "url": tab.url}));
        }
    }
    for (reason, count) in kept {
        lines.push(format!("kept {count}: {reason}"));
    }
    lines.extend(failed_profile_lines(gathered));
    let complete = gathered.failed.is_empty() && refused.is_empty();
    Report {
        value: json!({
            "dry_run": true,
            "partial": !gathered.failed.is_empty(),
            "failed": gathered.failed,
            "would_unload": would,
            "kept": kept,
            "results": refused,
        }),
        human: lines.join("\n"),
        complete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_and_garbage_is_refused() {
        assert_eq!(parse_duration_ms("90m"), Ok(5_400_000));
        assert_eq!(parse_duration_ms("30s"), Ok(30_000));
        assert_eq!(parse_duration_ms("2h"), Ok(7_200_000));
        assert_eq!(parse_duration_ms("7d"), Ok(604_800_000));
        for text in [
            "",
            "h",
            "2",
            "2 hours",
            "-1h",
            "1.5h",
            "2w",
            "ą",
            "99999999999999999d",
        ] {
            assert!(parse_duration_ms(text).is_err(), "{text}");
        }
    }
}
