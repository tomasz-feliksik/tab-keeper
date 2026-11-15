//! Asking the person before `close_tabs` closes anything: the question, built from what
//! Chrome shows now, and what an answer allows. Pure, so every answer can be tested
//! without a harness.

use std::collections::BTreeMap;
use std::time::Duration;

use rmcp::ServiceError;
use rmcp::model::{
    ClientCapabilities, ConstTitle, ElicitRequestParams, ElicitResult, ElicitationAction,
    ElicitationSchema, EnumSchema, MultiSelectEnumSchema, PrimitiveSchemaDefinition, TitledItems,
    TitledMultiSelectEnumSchema,
};
use serde_json::Value;

use crate::ops::{ClosePlan, Offer};

/// How long the person has to answer before nothing closes.
pub const TIMEOUT: Duration = Duration::from_mins(10);

/// What the person's answer allows.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Close these references, every one of them offered.
    Close(Vec<String>),
    /// Close nothing.
    Keep(Refusal),
}

/// Why nothing closes, as the agent is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Declined,
    Cancelled,
    ChoseNone,
    Unreadable,
    NoAnswer,
}

impl Refusal {
    /// What the agent is told the person did.
    pub fn answer(self) -> &'static str {
        match self {
            Self::Declined => "declined",
            Self::Cancelled => "cancelled",
            Self::ChoseNone => "chose_none",
            Self::Unreadable => "unreadable",
            Self::NoAnswer => "no_answer",
        }
    }
}

/// The one field the form has: the tabs to close, by reference.
const FIELD: &str = "close";
/// Hosts named in the first line before the rest are counted.
const HOSTS_NAMED: usize = 3;

/// Whether the harness can show a form. rmcp's `supported_elicitation_modes` says the
/// same, behind the feature `ask` in src/mcp.rs explains: a client that names no mode is
/// a 2025-06-18 client, which only has forms.
pub fn supports_form(capabilities: &ClientCapabilities) -> bool {
    capabilities
        .elicitation
        .as_ref()
        .is_some_and(|modes| modes.form.is_some() || modes.url.is_none())
}

/// The message lists every tab, because Codex shows only the message. The form offers
/// every tab again, all ticked, because Claude Code folds a message after two lines but
/// lists a multi-select's choices; so those two lines carry the summary.
pub fn question(plan: &ClosePlan) -> ElicitRequestParams {
    let mut lines = vec![summary(&plan.offered), provenance(plan.skipped.len())];
    lines.extend(plan.offered.iter().enumerate().map(|(number, offer)| {
        format!(
            // Bracketed, so the window's number cannot run into the next line's: Codex
            // shows the message with its line breaks removed.
            "{}. {} \u{2014} {} ({}, window {})",
            number + 1,
            offer.title,
            offer.host,
            offer.profile,
            offer.window
        )
    }));
    let choices = plan
        .offered
        .iter()
        .map(|offer| {
            ConstTitle::new(
                offer.reference.clone(),
                format!("{} \u{2014} {}", offer.title, offer.host),
            )
        })
        .collect();
    let field = TitledMultiSelectEnumSchema::new(TitledItems::new(choices))
        .with_title("Tabs to close")
        .with_description("Untick any tab to keep it open.")
        .with_default(plan.offered.iter().map(|o| o.reference.clone()).collect());
    let mut schema = ElicitationSchema::new(BTreeMap::from([(
        FIELD.to_owned(),
        PrimitiveSchemaDefinition::Enum(EnumSchema::Multi(MultiSelectEnumSchema::Titled(field))),
    )]));
    // Required, so a harness cannot send an accept with the field cleared; Codex, which
    // cannot draw the field at all, still sends one, and that is read as everything.
    schema.required = Some(vec![FIELD.to_owned()]);
    ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: lines.join("\n"),
        requested_schema: schema,
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn summary(offered: &[Offer]) -> String {
    let mut hosts: Vec<&str> = Vec::new();
    for offer in offered {
        if !hosts.contains(&offer.host.as_str()) {
            hosts.push(&offer.host);
        }
    }
    let mut named = hosts
        .iter()
        .take(HOSTS_NAMED)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if hosts.len() > HOSTS_NAMED {
        let more = plural(hosts.len() - HOSTS_NAMED, "more host", "more hosts");
        named = format!("{named} and {more}");
    }
    format!(
        "Close {} from {named}?",
        plural(offered.len(), "tab", "tabs")
    )
}

fn provenance(skipped: usize) -> String {
    let mut line = "Titles below were written by websites, not by tab-keeper.".to_owned();
    if skipped > 0 {
        let others = plural(skipped, "other tab", "other tabs");
        line =
            format!("{line} {others} asked for will stay open: stale, protected or unreachable.");
    }
    line
}

/// Only an accept closes anything, and only tabs that were offered. An accept without
/// the field closes everything offered: Codex cannot draw a multi-select, shows the
/// message with every title, and sends an accept with no content (owner, 2026-11-15).
pub fn verdict(offered: &[Offer], answer: Result<ElicitResult, ServiceError>) -> Verdict {
    let Ok(answer) = answer else {
        return Verdict::Keep(Refusal::NoAnswer);
    };
    match answer.action {
        ElicitationAction::Accept => chosen(offered, answer.content.as_ref()),
        ElicitationAction::Decline => Verdict::Keep(Refusal::Declined),
        ElicitationAction::Cancel => Verdict::Keep(Refusal::Cancelled),
        // A newer protocol's action is not an answer this code knows to be a yes.
        _ => Verdict::Keep(Refusal::Unreadable),
    }
}

fn chosen(offered: &[Offer], content: Option<&Value>) -> Verdict {
    let everything = || Verdict::Close(offered.iter().map(|o| o.reference.clone()).collect());
    let Some(content) = content else {
        return everything();
    };
    let Some(fields) = content.as_object() else {
        return Verdict::Keep(Refusal::Unreadable);
    };
    let Some(field) = fields.get(FIELD) else {
        return everything();
    };
    let Some(items) = field.as_array() else {
        return Verdict::Keep(Refusal::Unreadable);
    };
    let mut references: Vec<String> = Vec::new();
    for item in items {
        let Some(reference) = item.as_str() else {
            return Verdict::Keep(Refusal::Unreadable);
        };
        let known = offered.iter().any(|o| o.reference == reference);
        if !known || references.iter().any(|r| r == reference) {
            return Verdict::Keep(Refusal::Unreadable);
        }
        references.push(reference.to_owned());
    }
    if references.is_empty() {
        Verdict::Keep(Refusal::ChoseNone)
    } else {
        Verdict::Close(references)
    }
}

#[cfg(test)]
mod tests {
    use rmcp::model::{ElicitationCapability, FormElicitationCapability};
    use serde_json::json;

    use super::*;
    use crate::ops::TabResult;

    fn offer(id: i64, title: &str, host: &str) -> Offer {
        Offer {
            reference: format!("Default:{id}:0000000{id}"),
            title: title.to_owned(),
            host: host.to_owned(),
            profile: "Default".to_owned(),
            window: 7,
        }
    }

    fn plan(offered: Vec<Offer>, skipped: Vec<TabResult>) -> ClosePlan {
        ClosePlan {
            offered,
            skipped,
            failed: Vec::new(),
        }
    }

    fn two_hosts() -> ClosePlan {
        plan(
            vec![
                offer(1, "Invented page one", "example.com"),
                offer(2, "Invented page two", "example.org"),
                offer(3, "Invented page three", "example.com"),
            ],
            Vec::new(),
        )
    }

    /// The question as it travels, so the tests read what a harness reads.
    fn wire(params: &ElicitRequestParams) -> Value {
        serde_json::to_value(params).expect("serializes")
    }

    fn message(params: &ElicitRequestParams) -> String {
        wire(params)["message"]
            .as_str()
            .expect("a message")
            .to_owned()
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "an answer in the shape verdict takes"
    )]
    fn accepted(content: Option<Value>) -> Result<ElicitResult, ServiceError> {
        let mut result = ElicitResult::new(ElicitationAction::Accept);
        result.content = content;
        Ok(result)
    }

    fn references(plan: &ClosePlan) -> Vec<String> {
        plan.offered.iter().map(|o| o.reference.clone()).collect()
    }

    #[test]
    fn the_first_two_lines_say_how_many_close_from_which_hosts_and_who_wrote_the_titles() {
        let text = message(&question(&two_hosts()));
        let lines: Vec<&str> = text.lines().collect();

        assert!(lines.len() > 2, "{text}");
        assert!(lines[0].contains("Close 3 tabs"), "{}", lines[0]);
        assert!(lines[0].contains("example.com"), "{}", lines[0]);
        assert!(lines[0].contains("example.org"), "{}", lines[0]);
        assert!(lines[1].contains("written by websites"), "{}", lines[1]);
    }

    #[test]
    fn each_offered_tab_has_a_line_with_title_host_profile_and_window() {
        let text = message(&question(&two_hosts()));
        let tab_lines: Vec<&str> = text.lines().skip(2).filter(|l| !l.is_empty()).collect();

        assert_eq!(tab_lines.len(), 3, "{text}");
        for (line, (title, host)) in tab_lines.iter().zip([
            ("Invented page one", "example.com"),
            ("Invented page two", "example.org"),
            ("Invented page three", "example.com"),
        ]) {
            for part in [title, host, "Default", "window 7"] {
                assert!(line.contains(part), "{part:?} missing from {line:?}");
            }
        }
    }

    #[test]
    fn skipped_tabs_are_counted_and_never_offered() {
        let skipped = vec![
            TabResult::refused("Default:8:00000008".to_owned(), "pinned"),
            TabResult::refused("Default:9:00000009".to_owned(), "gone"),
        ];
        let asked = question(&plan(vec![offer(1, "Invented", "example.com")], skipped));
        let text = message(&asked);

        let first_two = text.lines().take(2).collect::<Vec<_>>().join("\n");
        assert!(first_two.contains("2 "), "{first_two}");
        assert!(!text.contains("Default:8"), "{text}");
        let choices = &wire(&asked)["requestedSchema"]["properties"]["close"]["items"]["anyOf"];
        assert_eq!(choices.as_array().map(Vec::len), Some(1), "{choices}");
    }

    #[test]
    fn every_offered_tab_is_one_ticked_choice_in_a_single_multi_select() {
        let asked = wire(&question(&two_hosts()));
        let schema = &asked["requestedSchema"];

        assert_eq!(asked["mode"], "form");
        assert_eq!(
            schema["properties"].as_object().map(serde_json::Map::len),
            Some(1)
        );
        assert_eq!(schema["required"], json!(["close"]));
        let close = &schema["properties"]["close"];
        assert_eq!(close["type"], "array");
        let choices: Vec<(&str, &str)> = close["items"]["anyOf"]
            .as_array()
            .expect("choices")
            .iter()
            .map(|c| {
                (
                    c["const"].as_str().expect("const"),
                    c["title"].as_str().expect("title"),
                )
            })
            .collect();
        assert_eq!(choices.len(), 3);
        assert_eq!(choices[1].0, "Default:2:00000002");
        assert!(
            choices[1].1.contains("Invented page two"),
            "{}",
            choices[1].1
        );
        assert!(choices[1].1.contains("example.org"), "{}", choices[1].1);
        assert_eq!(close["default"], json!(references(&two_hosts())));
    }

    #[test]
    fn accepting_some_tabs_closes_exactly_those() {
        let plan = two_hosts();
        let answer = accepted(Some(
            json!({"close": ["Default:3:00000003", "Default:1:00000001"]}),
        ));

        assert_eq!(
            verdict(&plan.offered, answer),
            Verdict::Close(vec![
                "Default:3:00000003".to_owned(),
                "Default:1:00000001".to_owned()
            ])
        );
    }

    #[test]
    fn an_answer_naming_a_tab_never_offered_closes_nothing() {
        let plan = two_hosts();
        let answer = accepted(Some(
            json!({"close": ["Default:1:00000001", "Default:9:00000009"]}),
        ));

        assert_eq!(
            verdict(&plan.offered, answer),
            Verdict::Keep(Refusal::Unreadable)
        );
    }

    #[test]
    fn accepting_without_the_field_closes_everything_offered() {
        let plan = two_hosts();

        for content in [None, Some(json!({}))] {
            assert_eq!(
                verdict(&plan.offered, accepted(content)),
                Verdict::Close(references(&plan))
            );
        }
    }

    #[test]
    fn accepting_an_empty_list_or_a_malformed_one_closes_nothing() {
        let plan = two_hosts();

        assert_eq!(
            verdict(&plan.offered, accepted(Some(json!({"close": []})))),
            Verdict::Keep(Refusal::ChoseNone)
        );
        for content in [
            json!({"close": "Default:1:00000001"}),
            json!({"close": [1]}),
            json!(["Default:1:00000001"]),
            json!({"close": ["Default:1:00000001", "Default:1:00000001"]}),
        ] {
            assert_eq!(
                verdict(&plan.offered, accepted(Some(content.clone()))),
                Verdict::Keep(Refusal::Unreadable),
                "{content}"
            );
        }
    }

    #[test]
    fn decline_and_cancel_close_nothing() {
        let plan = two_hosts();
        let everything = Some(json!({"close": references(&plan)}));

        for (action, refusal) in [
            (ElicitationAction::Decline, Refusal::Declined),
            (ElicitationAction::Cancel, Refusal::Cancelled),
        ] {
            let mut answer = ElicitResult::new(action);
            answer.content.clone_from(&everything);
            assert_eq!(verdict(&plan.offered, Ok(answer)), Verdict::Keep(refusal));
        }
    }

    #[test]
    fn an_error_or_a_timeout_closes_nothing() {
        let plan = two_hosts();

        for error in [
            ServiceError::Timeout { timeout: TIMEOUT },
            ServiceError::TransportClosed,
            ServiceError::McpError(rmcp::model::ErrorData::internal_error("no", None)),
        ] {
            assert_eq!(
                verdict(&plan.offered, Err(error)),
                Verdict::Keep(Refusal::NoAnswer)
            );
        }
    }

    #[test]
    fn form_elicitation_is_supported_when_declared_or_when_no_mode_is_named() {
        let with = |elicitation: Option<ElicitationCapability>| {
            let mut capabilities = ClientCapabilities::default();
            capabilities.elicitation = elicitation;
            supports_form(&capabilities)
        };
        let url_only: ElicitationCapability =
            serde_json::from_value(json!({"url": {}})).expect("parses");

        assert!(with(Some(
            ElicitationCapability::new().with_form(FormElicitationCapability::default())
        )));
        assert!(with(Some(ElicitationCapability::new())));
        assert!(!with(Some(url_only)));
        assert!(!with(None));
    }
}
