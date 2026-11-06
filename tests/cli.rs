//! The guarantees in `docs/functionality.md`, through the real binary, against stand-in
//! profiles. `tests/e2e` holds the same promises against real Chromium.

mod support;

use std::fs;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use support::{
    INSTANCE_A, INSTANCE_B, World, answer_json, answer_obligingly, exit_code, make_read_only,
    obliging, tab,
};

const URL: &str = "https://example.com/page";
const PARTIAL: i32 = 2;
const ERROR: i32 = 1;

fn refs_of(world: &World) -> Vec<String> {
    answer_json(&world.run(&["list"]))["tabs"]
        .as_array()
        .expect("tabs")
        .iter()
        .map(|tab| tab["ref"].as_str().expect("a reference").to_owned())
        .collect()
}

fn saved_files(world: &World) -> Vec<String> {
    fs::read_dir(world.saved_dir())
        .map(|entries| {
            entries
                .map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .into_string()
                        .expect("utf-8")
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn reports_its_version() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tab-keeper"))
        .arg("--version")
        .output()
        .expect("the binary runs");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("the version is UTF-8");
    assert_eq!(
        stdout.trim(),
        format!("tab-keeper {}", env!("CARGO_PKG_VERSION"))
    );
}

// Listing

#[test]
fn no_connected_profile_is_an_error_rather_than_an_empty_list() {
    let world = World::new();

    let output = world.run(&["list"]);

    assert_eq!(exit_code(&output), ERROR);
    assert_eq!(output.stdout, b"");
    assert!(String::from_utf8_lossy(&output.stderr).contains("no profile is connected"));
}

#[test]
fn a_profile_that_does_not_answer_is_reported_and_marks_the_result_partial() {
    let world = World::new();
    let _healthy = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let _silent = world.profile(INSTANCE_B, Box::new(|_| Value::Null));

    let output = world.run(&["list"]);

    assert_eq!(exit_code(&output), PARTIAL);
    let answer = answer_json(&output);
    assert_eq!(answer["partial"], true);
    assert_eq!(answer["failed"][0]["instance"], INSTANCE_B);
    assert_eq!(answer["total"], 1);
}

#[test]
fn a_profile_that_refuses_is_reported_too() {
    let world = World::new();
    let _refusing = world.profile(
        INSTANCE_A,
        Box::new(|request| json!({"id": request["id"], "ok": false, "error": "switched off"})),
    );

    let output = world.run(&["list"]);

    assert_eq!(exit_code(&output), PARTIAL);
    assert!(
        answer_json(&output)["failed"][0]["error"]
            .as_str()
            .expect("an error")
            .contains("switched off")
    );
}

#[test]
fn a_list_is_bounded_by_default_and_says_where_the_next_page_starts() {
    let world = World::new();
    let tabs = (0..300).map(|id| tab(id, URL)).collect();
    let _profile = world.profile(INSTANCE_A, obliging(tabs));

    let answer = answer_json(&world.run(&["list"]));

    assert_eq!(answer["tabs"].as_array().expect("tabs").len(), 50);
    assert_eq!(answer["total"], 300);
    assert_eq!(answer["next_offset"], 50);

    let last = answer_json(&world.run(&["list", "--offset", "290"]));
    assert_eq!(last["tabs"].as_array().expect("tabs").len(), 10);
    assert_eq!(last["next_offset"], Value::Null);

    let capped = answer_json(&world.run(&["list", "--limit", "100000"]));
    assert_eq!(capped["limit"], 1000);
}

#[test]
fn long_titles_are_shortened_and_flagged_unless_asked_in_full() {
    let world = World::new();
    let mut long = tab(1, URL);
    long["title"] = json!("t".repeat(5000));
    let _profile = world.profile(INSTANCE_A, obliging(vec![long]));

    let short = answer_json(&world.run(&["list"]));
    assert_eq!(short["tabs"][0]["truncated"], true);
    assert!(short["tabs"][0]["title"].as_str().expect("title").len() < 500);

    let full = answer_json(&world.run(&["list", "--full"]));
    assert_eq!(
        full["tabs"][0]["title"].as_str().expect("title").len(),
        5000
    );
}

#[test]
fn a_host_filter_takes_subdomains_but_not_lookalikes() {
    let world = World::new();
    let tabs = vec![
        tab(1, "https://example.com/"),
        tab(2, "https://mail.example.com/"),
        tab(3, "https://notexample.com/"),
        tab(4, "https://evil.test/example.com"),
    ];
    let _profile = world.profile(INSTANCE_A, obliging(tabs));

    let answer = answer_json(&world.run(&["list", "--host", "example.com"]));

    assert_eq!(answer["total"], 2);
}

#[test]
fn human_output_strips_what_a_title_could_use_to_control_a_terminal() {
    let world = World::new();
    let mut hostile = tab(1, URL);
    hostile["title"] = json!("safe\u{1b}[2J\u{202e}\nfake line");
    let _profile = world.profile(INSTANCE_A, obliging(vec![hostile]));

    let output = world.command().args(["list"]).output().expect("runs");

    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains('\u{202e}'));
    assert!(
        !text.lines().any(|line| line.starts_with("fake line")),
        "{text}"
    );
}

// Discarding

fn discard_requests_after(world: &World, tabs: Vec<Value>, args: &[&str]) -> Vec<Value> {
    let profile = world.profile(INSTANCE_A, obliging(tabs));
    let mut full = vec!["discard", "--policy-dir"];
    let policy = world.policy_dir();
    full.push(policy.to_str().expect("utf-8"));
    full.extend_from_slice(args);
    world.run(&full);
    profile.requests_for("discard")
}

fn with(mut tab: Value, field: &str, value: Value) -> Value {
    tab[field] = value;
    tab
}

fn discarded_ids(requests: &[Value]) -> Vec<i64> {
    requests
        .iter()
        .flat_map(|request| request["targets"].as_array().expect("targets"))
        .map(|target| target["id"].as_i64().expect("an id"))
        .collect()
}

#[test]
fn idle_selection_skips_every_protected_tab() {
    let cases = [
        ("active", with(tab(2, URL), "active", json!(true))),
        ("pinned", with(tab(2, URL), "pinned", json!(true))),
        ("audible", with(tab(2, URL), "audible", json!(true))),
        ("discarded", with(tab(2, URL), "discarded", json!(true))),
        // A restart resets this, which can only make a tab look recently used.
        ("recent", with(tab(2, URL), "lastAccessed", json!(9.0e15))),
        (
            "unknown age",
            with(tab(2, URL), "lastAccessed", Value::Null),
        ),
    ];
    for (name, protected) in cases {
        let world = World::new();
        let requests =
            discard_requests_after(&world, vec![tab(1, URL), protected], &["--idle", "2h"]);
        assert_eq!(discarded_ids(&requests), [1], "{name}");
    }
}

#[test]
fn a_named_protection_is_overridden_and_passed_to_the_extension() {
    let world = World::new();
    let pinned = with(tab(1, URL), "pinned", json!(true));

    let requests =
        discard_requests_after(&world, vec![pinned], &["--idle", "2h", "--allow", "pinned"]);

    assert_eq!(discarded_ids(&requests), [1]);
    assert_eq!(requests[0]["allow"], json!(["pinned"]));
}

#[test]
fn a_host_in_the_exceptions_policy_is_kept_even_with_every_protection_allowed() {
    let world = World::new();
    world.set_policy(&json!({"TabDiscardingExceptions": ["meet.example.com"]}));
    let tabs = vec![tab(1, URL), tab(2, "https://meet.example.com/abc")];

    let requests = discard_requests_after(
        &world,
        tabs,
        &["--idle", "2h", "--allow", "active,pinned,audible"],
    );

    assert_eq!(discarded_ids(&requests), [1]);
}

#[test]
fn a_named_tab_in_the_exceptions_policy_is_refused_too() {
    let world = World::new();
    world.set_policy(&json!({"TabDiscardingExceptions": ["example.com"]}));
    let profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let reference = refs_of(&world).remove(0);
    let policy = world.policy_dir();

    let output = world.run(&[
        "discard",
        &reference,
        "--policy-dir",
        policy.to_str().expect("utf-8"),
    ]);

    assert_eq!(exit_code(&output), PARTIAL);
    assert_eq!(
        answer_json(&output)["results"][0]["reason"],
        "policy_exempt"
    );
    assert_eq!(profile.requests_for("discard"), Vec::<Value>::new());
}

#[test]
fn without_a_readable_policy_discard_asks_the_browser_nothing_at_all() {
    let world = World::new();
    fs::remove_file(world.policy_dir().join("policy.json")).expect("removed");
    let profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let policy = world.policy_dir();

    let output = world.run(&[
        "discard",
        "--idle",
        "2h",
        "--policy-dir",
        policy.to_str().expect("utf-8"),
    ]);

    assert_eq!(exit_code(&output), ERROR);
    assert_eq!(profile.ops(), Vec::<String>::new());
}

#[test]
fn a_policy_that_does_not_parse_refuses_the_same_way() {
    let world = World::new();
    fs::write(world.policy_dir().join("policy.json"), "{").expect("damaged");
    let profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let policy = world.policy_dir();

    let output = world.run(&[
        "discard",
        "--idle",
        "2h",
        "--policy-dir",
        policy.to_str().expect("utf-8"),
    ]);

    assert_eq!(exit_code(&output), ERROR);
    assert_eq!(profile.ops(), Vec::<String>::new());
}

#[test]
fn a_dry_run_changes_nothing_and_says_what_it_would_do() {
    let world = World::new();
    let profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let policy = world.policy_dir();

    let output = world.run(&[
        "discard",
        "--idle",
        "2h",
        "--dry-run",
        "--policy-dir",
        policy.to_str().expect("utf-8"),
    ]);

    assert_eq!(exit_code(&output), 0);
    assert_eq!(answer_json(&output)["would_unload"][0]["url"], URL);
    assert_eq!(profile.ops(), ["list"]);
}

#[test]
fn discard_without_a_selection_is_refused() {
    let world = World::new();
    let policy = world.policy_dir();

    let output = world.run(&["discard", "--policy-dir", policy.to_str().expect("utf-8")]);

    assert_eq!(exit_code(&output), ERROR);
}

// Closing

#[test]
fn every_record_is_on_disk_before_any_close_is_sent() {
    let world = World::new();
    let saved = world.saved_dir();
    let at_close: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&at_close);
    let tabs = vec![tab(1, URL)];
    let _profile = world.profile(
        INSTANCE_A,
        Box::new(move |request| {
            if request["op"] == "close" {
                for entry in fs::read_dir(&saved).expect("the record's directory exists") {
                    let text = fs::read_to_string(entry.expect("entry").path()).expect("readable");
                    seen.lock().expect("not poisoned").push(text);
                }
            }
            answer_obligingly(request, &tabs)
        }),
    );
    let reference = refs_of(&world).remove(0);

    let output = world.run(&["close", &reference]);

    assert_eq!(exit_code(&output), 0);
    let records = at_close.lock().expect("not poisoned");
    assert_eq!(records.len(), 1);
    let record: Value = serde_json::from_str(&records[0]).expect("a whole record");
    assert_eq!(record["tabs"][0]["url"], URL);
}

#[test]
fn no_tab_closes_when_the_record_cannot_be_written() {
    let world = World::new();
    let profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let reference = refs_of(&world).remove(0);
    fs::create_dir_all(world.state_home()).expect("state home");
    make_read_only(&world.state_home());

    let output = world.run(&["close", &reference]);

    assert_eq!(exit_code(&output), ERROR);
    assert_eq!(profile.requests_for("close"), Vec::<Value>::new());
}

#[test]
fn a_url_that_changed_is_refused_and_the_others_still_close() {
    let world = World::new();
    let first = world.profile(INSTANCE_A, obliging(vec![tab(1, URL), tab(2, URL)]));
    let refs = refs_of(&world);
    drop(first);
    fs::remove_file(world.socket_dir().join(format!("{INSTANCE_A}.sock"))).expect("removed");
    let second = world.profile(
        INSTANCE_A,
        obliging(vec![tab(1, "https://example.com/elsewhere"), tab(2, URL)]),
    );

    let output = world.run(&["close", &refs[0], &refs[1]]);

    assert_eq!(exit_code(&output), PARTIAL);
    let answer = answer_json(&output);
    let results = answer["results"].as_array().expect("results");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["reason"], "url_changed");
    assert_eq!(results[1]["status"], "done");
    let closes = second.requests_for("close");
    assert_eq!(discarded_ids(&closes), [2]);
}

#[test]
fn a_tab_that_is_gone_or_a_profile_not_connected_is_its_own_result() {
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let reference = refs_of(&world).remove(0);
    let gone = reference.replacen(":1:", ":99:", 1);
    let elsewhere = format!("nobody{}", &reference[8..]);

    let output = world.run(&["close", &gone, &elsewhere]);

    assert_eq!(exit_code(&output), PARTIAL);
    let answer = answer_json(&output);
    assert_eq!(answer["results"][0]["reason"], "gone");
    assert_eq!(answer["results"][1]["reason"], "profile_not_connected");
}

#[test]
fn a_tab_the_extension_refused_leaves_the_record_since_it_is_still_open() {
    let world = World::new();
    let tabs = vec![tab(1, URL), tab(2, URL)];
    let _profile = world.profile(
        INSTANCE_A,
        Box::new(move |request| {
            if request["op"] == "close" {
                return json!({"id": request["id"], "ok": true, "result": {"results": [
                    {"id": 1, "status": "refused", "reason": "active"},
                    {"id": 2, "status": "done"},
                ]}});
            }
            answer_obligingly(request, &tabs)
        }),
    );
    let refs = refs_of(&world);

    let output = world.run(&["close", &refs[0], &refs[1]]);

    assert_eq!(exit_code(&output), PARTIAL);
    let id = answer_json(&output)["records"][0]
        .as_str()
        .expect("a record")
        .to_owned();
    let record: Value = serde_json::from_str(
        &fs::read_to_string(world.saved_dir().join(format!("{id}.json"))).expect("the record"),
    )
    .expect("json");
    assert_eq!(record["tabs"].as_array().expect("tabs").len(), 1);
    assert_eq!(record["tabs"][0]["index"], 2);
}

#[test]
fn a_close_nothing_came_of_leaves_no_record() {
    let world = World::new();
    let tabs = vec![tab(1, URL)];
    let _profile = world.profile(
        INSTANCE_A,
        Box::new(move |request| {
            if request["op"] == "close" {
                return json!({"id": request["id"], "ok": true, "result": {"results": [
                    {"id": 1, "status": "refused", "reason": "active"},
                ]}});
            }
            answer_obligingly(request, &tabs)
        }),
    );
    let reference = refs_of(&world).remove(0);

    let output = world.run(&["close", &reference]);

    assert_eq!(exit_code(&output), PARTIAL);
    assert_eq!(saved_files(&world), Vec::<String>::new());
}

#[test]
fn a_close_whose_answer_was_lost_keeps_the_whole_record() {
    let world = World::new();
    let tabs = vec![tab(1, URL)];
    let _profile = world.profile(
        INSTANCE_A,
        Box::new(move |request| {
            if request["op"] == "close" {
                return Value::Null;
            }
            answer_obligingly(request, &tabs)
        }),
    );
    let reference = refs_of(&world).remove(0);

    let output = world.run(&["close", &reference]);

    assert_eq!(exit_code(&output), PARTIAL);
    assert_eq!(answer_json(&output)["results"][0]["status"], "failed");
    assert_eq!(saved_files(&world).len(), 1);
}

#[test]
fn a_profile_that_would_not_answer_the_list_fails_the_close_visibly() {
    let world = World::new();
    let _healthy = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let reference = refs_of(&world).remove(0);
    let _silent = world.profile(INSTANCE_B, Box::new(|_| Value::Null));

    let output = world.run(&["close", &reference]);

    assert_eq!(exit_code(&output), PARTIAL);
    assert_eq!(answer_json(&output)["partial"], true);
}

// Restoring

fn closed_record(world: &World) -> String {
    let reference = refs_of(world).remove(0);
    let output = world.run(&["close", &reference]);
    assert_eq!(exit_code(&output), 0);
    answer_json(&output)["records"][0]
        .as_str()
        .expect("a record")
        .to_owned()
}

fn opening_handler(tabs: Vec<Value>) -> support::Handler {
    Box::new(move |request| {
        if request["op"] == "restore" {
            return json!({"id": request["id"], "ok": true, "result": {
                "results": [{"position": 0, "status": "done", "id": 7}],
                "groupProblems": [],
            }});
        }
        answer_obligingly(request, &tabs)
    })
}

#[test]
fn a_restore_goes_to_the_records_own_profile_with_everything_saved() {
    let world = World::new();
    let mut original = tab(1, URL);
    original["pinned"] = json!(true);
    let profile = world.profile(INSTANCE_A, opening_handler(vec![original]));
    let other = world.profile(INSTANCE_B, opening_handler(vec![]));
    let record = closed_record(&world);

    let output = world.run(&["restore", &record]);

    assert_eq!(exit_code(&output), 0);
    let requests = profile.requests_for("restore");
    assert_eq!(requests[0]["tabs"][0]["url"], URL);
    assert_eq!(requests[0]["tabs"][0]["pinned"], true);
    assert_eq!(requests[0]["tabs"][0]["index"], 1);
    assert_eq!(other.requests_for("restore"), Vec::<Value>::new());
}

#[test]
fn a_missing_profile_is_reported_and_nothing_opens_elsewhere() {
    let world = World::new();
    let first = world.profile(INSTANCE_A, opening_handler(vec![tab(1, URL)]));
    let record = closed_record(&world);
    drop(first);
    fs::remove_file(world.socket_dir().join(format!("{INSTANCE_A}.sock"))).expect("removed");
    let other = world.profile(INSTANCE_B, opening_handler(vec![]));

    let output = world.run(&["restore", &record]);

    assert_eq!(exit_code(&output), ERROR);
    assert!(String::from_utf8_lossy(&output.stderr).contains("not connected"));
    assert_eq!(other.requests_for("restore"), Vec::<Value>::new());
}

#[test]
fn restoring_twice_needs_saying_so() {
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, opening_handler(vec![tab(1, URL)]));
    let record = closed_record(&world);
    assert_eq!(exit_code(&world.run(&["restore", &record])), 0);

    assert_eq!(exit_code(&world.run(&["restore", &record])), ERROR);
    assert_eq!(exit_code(&world.run(&["restore", &record, "--again"])), 0);
}

#[test]
fn a_tab_that_could_not_be_opened_is_named_in_the_result() {
    let world = World::new();
    let tabs = vec![tab(1, URL)];
    let _profile = world.profile(
        INSTANCE_A,
        Box::new(move |request| {
            if request["op"] == "restore" {
                return json!({"id": request["id"], "ok": true, "result": {
                    "results": [{"position": 0, "status": "refused", "reason": "not_openable"}],
                    "groupProblems": [],
                }});
            }
            answer_obligingly(request, &tabs)
        }),
    );
    let record = closed_record(&world);

    let output = world.run(&["restore", &record]);

    assert_eq!(exit_code(&output), PARTIAL);
    let answer = answer_json(&output);
    assert_eq!(answer["results"][0]["reason"], "not_openable");
    assert_eq!(answer["results"][0]["url"], URL);
}

#[test]
fn saved_records_are_private_and_list_without_their_contents() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let record = closed_record(&world);

    let answer = answer_json(&world.run(&["saved"]));

    assert_eq!(answer["records"][0]["id"], record);
    assert_eq!(answer["records"][0]["tabs"], 1);
    assert!(!answer.to_string().contains(URL));
    let mode = |path| fs::metadata(path).expect("exists").permissions().mode() & 0o777;
    assert_eq!(mode(world.saved_dir()), 0o700);
    assert_eq!(
        mode(world.saved_dir().join(format!("{record}.json"))),
        0o600
    );
}

// Arranging

#[test]
fn a_move_reports_where_the_tab_really_ended_up() {
    let world = World::new();
    let tabs = vec![tab(1, URL)];
    let profile = world.profile(
        INSTANCE_A,
        Box::new(move |request| {
            if request["op"] == "move" {
                return json!({"id": request["id"], "ok": true, "result": {"results": [
                    {"id": 1, "status": "done", "windowId": 1, "index": 2, "requestedIndex": 0},
                ]}});
            }
            answer_obligingly(request, &tabs)
        }),
    );
    let reference = refs_of(&world).remove(0);

    let output = world.run(&["move", &reference, "--index", "0"]);

    let answer = answer_json(&output);
    assert_eq!(answer["results"][0]["index"], 2);
    assert_eq!(profile.requests_for("move")[0]["index"], 0);
}

#[test]
fn a_move_to_the_end_accepts_minus_one() {
    let world = World::new();
    let profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let reference = refs_of(&world).remove(0);

    let output = world.run(&["move", &reference, "--index", "-1", "--window", "4"]);

    assert_eq!(exit_code(&output), 0);
    let request = &profile.requests_for("move")[0];
    assert_eq!(request["index"], -1);
    assert_eq!(request["windowId"], 4);
}

#[test]
fn tabs_from_two_profiles_cannot_share_an_arrangement() {
    let world = World::new();
    let first = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let second = world.profile(INSTANCE_B, obliging(vec![tab(2, URL)]));
    let refs = refs_of(&world);

    let output = world.run(&["group", &refs[0], &refs[1], "--title", "work"]);

    assert_eq!(exit_code(&output), PARTIAL);
    let answer = answer_json(&output);
    let reasons: Vec<&Value> = answer["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| &r["reason"])
        .collect();
    assert!(reasons.contains(&&json!("other_profile")), "{reasons:?}");
    assert_eq!(
        first.requests_for("group").len() + second.requests_for("group").len(),
        1
    );
}

#[test]
fn a_group_extends_one_of_the_same_title() {
    let world = World::new();
    let tabs = vec![tab(1, URL)];
    let profile = world.profile(
        INSTANCE_A,
        Box::new(move |request| {
            if request["op"] == "list" {
                return json!({"id": request["id"], "ok": true, "result": {
                    "extensionId": "niglgkgdkfompjmcbpldgepicfnjlfnc",
                    "tabs": tabs,
                    "groups": [{"id": 77, "windowId": 1, "title": "work", "color": "red", "collapsed": false}],
                }});
            }
            answer_obligingly(request, &[])
        }),
    );
    let reference = refs_of(&world).remove(0);

    world.run(&["group", &reference, "--title", "work"]);
    world.run(&["group", &reference, "--title", "fun"]);

    let requests = profile.requests_for("group");
    assert_eq!(requests[0]["groupId"], 77);
    assert_eq!(requests[1]["groupId"], Value::Null);
}

#[test]
fn an_unknown_colour_is_refused_before_anything_is_asked() {
    let world = World::new();
    let profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let reference = refs_of(&world).remove(0);
    let before = profile.ops().len();

    let output = world.run(&["group", &reference, "--color", "mauve"]);

    assert_eq!(exit_code(&output), ERROR);
    assert_eq!(profile.ops().len(), before);
}

#[test]
fn a_reference_that_is_not_one_is_refused_before_anything_is_asked() {
    let world = World::new();
    let profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));

    let output = world.run(&["close", "not-a-reference"]);

    assert_eq!(exit_code(&output), ERROR);
    assert_eq!(profile.ops(), Vec::<String>::new());
}

// Bookmarks

fn bookmark_handler(requests_fail_removal: bool) -> support::Handler {
    Box::new(move |request| {
        let result = match request["op"].as_str() {
            Some("list") => support::snapshot(&[]),
            Some("bookmarks_list") if request["nodeId"].is_null() => json!({"tree": [{
                "id": "0",
                "title": "",
                "children": [{"id": "1", "title": "Bar", "children": [
                    {"id": "5", "title": "a page", "url": URL, "parentId": "1", "index": 3},
                ]}],
            }]}),
            Some("bookmarks_list") => json!({"tree": [
                {"id": "5", "title": "a page", "url": URL, "parentId": "1", "index": 3},
            ]}),
            Some("bookmarks_remove") if requests_fail_removal => {
                return json!({"id": request["id"], "ok": false, "error": "changed"});
            }
            Some("bookmarks_remove") => json!({"removed": "5"}),
            Some("bookmarks_restore") => json!({"bookmarkId": "9"}),
            Some("bookmarks_add") => {
                json!({"folderId": null, "results": [{"status": "done", "bookmarkId": "6"}]})
            }
            _ => json!({}),
        };
        json!({"id": request["id"], "ok": true, "result": result})
    })
}

#[test]
fn bookmarks_list_in_pages_without_chromes_invisible_root() {
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, bookmark_handler(false));

    let answer = answer_json(&world.run(&["bookmark", "list"]));

    assert_eq!(answer["total"], 2);
    assert_eq!(answer["bookmarks"][0]["title"], "Bar");
    assert_eq!(answer["bookmarks"][0]["folder"], true);
    assert_eq!(answer["bookmarks"][1]["url"], URL);
}

#[test]
fn a_removed_bookmark_was_saved_first_and_comes_back_where_it_was() {
    let world = World::new();
    let saved = world.saved_dir();
    let profile = world.profile(INSTANCE_A, bookmark_handler(false));

    let output = world.run(&["bookmark", "remove", "5"]);

    assert_eq!(exit_code(&output), 0);
    let record = answer_json(&output)["record"]
        .as_str()
        .expect("a record")
        .to_owned();
    assert!(saved.join(format!("{record}.json")).exists());
    assert_eq!(
        profile.requests_for("bookmarks_remove")[0]["expectedTitle"],
        "a page"
    );

    assert_eq!(exit_code(&world.run(&["bookmark", "restore", &record])), 0);
    let restore = &profile.requests_for("bookmarks_restore")[0];
    assert_eq!(restore["node"]["url"], URL);
    assert_eq!(restore["parentId"], "1");
    assert_eq!(restore["index"], 3);
}

#[test]
fn a_removal_that_failed_leaves_nothing_to_undo() {
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, bookmark_handler(true));

    let output = world.run(&["bookmark", "remove", "5"]);

    assert_eq!(exit_code(&output), ERROR);
    assert_eq!(saved_files(&world), Vec::<String>::new());
}

#[test]
fn a_bookmark_is_not_saved_when_the_record_cannot_be_and_nothing_is_removed() {
    let world = World::new();
    let profile = world.profile(INSTANCE_A, bookmark_handler(false));
    fs::create_dir_all(world.state_home()).expect("state home");
    make_read_only(&world.state_home());

    let output = world.run(&["bookmark", "remove", "5"]);

    assert_eq!(exit_code(&output), ERROR);
    assert_eq!(
        profile.requests_for("bookmarks_remove"),
        Vec::<Value>::new()
    );
}

#[test]
fn a_bookmark_record_cannot_be_restored_as_tabs() {
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, bookmark_handler(false));
    let output = world.run(&["bookmark", "remove", "5"]);
    let record = answer_json(&output)["record"]
        .as_str()
        .expect("a record")
        .to_owned();

    assert_eq!(exit_code(&world.run(&["restore", &record])), ERROR);
}

// Saved groups

fn grouped_browser() -> support::Handler {
    let mut first = tab(1, URL);
    first["groupId"] = json!(77);
    let mut second = tab(2, "https://example.org/");
    second["groupId"] = json!(77);
    let tabs = vec![first, second, tab(3, URL)];
    Box::new(move |request| {
        let result = match request["op"].as_str() {
            Some("list") => json!({
                "extensionId": "niglgkgdkfompjmcbpldgepicfnjlfnc",
                "tabs": tabs,
                "groups": [{"id": 77, "windowId": 1, "title": "work", "color": "red", "collapsed": true}],
            }),
            Some("restore") => json!({
                "results": [
                    {"position": 0, "status": "done", "id": 8},
                    {"position": 1, "status": "done", "id": 9},
                ],
                "groupProblems": [],
            }),
            _ => return answer_obligingly(request, &[]),
        };
        json!({"id": request["id"], "ok": true, "result": result})
    })
}

#[test]
fn a_saved_group_closes_only_its_tabs_and_opens_with_its_title_colour_and_order() {
    let world = World::new();
    let profile = world.profile(INSTANCE_A, grouped_browser());

    let saved = world.run(&["group", "save", "focus", "--group", "work"]);
    assert_eq!(
        exit_code(&saved),
        0,
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    assert_eq!(discarded_ids(&profile.requests_for("close")), [1, 2]);

    let listed = answer_json(&world.run(&["group", "list"]));
    assert_eq!(listed["groups"][0]["name"], "focus");
    assert_eq!(listed["groups"][0]["tabs"], 2);

    assert_eq!(exit_code(&world.run(&["group", "open", "focus"])), 0);
    let request = &profile.requests_for("restore")[0];
    assert_eq!(request["groups"][0]["title"], "work");
    assert_eq!(request["groups"][0]["color"], "red");
    assert_eq!(request["groups"][0]["collapsed"], true);
    assert_eq!(request["tabs"][0]["index"], 1);
    assert_eq!(request["tabs"][1]["index"], 2);
    assert_eq!(request["tabs"][0]["group"], request["tabs"][1]["group"]);
}

#[test]
fn a_saved_group_opens_as_often_as_asked_until_it_is_deleted() {
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, grouped_browser());
    world.run(&["group", "save", "focus", "--group", "work"]);

    assert_eq!(exit_code(&world.run(&["group", "open", "focus"])), 0);
    assert_eq!(exit_code(&world.run(&["group", "open", "focus"])), 0);
    assert_eq!(exit_code(&world.run(&["group", "delete", "focus"])), 0);

    assert_eq!(exit_code(&world.run(&["group", "open", "focus"])), ERROR);
}

#[test]
fn a_name_already_saved_is_refused_rather_than_replaced() {
    let world = World::new();
    let profile = world.profile(INSTANCE_A, grouped_browser());
    world.run(&["group", "save", "focus", "--group", "work"]);
    let closes = profile.requests_for("close").len();

    let again = world.run(&["group", "save", "focus", "--group", "work"]);

    assert_eq!(exit_code(&again), ERROR);
    assert_eq!(profile.requests_for("close").len(), closes);
}

#[test]
fn a_group_that_does_not_exist_closes_nothing() {
    let world = World::new();
    let profile = world.profile(INSTANCE_A, grouped_browser());

    let output = world.run(&["group", "save", "focus", "--group", "nope"]);

    assert_eq!(exit_code(&output), ERROR);
    assert_eq!(profile.requests_for("close"), Vec::<Value>::new());
}

// What mutation testing found unguarded

fn hours_ago(hours: f64) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs_f64()
        * 1000.0;
    json!(now - hours * 3_600_000.0)
}

#[test]
fn idle_is_measured_from_now_against_the_threshold() {
    let world = World::new();
    let tabs = vec![
        with(tab(1, URL), "lastAccessed", hours_ago(3.0)),
        with(tab(2, URL), "lastAccessed", hours_ago(1.0)),
    ];

    let requests = discard_requests_after(&world, tabs, &["--idle", "2h"]);

    assert_eq!(discarded_ids(&requests), [1]);
}

#[test]
fn a_dry_run_counts_why_each_tab_was_kept() {
    let world = World::new();
    let tabs = vec![
        with(tab(1, URL), "pinned", json!(true)),
        with(tab(2, URL), "pinned", json!(true)),
        with(tab(3, URL), "active", json!(true)),
    ];
    let _profile = world.profile(INSTANCE_A, obliging(tabs));
    let policy = world.policy_dir();

    let answer = answer_json(&world.run(&[
        "discard",
        "--idle",
        "1s",
        "--dry-run",
        "--policy-dir",
        policy.to_str().expect("utf-8"),
    ]));

    assert_eq!(answer["kept"], json!({"pinned": 2, "active": 1}));
}

#[test]
fn a_dry_run_is_partial_when_a_profile_could_not_be_asked() {
    let world = World::new();
    let _healthy = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let _silent = world.profile(INSTANCE_B, Box::new(|_| Value::Null));
    let policy = world.policy_dir();

    let output = world.run(&[
        "discard",
        "--idle",
        "1s",
        "--dry-run",
        "--policy-dir",
        policy.to_str().expect("utf-8"),
    ]);

    assert_eq!(exit_code(&output), PARTIAL);
    assert_eq!(answer_json(&output)["partial"], true);
}

#[test]
fn search_reads_titles_and_urls_and_idle_over_reads_the_clock() {
    let world = World::new();
    let tabs = vec![
        with(
            tab(1, "https://a.test/needle"),
            "lastAccessed",
            hours_ago(1.0),
        ),
        with(tab(2, "https://b.test/"), "title", json!("A NEEDLE here")),
        with(tab(3, "https://c.test/"), "lastAccessed", hours_ago(1.0)),
    ];
    let _profile = world.profile(INSTANCE_A, obliging(tabs));

    let found = answer_json(&world.run(&["list", "--search", "needle"]));
    assert_eq!(found["total"], 2);

    // Tab 2 still has the ancient default.
    let idle = answer_json(&world.run(&["list", "--idle-over", "2h"]));
    assert_eq!(idle["total"], 1);
    assert_eq!(idle["tabs"][0]["url"], "https://b.test/");
    assert!(idle["tabs"][0]["idle_ms"].as_u64().expect("an age") > 3_600_000);
}

#[test]
fn list_names_the_group_each_tab_is_in() {
    let world = World::new();
    let grouped = with(tab(1, URL), "groupId", json!(77));
    let _profile = world.profile(
        INSTANCE_A,
        Box::new(move |request| {
            json!({"id": request["id"], "ok": true, "result": {
                "extensionId": "niglgkgdkfompjmcbpldgepicfnjlfnc",
                "tabs": [grouped.clone(), tab(2, URL)],
                "groups": [{"id": 77, "windowId": 1, "title": "work", "color": "red", "collapsed": false}],
            }})
        }),
    );

    let answer = answer_json(&world.run(&["list"]));

    assert_eq!(answer["tabs"][0]["group"], "work");
    assert_eq!(answer["tabs"][1]["group"], Value::Null);
}

#[test]
fn the_text_output_says_what_a_person_needs() {
    let world = World::new();
    let tabs = vec![
        with(
            with(tab(1, URL), "pinned", json!(true)),
            "lastAccessed",
            hours_ago(2.5),
        ),
        with(
            with(tab(2, URL), "discarded", json!(true)),
            "lastAccessed",
            hours_ago(0.5),
        ),
        with(tab(3, URL), "lastAccessed", hours_ago(50.0)),
    ];
    let _profile = world.profile(INSTANCE_A, obliging(tabs));

    let output = world
        .command()
        .args(["list", "--limit", "2"])
        .output()
        .expect("runs");

    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("aaaaaaaa: 3 tabs, 1 unloaded"), "{text}");
    assert!(text.contains("-P--   2h"), "{text}");
    assert!(
        text.contains("---D  30m") || text.contains("---D  29m"),
        "{text}"
    );
    assert!(text.contains("title 1"), "{text}");
    assert!(
        text.contains("showing 1-2 of 3; next page: --offset 2"),
        "{text}"
    );
    let last = world
        .command()
        .args(["list", "--offset", "2"])
        .output()
        .expect("runs");
    assert!(String::from_utf8_lossy(&last.stdout).contains("2d"));
}

#[test]
fn a_person_is_told_which_profile_failed_and_what_did_not_happen() {
    let world = World::new();
    let _healthy = world.profile(INSTANCE_A, obliging(vec![tab(1, URL), tab(2, URL)]));
    let refs = refs_of(&world);
    let gone = refs[1].replacen(":2:", ":99:", 1);
    let _silent = world.profile(INSTANCE_B, Box::new(|_| Value::Null));

    let output = world
        .command()
        .args(["close", &refs[0], &gone])
        .output()
        .expect("runs");

    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("closed 1 of 2 tabs"), "{text}");
    assert!(text.contains("refused"), "{text}");
    assert!(text.contains("gone"), "{text}");
    assert!(text.contains("undo with: tab-keeper restore"), "{text}");
    assert!(text.contains("profile bbbbbbbb did not answer"), "{text}");
}

#[test]
fn a_leftover_socket_is_reported_once_and_then_removed() {
    let world = World::new();
    let path = world.socket_dir().join(format!("{INSTANCE_B}.sock"));
    drop(std::os::unix::net::UnixListener::bind(&path).expect("bound"));
    let _healthy = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));

    let output = world.run(&["list"]);

    assert_eq!(exit_code(&output), PARTIAL);
    assert!(
        answer_json(&output)["failed"][0]["error"]
            .as_str()
            .expect("an error")
            .contains("no longer running")
    );
    assert!(!path.exists());
    assert_eq!(exit_code(&world.run(&["list"])), 0);
}

#[test]
fn a_live_socket_that_hangs_up_is_kept_because_its_host_is_still_there() {
    let world = World::new();
    let _silent = world.profile(INSTANCE_A, Box::new(|_| Value::Null));

    world.run(&["list"]);

    assert!(
        world
            .socket_dir()
            .join(format!("{INSTANCE_A}.sock"))
            .exists()
    );
}

#[test]
fn a_socket_directory_others_can_write_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    fs::set_permissions(world.socket_dir(), fs::Permissions::from_mode(0o777)).expect("opened");

    let output = world.run(&["list"]);

    assert_eq!(exit_code(&output), ERROR);
}

#[test]
fn a_profile_is_named_by_its_directory_and_answers_to_its_label() {
    let world = World::new();
    let browser = world.root.path().join("browser");
    let settings = browser
        .join("Profile 3")
        .join("Local Extension Settings")
        .join("niglgkgdkfompjmcbpldgepicfnjlfnc");
    fs::create_dir_all(&settings).expect("settings");
    fs::write(
        settings.join("000003.log"),
        format!("\0instance\0{INSTANCE_A}\0"),
    )
    .expect("log");
    let sleeping = browser
        .join("Default")
        .join("Local Extension Settings")
        .join("niglgkgdkfompjmcbpldgepicfnjlfnc");
    fs::create_dir_all(sleeping).expect("settings");
    fs::write(
        browser.join("Local State"),
        json!({"profile": {"info_cache": {
            "Profile 3": {"name": "Work"},
            "Default": {"name": "Home"},
        }}})
        .to_string(),
    )
    .expect("local state");
    let _profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));

    let output = world
        .command()
        .env("TAB_KEEPER_BROWSER_DIRS", &browser)
        .args(["--json", "list", "--profile", "Work"])
        .output()
        .expect("runs");

    assert_eq!(exit_code(&output), 0);
    let answer = answer_json(&output);
    assert_eq!(answer["tabs"][0]["profile"], "Profile 3");
    assert_eq!(answer["profiles"][0]["label"], "Work");
    assert_eq!(answer["not_connected"], json!(["Home"]));
}

#[test]
fn asking_for_a_profile_nobody_knows_says_who_is_connected() {
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));

    let output = world.run(&["list", "--profile", "nobody"]);

    assert_eq!(exit_code(&output), ERROR);
    assert!(String::from_utf8_lossy(&output.stderr).contains("connected: aaaaaaaa"));
}

#[test]
fn a_new_bookmark_has_to_say_where_when_several_profiles_are_connected() {
    let world = World::new();
    let first = world.profile(INSTANCE_A, bookmark_handler(false));
    let _second = world.profile(INSTANCE_B, bookmark_handler(false));

    let output = world.run(&["bookmark", "add", "--title", "t", "--url", URL]);

    assert_eq!(exit_code(&output), ERROR);
    assert!(String::from_utf8_lossy(&output.stderr).contains("aaaaaaaa, bbbbbbbb"));
    assert_eq!(first.requests_for("bookmarks_add"), Vec::<Value>::new());
}

#[test]
fn a_bookmark_the_extension_would_not_make_is_not_success() {
    let world = World::new();
    let _profile = world.profile(
        INSTANCE_A,
        Box::new(|request| {
            let result = if request["op"] == "list" {
                support::snapshot(&[])
            } else {
                json!({"folderId": null, "results": [{"status": "refused", "reason": "not_openable"}]})
            };
            json!({"id": request["id"], "ok": true, "result": result})
        }),
    );

    let output = world.run(&[
        "bookmark",
        "add",
        "--title",
        "t",
        "--url",
        "javascript:alert(1)",
    ]);

    assert_eq!(exit_code(&output), PARTIAL);
}

#[test]
fn bookmarks_page_and_say_where_the_next_page_starts() {
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, bookmark_handler(false));

    let first = answer_json(&world.run(&["bookmark", "list", "--limit", "1"]));
    assert_eq!(first["bookmarks"].as_array().expect("page").len(), 1);
    assert_eq!(first["next_offset"], 1);

    let last = answer_json(&world.run(&["bookmark", "list", "--offset", "1"]));
    assert_eq!(last["bookmarks"][0]["title"], "a page");
    assert_eq!(last["next_offset"], Value::Null);
}

#[test]
fn saved_records_page_too() {
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    closed_record(&world);
    closed_record(&world);

    let first = answer_json(&world.run(&["saved", "--limit", "1"]));
    assert_eq!(first["records"].as_array().expect("page").len(), 1);
    assert_eq!(first["total"], 2);
    assert_eq!(first["next_offset"], 1);

    let last = answer_json(&world.run(&["saved", "--offset", "1"]));
    assert_eq!(last["next_offset"], Value::Null);
}

#[test]
fn a_group_recreated_wrongly_is_reported_even_though_the_tabs_opened() {
    let world = World::new();
    let tabs = vec![tab(1, URL)];
    let _profile = world.profile(
        INSTANCE_A,
        Box::new(move |request| {
            if request["op"] == "restore" {
                return json!({"id": request["id"], "ok": true, "result": {
                    "results": [{"position": 0, "status": "done", "id": 7}],
                    "groupProblems": [{"key": "1", "reason": "no"}],
                }});
            }
            answer_obligingly(request, &tabs)
        }),
    );
    let record = closed_record(&world);

    let output = world.run(&["restore", &record]);

    assert_eq!(exit_code(&output), PARTIAL);
    assert_eq!(answer_json(&output)["group_problems"][0]["reason"], "no");
    // The tab did open, so a second restore is still something to ask for.
    assert_eq!(exit_code(&world.run(&["restore", &record])), ERROR);
}

#[test]
fn a_restore_that_opened_nothing_can_simply_be_tried_again() {
    let world = World::new();
    let tabs = vec![tab(1, URL)];
    let _profile = world.profile(
        INSTANCE_A,
        Box::new(move |request| {
            if request["op"] == "restore" {
                return json!({"id": request["id"], "ok": true, "result": {
                    "results": [{"position": 0, "status": "failed", "reason": "no"}],
                    "groupProblems": [],
                }});
            }
            answer_obligingly(request, &tabs)
        }),
    );
    let record = closed_record(&world);

    assert_eq!(exit_code(&world.run(&["restore", &record])), PARTIAL);
    assert_eq!(exit_code(&world.run(&["restore", &record])), PARTIAL);
}

#[test]
fn only_a_window_left_with_nothing_is_reported_closed() {
    let world = World::new();
    let in_window = |id, window| {
        let mut one = tab(id, URL);
        one["windowId"] = json!(window);
        one
    };
    let tabs = vec![
        in_window(1, 10),
        in_window(2, 20),
        in_window(3, 30),
        in_window(4, 30),
    ];
    let _profile = world.profile(
        INSTANCE_A,
        Box::new(move |request| {
            if request["op"] == "group" {
                return json!({"id": request["id"], "ok": true, "result": {"groupId": 5, "results": [
                    {"id": 1, "status": "done", "fromWindowId": 10, "windowId": 10},
                    {"id": 2, "status": "done", "fromWindowId": 20, "windowId": 10},
                    {"id": 3, "status": "done", "fromWindowId": 30, "windowId": 10},
                ]}});
            }
            answer_obligingly(request, &tabs)
        }),
    );
    let refs = refs_of(&world);

    let answer = answer_json(&world.run(&["group", &refs[0], &refs[1], &refs[2]]));

    assert_eq!(answer["closed_windows"], json!([20]));
    assert_eq!(answer["group_id"], 5);
}

#[test]
fn a_name_is_found_only_among_saved_groups() {
    let world = World::new();
    let _profile = world.profile(INSTANCE_A, bookmark_handler(false));
    // A removed bookmark's record carries its title as a name.
    world.run(&["bookmark", "remove", "5"]);

    assert_eq!(exit_code(&world.run(&["group", "open", "a page"])), ERROR);
    assert_eq!(
        answer_json(&world.run(&["group", "list"]))["groups"],
        json!([])
    );
}
