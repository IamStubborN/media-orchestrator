use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    time::{Duration, Instant},
};

use futures_util::{SinkExt as _, StreamExt as _};
use rezka_client::{
    RezkaError,
    redaction::sanitize_provider_text,
    session::anubis::{self, BrowserChallengeFallback, CLEARANCE_COOKIE},
};
use serde_json::{Value, json};
use tokio::{
    io::AsyncReadExt as _,
    process::{Child, Command},
    time::timeout,
};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

const DEFAULT_CHROMIUM_BIN: &str = "/usr/local/lib/chrome-headless-shell/chrome-headless-shell";
const HELPER_SUBCOMMAND: &str = "anubis-browser-challenge";
const CHROME_EXEC_SUBCOMMAND: &str = "anubis-exec-chromium";
const MAX_HELPER_STDOUT_BYTES: usize = 64 * 1024;
const MAX_SET_COOKIE_HEADERS: usize = 64;
const MAX_SET_COOKIE_HEADER_BYTES: usize = 8 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone)]
pub struct BrowserFallbackSettings {
    helper: Option<PathBuf>,
    chromium_bin: Option<PathBuf>,
    user_agent: String,
    proxy_url: Option<Url>,
    timeout: Duration,
}

impl std::fmt::Debug for BrowserFallbackSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserFallbackSettings")
            .field("helper", &"[REDACTED]")
            .field("chromium_bin", &"[REDACTED]")
            .field("user_agent", &"[REDACTED]")
            .field("proxy_url", &"[REDACTED]")
            .field("timeout_ms", &self.timeout.as_millis())
            .finish()
    }
}

impl BrowserFallbackSettings {
    #[must_use]
    pub fn new(
        helper: Option<PathBuf>,
        chromium_bin: Option<PathBuf>,
        user_agent: String,
        proxy_url: Option<Url>,
        timeout: Duration,
    ) -> Self {
        Self {
            helper,
            chromium_bin,
            user_agent,
            proxy_url,
            timeout,
        }
    }
}

pub struct ChromiumChallengeFallback {
    settings: BrowserFallbackSettings,
}

impl std::fmt::Debug for ChromiumChallengeFallback {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChromiumChallengeFallback")
            .field("settings", &self.settings)
            .finish()
    }
}

impl ChromiumChallengeFallback {
    #[must_use]
    pub fn new(settings: BrowserFallbackSettings) -> Self {
        Self { settings }
    }
}

impl BrowserChallengeFallback for ChromiumChallengeFallback {
    fn solve<'a>(
        &'a mut self,
        _challenge: &'a anubis::AnubisChallenge,
        origin: &'a Url,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, RezkaError>> + Send + 'a>> {
        Box::pin(async move { spawn_helper(&self.settings, origin).await })
    }
}

pub struct ChallengeHelperRequest {
    pub url: Url,
    pub chromium_bin: PathBuf,
    pub user_agent: String,
    pub proxy: Option<Url>,
    pub timeout: Duration,
}

pub async fn run_challenge_helper(request: ChallengeHelperRequest) -> i32 {
    install_parent_death_signal();
    match drive_chromium(request).await {
        Ok(cookies) => {
            println!("{}", json!({ "set_cookie": cookies }));
            0
        }
        Err(HelperFailure::Timeout) => {
            let _ = writeln_stderr("anubis-browser: timeout");
            1
        }
        Err(HelperFailure::Process) => {
            let _ = writeln_stderr("anubis-browser: process failed");
            2
        }
        Err(HelperFailure::NoClearance) => {
            let _ = writeln_stderr("anubis-browser: clearance missing");
            3
        }
        Err(HelperFailure::Rejected) => {
            let _ = writeln_stderr("anubis-browser: challenge remained");
            4
        }
    }
}

enum HelperFailure {
    Timeout,
    Process,
    NoClearance,
    Rejected,
}

async fn spawn_helper(
    settings: &BrowserFallbackSettings,
    origin: &Url,
) -> Result<Vec<String>, RezkaError> {
    let (program, extra_args, chromium_bin) = helper_command(settings)?;
    let mut command = Command::new(program);
    command
        .args(extra_args)
        .arg("--url")
        .arg(origin.as_str())
        .arg("--user-agent")
        .arg(&settings.user_agent)
        .arg("--timeout-ms")
        .arg(settings.timeout.as_millis().to_string())
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(chromium_bin) = chromium_bin {
        command.arg("--chromium-bin").arg(chromium_bin);
    }
    if let Some(proxy) = settings.proxy_url.as_ref() {
        command.arg("--proxy").arg(proxy.as_str());
    }
    #[cfg(unix)]
    {
        command.process_group(0);
    }

    let mut child = GroupChild::spawn(command).map_err(|_| helper_failed())?;
    drain_stderr(child.child.stderr.take());
    let mut stdout = child.child.stdout.take().ok_or_else(helper_failed)?;
    let stdout_task = tokio::spawn(async move {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).await.map(|_| output)
    });

    let status = match timeout(
        settings.timeout + Duration::from_secs(2),
        child.child.wait(),
    )
    .await
    {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => {
            child.terminate();
            stdout_task.abort();
            let _ = timeout(Duration::from_secs(1), child.child.wait()).await;
            return Err(helper_failed());
        }
        Err(_) => {
            child.terminate();
            stdout_task.abort();
            let _ = timeout(Duration::from_secs(1), child.child.wait()).await;
            return Err(timeout_error());
        }
    };
    let output = stdout_task.await.map_err(|_| helper_failed())?;
    let output = output.map_err(|_| helper_failed())?;
    if output.len() > MAX_HELPER_STDOUT_BYTES {
        return Err(helper_failed());
    }
    parse_helper_output(status.code(), &output)
}

fn helper_command(
    settings: &BrowserFallbackSettings,
) -> Result<(PathBuf, Vec<String>, Option<PathBuf>), RezkaError> {
    if let Some(helper) = settings.helper.as_ref() {
        return Ok((helper.clone(), Vec::new(), settings.chromium_bin.clone()));
    }
    let exe = std::env::current_exe().map_err(|_| helper_failed())?;
    let chromium = settings.chromium_bin.clone().ok_or_else(helper_failed)?;
    Ok((exe, vec![HELPER_SUBCOMMAND.to_owned()], Some(chromium)))
}

fn parse_helper_output(code: Option<i32>, stdout: &[u8]) -> Result<Vec<String>, RezkaError> {
    match code.unwrap_or(2) {
        0 => {}
        1 => return Err(timeout_error()),
        3 | 4 => return Err(rejected_error()),
        _ => return Err(helper_failed()),
    }
    let parsed: Value = serde_json::from_slice(stdout).map_err(|_| helper_failed())?;
    let headers = parsed
        .get("set_cookie")
        .and_then(Value::as_array)
        .ok_or_else(helper_failed)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(helper_failed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if headers.len() > MAX_SET_COOKIE_HEADERS
        || headers
            .iter()
            .any(|header| header.len() > MAX_SET_COOKIE_HEADER_BYTES)
    {
        return Err(helper_failed());
    }
    if !headers
        .iter()
        .any(|header| header.starts_with(CLEARANCE_COOKIE) && header.contains('='))
    {
        return Err(rejected_error());
    }
    Ok(headers)
}

struct GroupChild {
    child: Child,
    pid: Option<u32>,
}

impl GroupChild {
    fn spawn(mut command: Command) -> std::io::Result<Self> {
        let child = command.spawn()?;
        let pid = child.id();
        Ok(Self { child, pid })
    }

    fn terminate(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            let pid = nix::unistd::Pid::from_raw(pid as i32);
            let _ = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL);
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
        }
        let _ = self.child.start_kill();
    }
}

impl Drop for GroupChild {
    fn drop(&mut self) {
        self.terminate();
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            let _ = nix::sys::wait::waitpid(nix::unistd::Pid::from_raw(pid as i32), None);
        }
    }
}

fn drain_stderr(stderr: Option<tokio::process::ChildStderr>) {
    let Some(mut stderr) = stderr else {
        return;
    };
    tokio::spawn(async move {
        let mut buf = [0_u8; 1024];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });
}

async fn drive_chromium(request: ChallengeHelperRequest) -> Result<Vec<String>, HelperFailure> {
    if !request.chromium_bin.is_absolute() {
        return Err(HelperFailure::Process);
    }
    let tmp_root = {
        let tmp = Path::new("/tmp");
        if tmp.is_dir() {
            tmp.to_path_buf()
        } else {
            std::env::temp_dir()
        }
    };
    let user_data = tmp_root.join(format!("anubis-browser-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&user_data)
        .await
        .map_err(|_| HelperFailure::Process)?;
    let mut session = ChromeSession {
        child: spawn_chrome(&request, &user_data)?,
        user_data,
    };
    let deadline = Instant::now() + request.timeout;
    let result = drive_cdp(&mut session, &request, deadline).await;
    result
}

pub fn exec_chromium(args: Vec<String>) -> i32 {
    install_parent_death_signal();
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let Ok(c_args) = args
            .iter()
            .map(|argument| CString::new(argument.as_str()))
            .collect::<Result<Vec<_>, _>>()
        else {
            return 2;
        };
        let Some(path) = c_args.first() else {
            return 2;
        };
        let _ = nix::unistd::execv(path, &c_args);
    }
    2
}

fn spawn_chrome(
    request: &ChallengeHelperRequest,
    user_data: &Path,
) -> Result<Child, HelperFailure> {
    let exe = std::env::current_exe().map_err(|_| HelperFailure::Process)?;
    let mut command = Command::new(exe);
    command
        .arg(CHROME_EXEC_SUBCOMMAND)
        .arg("--")
        .arg(&request.chromium_bin)
        .arg("--disable-gpu")
        .arg("--disable-dev-shm-usage")
        .arg("--no-sandbox")
        .arg("--disable-setuid-sandbox")
        .arg("--no-zygote")
        .arg("--single-process")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--metrics-recording-only")
        .arg("--disable-background-networking")
        .arg("--disable-extensions")
        .arg("--disable-sync")
        .arg("--mute-audio")
        .arg("--hide-scrollbars")
        .arg("--remote-debugging-port=0")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg("--remote-allow-origins=*")
        .arg(format!("--user-data-dir={}", user_data.display()))
        .arg(format!("--user-agent={}", request.user_agent))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(proxy) = request.proxy.as_ref() {
        let server = match (proxy.host_str(), proxy.port_or_known_default()) {
            (Some(host), Some(port)) => format!("http://{host}:{port}"),
            _ => return Err(HelperFailure::Process),
        };
        command.arg(format!("--proxy-server={server}"));
    }
    command.spawn().map_err(|_| HelperFailure::Process)
}

struct ChromeSession {
    child: Child,
    user_data: PathBuf,
}

impl Drop for ChromeSession {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.user_data);
    }
}

async fn drive_cdp(
    session: &mut ChromeSession,
    request: &ChallengeHelperRequest,
    deadline: Instant,
) -> Result<Vec<String>, HelperFailure> {
    let port = wait_devtools_port(&session.user_data, deadline).await?;
    let websocket = websocket_url(port, deadline).await?;
    let (stream, _) = timeout(
        remaining(deadline)?,
        tokio_tungstenite::connect_async(websocket.as_str()),
    )
    .await
    .map_err(|_| HelperFailure::Timeout)?
    .map_err(|_| HelperFailure::Process)?;
    let (write, read) = stream.split();
    let mut cdp = CdpClient {
        write,
        read,
        next_id: 1,
        deadline,
    };
    let created = cdp
        .call("Target.createTarget", json!({ "url": "about:blank" }), None)
        .await?;
    let target_id = created
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or(HelperFailure::Process)?
        .to_owned();
    let attached = cdp
        .call(
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
            None,
        )
        .await?;
    let session_id = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or(HelperFailure::Process)?
        .to_owned();
    cdp.call("Page.enable", json!({}), Some(&session_id))
        .await?;
    cdp.call("Network.enable", json!({}), Some(&session_id))
        .await?;
    cdp.call(
        "Page.navigate",
        json!({ "url": request.url.as_str() }),
        Some(&session_id),
    )
    .await?;

    let mut challenge_seen = false;
    loop {
        let present = cdp
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": "Boolean(document.getElementById('anubis_challenge'))",
                    "returnByValue": true
                }),
                Some(&session_id),
            )
            .await?;
        challenge_seen |= js_bool(&present);
        let cookies = cdp
            .call(
                "Network.getCookies",
                json!({ "urls": [request.url.as_str()] }),
                Some(&session_id),
            )
            .await?;
        let headers = set_cookie_headers(&cookies, request.url.host_str().unwrap_or_default());
        let has_clearance = headers
            .iter()
            .any(|header| header.starts_with(CLEARANCE_COOKIE) && header.contains('='));
        // HttpOnly clearance is invisible to document.cookie; CDP Network.getCookies is the source of truth.
        if !js_bool(&present) && has_clearance {
            return Ok(headers);
        }
        if Instant::now() >= deadline {
            if has_clearance {
                return Ok(headers);
            }
            return Err(if challenge_seen {
                HelperFailure::Rejected
            } else if js_bool(&present) {
                HelperFailure::Timeout
            } else {
                HelperFailure::NoClearance
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn js_bool(result: &Value) -> bool {
    result
        .pointer("/result/value")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn set_cookie_headers(result: &Value, host: &str) -> Vec<String> {
    let Some(cookies) = result.get("cookies").and_then(Value::as_array) else {
        return Vec::new();
    };
    cookies
        .iter()
        .filter(|cookie| cookie_matches_host(cookie, host))
        .filter_map(cookie_to_set_cookie)
        .take(MAX_SET_COOKIE_HEADERS)
        .collect()
}

fn cookie_matches_host(cookie: &Value, host: &str) -> bool {
    let Some(domain) = cookie.get("domain").and_then(Value::as_str) else {
        return false;
    };
    let domain = domain.trim_start_matches('.');
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn cookie_to_set_cookie(cookie: &Value) -> Option<String> {
    let name = cookie.get("name").and_then(Value::as_str)?;
    let value = cookie.get("value").and_then(Value::as_str)?;
    if name.len() + value.len() + 32 > MAX_SET_COOKIE_HEADER_BYTES {
        return None;
    }
    let mut header = format!("{name}={value}");
    if let Some(path) = cookie.get("path").and_then(Value::as_str) {
        header.push_str("; Path=");
        header.push_str(path);
    }
    if cookie.get("httpOnly").and_then(Value::as_bool) == Some(true) {
        header.push_str("; HttpOnly");
    }
    if cookie.get("secure").and_then(Value::as_bool) == Some(true) {
        header.push_str("; Secure");
    }
    match cookie.get("sameSite").and_then(Value::as_str) {
        Some("Strict") => header.push_str("; SameSite=Strict"),
        Some("Lax") => header.push_str("; SameSite=Lax"),
        Some("None") => header.push_str("; SameSite=None"),
        _ => {}
    }
    Some(header)
}

async fn wait_devtools_port(user_data: &Path, deadline: Instant) -> Result<u16, HelperFailure> {
    let path = user_data.join("DevToolsActivePort");
    loop {
        if let Ok(contents) = tokio::fs::read_to_string(&path).await {
            if let Some(port) = contents
                .lines()
                .next()
                .and_then(|line| line.trim().parse().ok())
            {
                if port > 0 {
                    return Ok(port);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(HelperFailure::Timeout);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn websocket_url(port: u16, deadline: Instant) -> Result<Url, HelperFailure> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| HelperFailure::Process)?;
    let endpoint = format!("http://127.0.0.1:{port}/json/version");
    loop {
        if let Ok(response) = client.get(&endpoint).send().await {
            if let Ok(body) = response.json::<Value>().await {
                if let Some(url) = body
                    .get("webSocketDebuggerUrl")
                    .and_then(Value::as_str)
                    .and_then(|value| Url::parse(value).ok())
                {
                    return Ok(url);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(HelperFailure::Timeout);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct CdpClient<S> {
    write: S,
    read: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    next_id: u64,
    deadline: Instant,
}

impl<S> CdpClient<S>
where
    S: futures_util::Sink<Message> + Unpin,
{
    async fn call(
        &mut self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, HelperFailure> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session_id) = session_id {
            message["sessionId"] = json!(session_id);
        }
        self.write
            .send(Message::Text(message.to_string().into()))
            .await
            .map_err(|_| HelperFailure::Process)?;
        loop {
            let incoming = timeout(remaining(self.deadline)?, self.read.next())
                .await
                .map_err(|_| HelperFailure::Timeout)?
                .ok_or(HelperFailure::Process)?
                .map_err(|_| HelperFailure::Process)?;
            let Message::Text(text) = incoming else {
                continue;
            };
            let parsed: Value = serde_json::from_str(&text).map_err(|_| HelperFailure::Process)?;
            if parsed.get("id") != Some(&json!(id)) {
                continue;
            }
            if parsed.get("error").is_some() {
                return Err(HelperFailure::Process);
            }
            return Ok(parsed.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

fn remaining(deadline: Instant) -> Result<Duration, HelperFailure> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(HelperFailure::Timeout)
}

fn writeln_stderr(line: &str) -> std::io::Result<()> {
    eprintln!("{line}");
    Ok(())
}

fn install_parent_death_signal() {
    #[cfg(all(unix, target_os = "linux"))]
    {
        // Do not treat ppid 1 as "parent already died": in Docker the runner
        // is PID 1, so the helper's living parent is 1.
        let _ = nix::sys::prctl::set_pdeathsig(Some(nix::sys::signal::Signal::SIGKILL));
    }
}

fn helper_failed() -> RezkaError {
    RezkaError::ChallengeFailed {
        context: sanitize_provider_text("browser helper failed"),
    }
}

fn timeout_error() -> RezkaError {
    RezkaError::AnubisTimeout {
        context: sanitize_provider_text("browser helper timed out"),
    }
}

fn rejected_error() -> RezkaError {
    RezkaError::AnubisRejected {
        context: sanitize_provider_text("browser helper did not produce clearance"),
    }
}

#[must_use]
pub fn default_chromium_bin() -> &'static str {
    DEFAULT_CHROMIUM_BIN
}
