//! The command-line surface, against the real binary.

use std::process::Command;

#[test]
fn reports_its_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_tab-keeper"))
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
