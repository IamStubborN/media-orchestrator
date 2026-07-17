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
        "release",
        "trending",
        "search",
        "download",
        "runner",
        "healthcheck",
        "migrate",
        "serve",
    ] {
        assert!(stdout.contains(command), "help did not include {command}");
    }
}

#[test]
fn user_commands_reject_identity_flags_at_parse_time() {
    for args in [
        vec!["search", "rezka", "Movie", "--requested-by", "other"],
        vec![
            "download",
            "--session",
            "s",
            "--result",
            "r",
            "--owner-id",
            "other",
        ],
    ] {
        let output = assert_cmd::cargo::cargo_bin_cmd!("media")
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
    }
}

#[test]
fn trending_rejects_zero_page_at_parse_time() {
    let output = assert_cmd::cargo::cargo_bin_cmd!("media")
        .args(["trending", "--page", "0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("page must be a positive integer"));
}
