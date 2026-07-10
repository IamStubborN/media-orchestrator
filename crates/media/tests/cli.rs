#[test]
fn binary_reports_its_version() {
    assert_cmd::cargo::cargo_bin_cmd!("media")
        .arg("--version")
        .assert()
        .success()
        .stdout("media 0.1.0\n");
}
