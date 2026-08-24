//! Contract tests for the isolated Anubis browser helper.
//!
//! These tests drive a local fake executable. They do **not** launch
//! chrome-headless-shell or talk to real Anubis/Rezka. Live Chromium JS for
//! `preact` / `metarefresh` against a real challenge page is an operator smoke
//! test after deploy, not CI.

use std::{
    collections::HashMap,
    ffi::OsString,
    io::{self, Write as _},
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use media::anubis_browser::{BrowserFallbackSettings, ChromiumChallengeFallback};
use media::config::{ConfigError, ConfigSource, RunnerConfig};
use rezka_client::session::anubis::{AnubisChallenge, BrowserChallengeFallback};
use url::Url;

#[derive(Default)]
struct FakeSource {
    env: HashMap<&'static str, OsString>,
    files: HashMap<PathBuf, Vec<u8>>,
}

impl FakeSource {
    fn set_env(&mut self, name: &'static str, value: impl Into<OsString>) {
        self.env.insert(name, value.into());
    }

    fn set_secret(&mut self, name: &'static str, value: &[u8]) {
        let path = PathBuf::from(format!("/{name}.secret"));
        self.set_env(name, path.as_os_str());
        self.files.insert(path, value.to_vec());
    }

    fn valid_runner() -> Self {
        let mut source = Self::default();
        let encoded_key = STANDARD.encode([7_u8; 32]);
        source.set_env("MEDIA_SERVICE_URL", "https://media.internal.example");
        source.set_secret("MEDIA_TOKEN_FILE", b"runner-token");
        source.set_env("MEDIA_REZKA_MIRRORS", "https://rezka.test");
        source.set_env(
            "MEDIA_REZKA_SESSION_PROBE_URL",
            "https://rezka.test/account/probe",
        );
        source.set_env(
            "MEDIA_REZKA_SESSION_VALID_MARKERS_JSON",
            r#"["account-menu"]"#,
        );
        source.set_env(
            "MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON",
            r#"["login-form"]"#,
        );
        source.set_env(
            "MEDIA_REZKA_SESSION_STORE_FILE",
            "/runner/rezka/session.bin",
        );
        source.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", encoded_key.as_bytes());
        source
    }
}

impl ConfigSource for FakeSource {
    fn var_os(&self, name: &'static str) -> Option<OsString> {
        self.env.get(name).cloned()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing"))
    }
}

fn challenge() -> AnubisChallenge {
    AnubisChallenge {
        id: "challenge-secret".to_owned(),
        random_data: "random-secret".to_owned(),
        difficulty: 2,
    }
}

fn origin() -> Url {
    Url::parse("https://rezka.example/title").unwrap()
}

fn write_helper(script: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::Builder::new()
        .prefix("anubis-fake-browser-")
        .suffix(".sh")
        .tempfile()
        .unwrap();
    file.write_all(script.as_bytes()).unwrap();
    file.flush().unwrap();
    let mut permissions = file.as_file().metadata().unwrap().permissions();
    permissions.set_mode(0o755);
    file.as_file().set_permissions(permissions).unwrap();
    file
}

fn settings_for(helper: &Path, timeout: Duration) -> BrowserFallbackSettings {
    BrowserFallbackSettings::new(
        Some(helper.to_path_buf()),
        None,
        "media-orchestrator-test".to_owned(),
        None,
        timeout,
    )
}

fn process_exists(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn wait_for_pid(path: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Ok(pid) = contents.trim().parse::<i32>() {
                if pid > 1 {
                    return pid;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for pid file {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_pid_exited(pid: i32, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_exists(pid) {
        assert!(Instant::now() < deadline, "{message}: pid {pid}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[tokio::test]
async fn fake_helper_success_returns_only_set_cookie_headers() {
    let helper = write_helper(
        r#"#!/bin/sh
printf '%s\n' '{"set_cookie":["techaro.lol-anubis-auth=browser-clearance; Path=/; HttpOnly"]}'
"#,
    );
    let mut fallback =
        ChromiumChallengeFallback::new(settings_for(helper.path(), Duration::from_secs(2)));
    let cookies = fallback.solve(&challenge(), &origin()).await.unwrap();
    assert_eq!(
        cookies,
        vec!["techaro.lol-anubis-auth=browser-clearance; Path=/; HttpOnly".to_owned()]
    );
}

#[tokio::test]
async fn fake_helper_timeout_is_typed_and_kills_the_process_group() {
    let helper_pid_file = tempfile::NamedTempFile::new().unwrap();
    let child_pid_file = tempfile::NamedTempFile::new().unwrap();
    let helper = write_helper(&format!(
        "#!/bin/sh\n(sleep 30) &\necho $! > '{child}'\necho $$ > '{helper}'\nwait\n",
        child = child_pid_file.path().display(),
        helper = helper_pid_file.path().display(),
    ));
    let mut fallback =
        ChromiumChallengeFallback::new(settings_for(helper.path(), Duration::from_millis(200)));
    let started = Instant::now();
    let error = fallback.solve(&challenge(), &origin()).await.unwrap_err();
    assert_eq!(error.code(), rezka_client::RezkaErrorCode::AnubisTimeout);
    assert!(started.elapsed() < Duration::from_secs(3));
    let rendered = format!("{error:?}: {error}");
    assert!(!rendered.contains("challenge-secret"));
    assert!(!rendered.contains("browser-clearance"));
    let helper_pid = wait_for_pid(helper_pid_file.path());
    let child_pid = wait_for_pid(child_pid_file.path());
    assert_pid_exited(helper_pid, "browser helper was still running after timeout");
    assert_pid_exited(
        child_pid,
        "helper grandchild survived SIGKILL of the process group",
    );
}

#[tokio::test]
async fn fake_helper_process_failure_is_challenge_failed() {
    let helper = write_helper("#!/bin/sh\necho 'anubis-browser: process failed' >&2\nexit 2\n");
    let mut fallback =
        ChromiumChallengeFallback::new(settings_for(helper.path(), Duration::from_secs(2)));
    let error = fallback.solve(&challenge(), &origin()).await.unwrap_err();
    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ChallengeFailed);
}

#[tokio::test]
async fn fake_helper_without_clearance_is_rejected() {
    let helper = write_helper("#!/bin/sh\nprintf '%s\\n' '{\"set_cookie\":[]}'\n");
    let mut fallback =
        ChromiumChallengeFallback::new(settings_for(helper.path(), Duration::from_secs(2)));
    let error = fallback.solve(&challenge(), &origin()).await.unwrap_err();
    assert_eq!(error.code(), rezka_client::RezkaErrorCode::AnubisRejected);
}

#[tokio::test]
async fn fake_helper_rejected_exit_is_anubis_rejected() {
    let helper = write_helper("#!/bin/sh\necho 'anubis-browser: challenge remained' >&2\nexit 4\n");
    let mut fallback =
        ChromiumChallengeFallback::new(settings_for(helper.path(), Duration::from_secs(2)));
    let error = fallback.solve(&challenge(), &origin()).await.unwrap_err();
    assert_eq!(error.code(), rezka_client::RezkaErrorCode::AnubisRejected);
}

#[tokio::test]
async fn helper_html_and_cookie_values_do_not_leak_through_errors() {
    let helper = write_helper(
        r#"#!/bin/sh
echo '<script id="anubis_challenge">{"challenge":{"id":"leak-id"}}</script>' >&2
echo 'techaro.lol-anubis-auth=leak-cookie-value' >&2
echo 'eyJhbGciOiJIUzI1NiJ9.leak' >&2
printf '%s\n' 'not-json'
exit 2
"#,
    );
    let mut fallback =
        ChromiumChallengeFallback::new(settings_for(helper.path(), Duration::from_secs(2)));
    let error = fallback.solve(&challenge(), &origin()).await.unwrap_err();
    let rendered = format!("{error:?}: {error}");
    for secret in [
        "leak-id",
        "leak-cookie-value",
        "eyJhbGciOiJIUzI1NiJ9",
        "anubis_challenge",
        "<script",
        "not-json",
    ] {
        assert!(!rendered.contains(secret), "leaked {secret} via {rendered}");
    }
}

#[tokio::test]
async fn dropping_a_sleeping_helper_cancels_the_process() {
    let pid_file = tempfile::NamedTempFile::new().unwrap();
    let helper = write_helper(&format!(
        "#!/bin/sh\necho $$ > '{}'\nsleep 30\n",
        pid_file.path().display()
    ));
    let mut fallback =
        ChromiumChallengeFallback::new(settings_for(helper.path(), Duration::from_secs(30)));
    let challenge = challenge();
    let origin = origin();
    let started = Instant::now();
    let helper_pid;
    {
        let mut solve_fut = fallback.solve(&challenge, &origin);
        helper_pid = loop {
            tokio::select! {
                result = &mut solve_fut => {
                    panic!("helper exited before publishing pid: {result:?}");
                }
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    if let Ok(pid) = std::fs::read_to_string(pid_file.path()) {
                        if let Ok(pid) = pid.trim().parse::<i32>() {
                            if pid > 1 {
                                break pid;
                            }
                        }
                    }
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "helper never published pid"
            );
        };
        drop(solve_fut);
    }
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_pid_exited(helper_pid, "browser helper was still running after cancel");
}

#[test]
fn chrome_launch_keeps_a_single_process_tree() {
    let source = include_str!("../src/anubis_browser.rs");
    assert!(source.contains("--no-zygote"));
    assert!(source.contains("--single-process"));
    assert!(source.contains("anubis-exec-chromium"));
    assert!(source.contains("set_pdeathsig"));
}

#[cfg(target_os = "linux")]
#[test]
fn exec_wrapper_dies_when_its_parent_is_sigkilled() {
    let pid_file = tempfile::NamedTempFile::new().unwrap();
    let exe = assert_cmd::cargo::cargo_bin("media");
    let script = format!(
        "'{exe}' anubis-exec-chromium -- /bin/sh -c 'echo $$ > \"{pid}\"; exec sleep 30' & sleep 0.4; kill -9 $$",
        exe = exe.display(),
        pid = pid_file.path().display(),
    );
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .status();
    let child_pid = wait_for_pid(pid_file.path());
    assert_pid_exited(
        child_pid,
        "chromium exec wrapper survived SIGKILL of its parent",
    );
}

#[test]
fn fallback_debug_redacts_helper_paths_and_user_agent() {
    let settings = BrowserFallbackSettings::new(
        Some("/secret/helper".into()),
        Some("/secret/chrome".into()),
        "secret-user-agent".to_owned(),
        Some(Url::parse("http://rezka-proxy.internal:8888").unwrap()),
        Duration::from_secs(30),
    );
    let rendered = format!("{:?}", ChromiumChallengeFallback::new(settings));
    for secret in [
        "/secret/helper",
        "/secret/chrome",
        "secret-user-agent",
        "rezka-proxy.internal",
    ] {
        assert!(!rendered.contains(secret), "debug leaked {secret}");
    }
}

#[test]
fn default_config_enables_fallback_only_when_chrome_is_present() {
    let config = RunnerConfig::load_from(&FakeSource::valid_runner()).unwrap();
    let default_chrome = Path::new(media::anubis_browser::default_chromium_bin());
    if default_chrome.is_file() {
        let fallback = config.rezka().browser_fallback().unwrap();
        assert_eq!(fallback.chromium_bin(), Some(default_chrome));
        assert!(fallback.helper().is_none());
    } else {
        assert!(config.rezka().browser_fallback().is_none());
    }
}

#[test]
fn helper_enables_fallback_without_chrome() {
    let helper = write_helper("#!/bin/sh\nexit 0\n");
    let mut source = FakeSource::valid_runner();
    source.set_env("MEDIA_REZKA_BROWSER_HELPER", helper.path().as_os_str());
    let config = RunnerConfig::load_from(&source).unwrap();
    let fallback = config.rezka().browser_fallback().unwrap();
    assert_eq!(fallback.helper(), Some(helper.path()));
    assert!(fallback.chromium_bin().is_none());
    let debug = format!("{:?}", config.rezka());
    assert!(!debug.contains(helper.path().to_str().unwrap()));
}

#[test]
fn relative_helper_path_is_rejected() {
    let mut relative = FakeSource::valid_runner();
    relative.set_env("MEDIA_REZKA_BROWSER_HELPER", "relative-helper");
    assert_eq!(
        RunnerConfig::load_from(&relative).unwrap_err(),
        ConfigError::InvalidEnvironment {
            name: "MEDIA_REZKA_BROWSER_HELPER",
        }
    );
}
