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

/// Every tool reaches the operation it names, with its arguments.
#[test]
fn each_write_tool_sends_its_own_operation() {
    let world = World::new();
    let tabs = vec![tab(1, URL)];
    let handler = move |request: &Value| {
        let result = match request["op"].as_str() {
            Some("restore") => json!({
                "results": [{"position": 0, "status": "done", "id": 7}],
                "groupProblems": [],
            }),
            Some("bookmarks_list") => json!({"tree": [{"id": "5", "title": "t", "url": URL}]}),
            Some("bookmarks_add") => json!({"results": [{"status": "done", "bookmarkId": "6"}]}),
            Some("bookmarks_remove") => json!({"removed": "5"}),
            _ => return support::answer_obligingly(request, &tabs),
        };
        json!({"id": request["id"], "ok": true, "result": result})
    };
    let profile = world.profile(INSTANCE_A, Box::new(handler));
    let listed = converse(&world, &[call(1, "list_tabs", &json!({}))]);
    let reference = payload(&listed[0])["tabs"][0]["ref"].clone();
    let cases = [
        (
            "move_tabs",
            json!({"tabs": [reference], "index": 0, "window": 4}),
            "move",
        ),
        (
            "group_tabs",
            json!({"tabs": [reference], "title": "t", "color": "red"}),
            "group",
        ),
        ("ungroup_tabs", json!({"tabs": [reference]}), "ungroup"),
        (
            "discard_tabs",
            json!({"tabs": [reference], "allow": ["pinned"]}),
            "discard",
        ),
        ("list_bookmarks", json!({"limit": 1}), "bookmarks_list"),
        (
            "add_bookmark",
            json!({"title": "t", "url": URL}),
            "bookmarks_add",
        ),
        ("remove_bookmark", json!({"id": "5"}), "bookmarks_remove"),
    ];
    for (name, arguments, op) in cases {
        let answers = converse(&world, &[call(1, name, &arguments)]);
        assert_eq!(
            answers[0]["result"]["isError"], false,
            "{name}: {}",
            answers[0]
        );
        assert_eq!(profile.requests_for(op).len(), 1, "{name}");
    }
    assert_eq!(profile.requests_for("move")[0]["windowId"], 4);
    assert_eq!(profile.requests_for("group")[0]["color"], "red");
    assert_eq!(
        profile.requests_for("discard")[0]["allow"],
        json!(["pinned"])
    );
}

#[test]
fn reading_tools_answer_and_filters_arrive() {
    let world = World::new();
    let tabs = vec![tab(1, URL), tab(2, "https://other.test/")];
    let _profile = world.profile(INSTANCE_A, obliging(tabs));

    let answers = converse(
        &world,
        &[
            call(1, "list_profiles", &json!({})),
            call(2, "list_groups", &json!({})),
            call(3, "list_saved", &json!({})),
            call(4, "list_saved_groups", &json!({})),
            call(5, "list_tabs", &json!({"host": "other.test"})),
            call(6, "list_tabs", &json!({"search": "other"})),
            call(7, "list_tabs", &json!({"limit": 1, "offset": 1})),
            call(8, "list_tabs", &json!({"idle_over": "1s"})),
            call(9, "list_tabs", &json!({"idle_over": "soon"})),
            call(10, "list_tabs", &json!({"profile": "nobody"})),
        ],
    );

    assert_eq!(payload(&answers[0])["profiles"][0]["tabs"], 2);
    assert_eq!(payload(&answers[1])["groups"], json!([]));
    assert_eq!(payload(&answers[2])["records"], json!([]));
    assert_eq!(payload(&answers[3])["groups"], json!([]));
    assert_eq!(payload(&answers[4])["total"], 1);
    assert_eq!(payload(&answers[5])["total"], 1);
    assert_eq!(
        payload(&answers[6])["tabs"][0]["url"],
        "https://other.test/"
    );
    assert_eq!(payload(&answers[6])["limit"], 1);
    assert_eq!(payload(&answers[7])["total"], 2);
    assert_eq!(answers[8]["result"]["isError"], true);
    assert_eq!(answers[9]["result"]["isError"], true);
}

#[test]
fn a_saved_group_can_be_saved_listed_and_opened() {
    let world = World::new();
    let mut grouped = tab(1, URL);
    grouped["groupId"] = json!(77);
    let handler = move |request: &Value| {
        let result = match request["op"].as_str() {
            Some("list") => json!({
                "extensionId": "niglgkgdkfompjmcbpldgepicfnjlfnc",
                "tabs": [grouped.clone()],
                "groups": [{"id": 77, "windowId": 1, "title": "work", "color": "red", "collapsed": false}],
            }),
            Some("restore") => json!({
                "results": [{"position": 0, "status": "done", "id": 8}],
                "groupProblems": [],
            }),
            _ => return support::answer_obligingly(request, &[]),
        };
        json!({"id": request["id"], "ok": true, "result": result})
    };
    let profile = world.profile(INSTANCE_A, Box::new(handler));

    let answers = converse(
        &world,
        &[
            call(1, "save_group", &json!({"name": "later", "group": "work"})),
            call(2, "list_saved_groups", &json!({})),
            call(3, "open_saved_group", &json!({"name": "later"})),
            call(4, "save_group", &json!({"name": "later"})),
            call(5, "open_saved_group", &json!({})),
        ],
    );

    assert_eq!(answers[0]["result"]["isError"], false, "{}", answers[0]);
    assert_eq!(payload(&answers[1])["groups"][0]["name"], "later");
    assert_eq!(answers[2]["result"]["isError"], false, "{}", answers[2]);
    assert_eq!(profile.requests_for("restore").len(), 1);
    assert_eq!(answers[3]["result"]["isError"], true);
    assert_eq!(answers[4]["result"]["isError"], true);
}

#[test]
fn a_record_closed_through_the_server_comes_back_through_it() {
    let world = World::new();
    let tabs = vec![tab(1, URL)];
    let handler = move |request: &Value| {
        if request["op"] == "restore" {
            return json!({"id": request["id"], "ok": true, "result": {
                "results": [{"position": 0, "status": "done", "id": 7}],
                "groupProblems": [],
            }});
        }
        support::answer_obligingly(request, &tabs)
    };
    let profile = world.profile(INSTANCE_A, Box::new(handler));
    let listed = converse(&world, &[call(1, "list_tabs", &json!({}))]);
    let reference = payload(&listed[0])["tabs"][0]["ref"].clone();
    let closed = converse(
        &world,
        &[call(2, "close_tabs", &json!({"tabs": [reference]}))],
    );
    let record = payload(&closed[0])["records"][0].clone();

    let answers = converse(
        &world,
        &[
            call(3, "list_saved", &json!({"limit": 5})),
            call(4, "restore_saved", &json!({"record": record})),
            call(5, "restore_saved", &json!({"record": record})),
            call(
                6,
                "restore_saved",
                &json!({"record": record, "again": true}),
            ),
            call(7, "restore_saved", &json!({})),
        ],
    );

    assert_eq!(payload(&answers[0])["total"], 1);
    assert_eq!(answers[1]["result"]["isError"], false);
    assert_eq!(answers[2]["result"]["isError"], true);
    assert_eq!(answers[3]["result"]["isError"], false);
    assert_eq!(answers[4]["result"]["isError"], true);
    assert_eq!(profile.requests_for("restore").len(), 2);
}

/// A harness validates arguments against these, so an empty schema would mean no checks.
#[test]
fn every_schema_names_the_arguments_its_tool_reads() {
    let world = World::new();
    let answers = converse(&world, &[request(1, "tools/list", &json!({}))]);
    let tools = answers[0]["result"]["tools"].as_array().expect("tools");
    let expected: &[(&str, &[&str])] = &[
        ("list_profiles", &[]),
        (
            "list_tabs",
            &["profile", "limit", "offset", "host", "search", "idle_over"],
        ),
        ("list_groups", &["profile"]),
        ("list_saved", &["limit", "offset"]),
        (
            "discard_tabs",
            &["tabs", "idle", "profile", "dry_run", "allow"],
        ),
        ("move_tabs", &["tabs", "window", "index"]),
        ("group_tabs", &["tabs", "title", "color", "allow"]),
        ("ungroup_tabs", &["tabs"]),
        ("restore_saved", &["record", "again"]),
        ("close_tabs", &["tabs", "allow"]),
        ("list_bookmarks", &["profile", "limit", "offset"]),
        (
            "add_bookmark",
            &["tabs", "title", "url", "folder", "parent", "profile"],
        ),
        ("remove_bookmark", &["id", "profile"]),
        ("list_saved_groups", &[]),
        ("save_group", &["name", "group", "profile", "allow"]),
        ("open_saved_group", &["name"]),
    ];
    for (name, properties) in expected {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == *name)
            .expect("listed");
        let mut found: Vec<&str> = tool["inputSchema"]["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .map(String::as_str)
            .collect();
        found.sort_unstable();
        let mut wanted = properties.to_vec();
        wanted.sort_unstable();
        assert_eq!(found, wanted, "{name}");
    }
    let close = tools
        .iter()
        .find(|tool| tool["name"] == "close_tabs")
        .expect("listed");
    assert_eq!(close["inputSchema"]["properties"]["tabs"]["type"], "array");
    assert_eq!(
        close["inputSchema"]["properties"]["allow"]["items"]["enum"][0],
        "active"
    );
}

#[test]
fn a_blank_line_is_not_a_message_and_a_line_at_the_limit_is_not_too_long() {
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
        writeln!(stdin).expect("written");
        let ping = request(1, "ping", &json!({})).to_string();
        // A newline ends this exactly at the limit, padded with insignificant spaces.
        writeln!(stdin, "{ping}{}", " ".repeat((1 << 20) - ping.len() - 1)).expect("written");
    }
    let output = child.wait_with_output().expect("finished");
    let answers: Vec<Value> = String::from_utf8(output.stdout)
        .expect("utf-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON"))
        .collect();

    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["result"], json!({}));
}
