//! The MCP server, spoken to as a harness would.

mod support;

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use support::{INSTANCE_A, INSTANCE_B, World, obliging, tab};

const URL: &str = "https://example.com/page";

/// Each message gets one line; the answers come back in order.
fn converse(world: &World, messages: &[Value]) -> Vec<Value> {
    let mut command: Command = world.command();
    let mut child = command
        .args(["--timeout", "5", "mcp", "--policy-dir"])
        .arg(world.policy_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for message in messages {
            writeln!(stdin, "{message}").expect("written");
        }
    }
    let output = child.wait_with_output().expect("finished");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("utf-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("every line is JSON"))
        .collect()
}

fn request(id: i64, method: &str, params: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn call(id: i64, name: &str, arguments: &Value) -> Value {
    request(
        id,
        "tools/call",
        &json!({"name": name, "arguments": arguments}),
    )
}

fn payload(answer: &Value) -> Value {
    serde_json::from_str(
        answer["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("the text is JSON")
}

#[test]
fn the_handshake_agrees_a_version_and_a_notification_gets_no_answer() {
    let world = World::new();

    let answers = converse(
        &world,
        &[
            request(1, "initialize", &json!({"protocolVersion": "2025-06-18"})),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            request(2, "ping", &json!({})),
            request(3, "initialize", &json!({"protocolVersion": "1999-01-01"})),
        ],
    );

    assert_eq!(answers.len(), 3);
    assert_eq!(answers[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(answers[0]["result"]["capabilities"], json!({"tools": {}}));
    assert_eq!(answers[1]["id"], 2);
    assert_eq!(answers[2]["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn nonsense_gets_an_error_and_the_server_carries_on() {
    let world = World::new();
    let mut child = world
        .command()
        .args(["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("runs");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        writeln!(stdin, "this is not json").expect("written");
        writeln!(stdin, "{}", "x".repeat(2 << 20)).expect("written");
        writeln!(stdin, "{}", request(1, "no/such", &json!({}))).expect("written");
        writeln!(stdin, "{}", call(2, "no_such_tool", &json!({}))).expect("written");
        writeln!(stdin, "{}", request(3, "ping", &json!({}))).expect("written");
    }
    let output = child.wait_with_output().expect("finished");
    let answers: Vec<Value> = String::from_utf8(output.stdout)
        .expect("utf-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON"))
        .collect();

    assert_eq!(answers[0]["error"]["code"], -32700);
    assert_eq!(answers[1]["error"]["code"], -32600);
    assert_eq!(answers[2]["error"]["code"], -32601);
    assert_eq!(answers[3]["error"]["code"], -32602);
    assert_eq!(answers[4]["result"], json!({}));
}

/// The documented table in `docs/functionality.md`. A change here is a change to what a
/// harness's permission prompt relies on, so it must be deliberate.
#[test]
fn every_tools_annotations_match_the_documented_table() {
    const READ: (bool, bool) = (true, false);
    const WRITE: (bool, bool) = (false, false);
    const DESTRUCTIVE: (bool, bool) = (false, true);
    let documented = [
        ("list_profiles", READ),
        ("list_tabs", READ),
        ("list_groups", READ),
        ("list_saved", READ),
        ("discard_tabs", WRITE),
        ("move_tabs", WRITE),
        ("group_tabs", WRITE),
        ("ungroup_tabs", WRITE),
        ("restore_saved", WRITE),
        ("close_tabs", DESTRUCTIVE),
        ("list_bookmarks", READ),
        ("add_bookmark", WRITE),
        ("remove_bookmark", DESTRUCTIVE),
        ("list_saved_groups", READ),
        ("save_group", DESTRUCTIVE),
        ("open_saved_group", WRITE),
    ];
    let world = World::new();

    let answers = converse(&world, &[request(1, "tools/list", &json!({}))]);

    let tools = answers[0]["result"]["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), documented.len());
    for (name, (read_only, destructive)) in documented {
        let tool = tools.iter().find(|tool| tool["name"] == name).expect(name);
        assert_eq!(tool["annotations"]["readOnlyHint"], read_only, "{name}");
        assert_eq!(
            tool["annotations"]["destructiveHint"], destructive,
            "{name}"
        );
        let description = tool["description"].as_str().expect("description");
        assert!(description.contains("untrusted"), "{name}: {description}");
    }
}

#[test]
fn a_page_of_three_hundred_tabs_stays_small() {
    const MAX_PAGE_BYTES: usize = 32 * 1024;
    let world = World::new();
    let tabs = (0..300)
        .map(|id| {
            let mut long = tab(id, &format!("https://example.com/{}", "u".repeat(2000)));
            long["title"] = json!("t".repeat(2000));
            long
        })
        .collect();
    let _profile = world.profile(INSTANCE_A, obliging(tabs));

    let answers = converse(&world, &[call(1, "list_tabs", &json!({}))]);

    let text = answers[0]["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.len() < MAX_PAGE_BYTES, "{} bytes", text.len());
    assert_eq!(payload(&answers[0])["total"], 300);
}

#[test]
fn a_failed_tool_says_so_and_never_reads_as_an_empty_answer() {
    let world = World::new();

    let answers = converse(&world, &[call(1, "list_tabs", &json!({}))]);

    assert_eq!(answers[0]["result"]["isError"], true);
    assert!(
        answers[0]["result"]["content"][0]["text"]
            .as_str()
            .expect("text")
            .contains("no profile is connected")
    );
}

#[test]
fn a_partial_result_is_marked_as_an_error_and_keeps_what_it_has() {
    let world = World::new();
    let _healthy = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let _silent = world.profile(INSTANCE_B, Box::new(|_| Value::Null));

    let answers = converse(&world, &[call(1, "list_tabs", &json!({}))]);

    assert_eq!(answers[0]["result"]["isError"], true);
    assert_eq!(payload(&answers[0])["total"], 1);
    assert_eq!(payload(&answers[0])["partial"], true);
}

#[test]
fn a_close_through_the_server_saves_first_and_can_be_restored() {
    let world = World::new();
    let profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));
    let listed = converse(&world, &[call(1, "list_tabs", &json!({}))]);
    let reference = payload(&listed[0])["tabs"][0]["ref"].clone();

    let closed = converse(
        &world,
        &[call(2, "close_tabs", &json!({"tabs": [reference]}))],
    );

    assert_eq!(closed[0]["result"]["isError"], false);
    assert_eq!(payload(&closed[0])["results"][0]["status"], "done");
    assert_eq!(
        payload(&closed[0])["records"]
            .as_array()
            .expect("records")
            .len(),
        1
    );
    assert_eq!(profile.requests_for("close").len(), 1);
}

#[test]
fn no_tool_can_ask_for_unshortened_output() {
    let world = World::new();
    let mut long = tab(1, URL);
    long["title"] = json!("t".repeat(5000));
    let _profile = world.profile(INSTANCE_A, obliging(vec![long]));

    let answers = converse(&world, &[call(1, "list_tabs", &json!({"full": true}))]);

    assert_eq!(payload(&answers[0])["tabs"][0]["truncated"], true);
}

#[test]
fn without_a_readable_policy_the_discard_tool_touches_nothing() {
    let world = World::new();
    std::fs::remove_file(world.policy_dir().join("policy.json")).expect("removed");
    let profile = world.profile(INSTANCE_A, obliging(vec![tab(1, URL)]));

    let answers = converse(&world, &[call(1, "discard_tabs", &json!({"idle": "2h"}))]);

    assert_eq!(answers[0]["result"]["isError"], true);
    assert_eq!(profile.ops(), Vec::<String>::new());
}
