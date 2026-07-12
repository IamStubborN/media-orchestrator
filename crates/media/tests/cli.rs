#[test]
fn binary_reports_its_version() {
    assert_cmd::cargo::cargo_bin_cmd!("media")
        .arg("--version")
        .assert()
        .success()
        .stdout("media 0.1.0\n");
}

#[test]
fn help_keeps_existing_commands_and_exposes_runtime_commands() {
    let output = assert_cmd::cargo::cargo_bin_cmd!("media")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for command in [
        "jobs",
        "queue",
        "tracking",
        "healthcheck",
        "migrate",
        "serve",
    ] {
        assert!(stdout.contains(command), "help did not include {command}");
    }
}
