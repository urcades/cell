use assert_cmd::cargo::cargo_bin_cmd;

fn read_fixture(path: &str) -> String {
    std::fs::read_to_string(format!("../../fixtures/cli/{path}")).expect("fixture")
}

fn with_trailing_newline(content: String) -> String {
    if content.ends_with('\n') {
        content
    } else {
        format!("{content}\n")
    }
}

#[test]
fn help_matches_fixture() {
    let fixture = read_fixture("help.txt");
    let mut command = cargo_bin_cmd!("cell");
    command
        .arg("--help")
        .assert()
        .success()
        .stdout(with_trailing_newline(fixture));
}

#[test]
fn version_matches_fixture() {
    let fixture = read_fixture("version.txt");
    let mut command = cargo_bin_cmd!("cell");
    command
        .arg("--version")
        .assert()
        .success()
        .stdout(format!("{}\n", fixture.trim_end()));
}

#[test]
fn extension_execution_is_explicitly_unsupported() {
    let fixture = read_fixture("extension-unsupported-stderr.txt");
    let mut command = cargo_bin_cmd!("cell");
    command
        .args(["--extension", "./example.ts"])
        .assert()
        .failure()
        .stderr(with_trailing_newline(fixture));
}

#[test]
fn package_help_is_available_before_package_execution_exists() {
    let fixture = read_fixture("install-help.txt");
    let mut command = cargo_bin_cmd!("cell");
    command
        .args(["install", "--help"])
        .assert()
        .success()
        .stdout(with_trailing_newline(fixture));
}

#[test]
fn invalid_package_options_fail_with_usage_hint() {
    let fixture = read_fixture("install-invalid-option-stderr.txt");
    let mut command = cargo_bin_cmd!("cell");
    command
        .args(["install", "--bogus"])
        .assert()
        .failure()
        .stderr(with_trailing_newline(fixture));
}
