use crate::request::Request;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::Mutex,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub minutes: u32,
    pub autostart: bool,
    pub theme: String,
    pub trusted_routes: Vec<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            minutes: 30,
            autostart: true,
            theme: "system".into(),
            trusted_routes: vec![],
        }
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub remote_port: u16,
    pub status: String,
    pub error: String,
    pub remaining: u64,
    pub pid: Option<u32>,
}
#[derive(Clone, Serialize)]
pub struct Notice {
    pub time: u64,
    pub message: String,
}
struct Connection {
    request: Request,
    view: View,
    child: Option<Child>,
    #[cfg(windows)]
    job: Option<crate::platform::OwnedJob>,
    expires: Instant,
    generation: u64,
    opened: bool,
}
struct Data {
    settings: Settings,
    connections: BTreeMap<String, Connection>,
    notices: Vec<Notice>,
    generation: u64,
}
#[derive(Clone)]
pub struct Engine {
    data: Arc<Mutex<Data>>,
    config: PathBuf,
}

fn unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    }
    #[cfg(target_os = "macos")]
    {
        // GUI apps do not inherit the interactive shell's Agent socket.
        if std::env::var_os("SSH_AUTH_SOCK").is_none() {
            if let Some(home) = dirs::home_dir() {
                for socket in [
                    home.join(
                        "Library/Containers/com.bitwarden.desktop/Data/.bitwarden-ssh-agent.sock",
                    ),
                    home.join(".bitwarden-ssh-agent.sock"),
                ] {
                    if socket.exists() {
                        cmd.env("SSH_AUTH_SOCK", socket);
                        break;
                    }
                }
            }
        }
    }
    cmd
}
impl Engine {
    pub fn new(config: PathBuf) -> Self {
        let settings = std::fs::read(&config)
            .ok()
            .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
            .unwrap_or_default();
        Self {
            data: Arc::new(Mutex::new(Data {
                settings,
                connections: BTreeMap::new(),
                notices: vec![],
                generation: 0,
            })),
            config,
        }
    }
    pub async fn settings(&self) -> Settings {
        self.data.lock().await.settings.clone()
    }
    pub async fn save_settings(&self, mut value: Settings) -> Result<(), String> {
        if !(1..=240).contains(&value.minutes)
            || !["system", "light", "dark"].contains(&value.theme.as_str())
        {
            return Err("设置值无效".into());
        }
        let mut d = self.data.lock().await;
        value.trusted_routes = d.settings.trusted_routes.clone();
        self.persist(&value)?;
        d.settings = value;
        Ok(())
    }
    fn persist(&self, s: &Settings) -> Result<(), String> {
        if let Some(p) = self.config.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let pending = self.config.with_extension("tmp");
        std::fs::write(
            &pending,
            serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        crate::platform::replace_file(&pending, &self.config)
    }
    pub async fn notice(&self, message: String) {
        let mut d = self.data.lock().await;
        d.notices.push(Notice {
            time: unix(),
            message,
        });
        if d.notices.len() > 80 {
            d.notices.remove(0);
        }
    }
    pub async fn notices(&self) -> Vec<Notice> {
        self.data.lock().await.notices.clone()
    }
    pub async fn snapshot(&self) -> Vec<View> {
        let d = self.data.lock().await;
        d.connections
            .values()
            .map(|c| {
                let mut v = c.view.clone();
                v.remaining = c
                    .expires
                    .saturating_duration_since(Instant::now())
                    .as_secs();
                v
            })
            .collect()
    }
    pub async fn submit(&self, request: Request) -> bool {
        let id = request.key();
        let mut d = self.data.lock().await;
        let ttl = Duration::from_secs(d.settings.minutes as u64 * 60);
        if let Some(c) = d.connections.get_mut(&id) {
            if c.request.fingerprint != request.fingerprint {
                c.view.error = "同一路由的主机指纹发生变化；先断开并核实身份".into();
                return false;
            }
            c.request = request.clone();
            c.expires = Instant::now() + ttl;
            c.opened = false;
            if !["failed", "awaitingTrust"].contains(&c.view.status.as_str()) {
                return false;
            }
        }
        d.generation += 1;
        let generation = d.generation;
        let trusted = d.settings.trusted_routes.contains(&id);
        let view = View {
            id: id.clone(),
            name: request.name.clone(),
            host: request.host.clone(),
            user: request.user.clone(),
            port: request.port,
            remote_port: request.remote_port,
            status: if trusted {
                "connecting"
            } else {
                "awaitingTrust"
            }
            .into(),
            error: String::new(),
            remaining: ttl.as_secs(),
            pid: None,
        };
        d.connections.insert(
            id.clone(),
            Connection {
                request,
                view,
                child: None,
                #[cfg(windows)]
                job: None,
                expires: Instant::now() + ttl,
                generation,
                opened: false,
            },
        );
        drop(d);
        if trusted {
            self.start(id, generation);
        }
        !trusted
    }
    pub async fn approve(&self, id: String) -> Result<(), String> {
        let mut d = self.data.lock().await;
        if !d.connections.contains_key(&id) {
            return Err("连接已不存在".into());
        }
        if !d.settings.trusted_routes.contains(&id) {
            d.settings.trusted_routes.push(id.clone());
            self.persist(&d.settings)?;
        }
        let c = d.connections.get_mut(&id).unwrap();
        if c.view.status != "awaitingTrust" {
            return Ok(());
        }
        c.view.status = "connecting".into();
        let generation = c.generation;
        drop(d);
        self.start(id, generation);
        Ok(())
    }
    pub async fn clear_trust(&self) -> Result<(), String> {
        let mut d = self.data.lock().await;
        d.settings.trusted_routes.clear();
        self.persist(&d.settings)
    }
    pub async fn disconnect(&self, id: &str) {
        let removed = self.data.lock().await.connections.remove(id);
        if let Some(mut c) = removed {
            if let Some(child) = c.child.as_mut() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            self.notice(format!("{} 已断开", c.view.name)).await;
        }
    }
    pub async fn disconnect_all(&self) {
        let ids = self
            .data
            .lock()
            .await
            .connections
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            self.disconnect(&id).await;
        }
    }
    pub async fn renew(&self, id: &str) -> Result<(), String> {
        let mut d = self.data.lock().await;
        let minutes = d.settings.minutes;
        let c = d.connections.get_mut(id).ok_or("连接已不存在")?;
        c.expires = Instant::now() + Duration::from_secs(minutes as u64 * 60);
        Ok(())
    }
    pub async fn open(&self, id: &str) -> Result<(), String> {
        let d = self.data.lock().await;
        let c = d.connections.get(id).ok_or("连接已不存在")?;
        if !["connected", "local"].contains(&c.view.status.as_str()) {
            return Err("连接尚未就绪".into());
        }
        open_browser(&c.request, c.view.status == "local")
    }
    async fn fail(&self, id: &str, generation: u64, message: String) {
        let mut d = self.data.lock().await;
        if let Some(c) = d.connections.get_mut(id) {
            if c.generation == generation {
                c.view.status = "failed".into();
                c.view.error = message;
            }
        }
    }
    fn start(&self, id: String, generation: u64) {
        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            engine.connect(id, generation).await;
        });
    }
    async fn connect(&self, id: String, generation: u64) {
        let request = {
            let d = self.data.lock().await;
            match d.connections.get(&id) {
                Some(c) if c.generation == generation => c.request.clone(),
                _ => return,
            }
        };
        let direct = is_this_machine(&request).await;
        if direct {
            if !ready(&request.page_url(true)).await {
                self.fail(
                    &id,
                    generation,
                    "本机网页尚未响应；不会向本机建立隧道".into(),
                )
                .await;
                return;
            }
        } else {
            if let Err(e) = agent_available().await {
                self.fail(&id, generation, e).await;
                return;
            }
            if tokio::net::TcpStream::connect(("127.0.0.1", request.port))
                .await
                .is_ok()
            {
                self.fail(
                    &id,
                    generation,
                    "本地端口被其他进程占用；未接管或终止其他程序".into(),
                )
                .await;
                return;
            }
            if let Some(expected) = &request.fingerprint {
                if let Err(e) = verify_known_host(&request, expected).await {
                    self.fail(&id, generation, e).await;
                    return;
                }
            }
            let mut cmd = command("ssh");
            cmd.args(request.ssh_args())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = match cmd.spawn() {
                Ok(p) => p,
                Err(_) => {
                    self.fail(
                        &id,
                        generation,
                        "无法启动系统 OpenSSH；请安装 SSH 客户端".into(),
                    )
                    .await;
                    return;
                }
            };
            #[cfg(windows)]
            let job = match crate::platform::OwnedJob::attach(child.id().unwrap_or(0)) {
                Ok(job) => job,
                Err(e) => {
                    let _ = child.kill().await;
                    self.fail(&id, generation, format!("无法建立进程生命周期保护：{e}"))
                        .await;
                    return;
                }
            };
            if let Some(stderr) = child.stderr.take() {
                let e = self.clone();
                let key = id.clone();
                tauri::async_runtime::spawn(async move {
                    let mut lines = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let line = line.chars().take(1200).collect::<String>();
                        let mut d = e.data.lock().await;
                        if let Some(c) = d.connections.get_mut(&key) {
                            if c.generation == generation {
                                c.view.error = explain_ssh(&line);
                            }
                        }
                    }
                });
            }
            let mut d = self.data.lock().await;
            match d.connections.get_mut(&id) {
                Some(c) if c.generation == generation => {
                    c.view.pid = child.id();
                    c.child = Some(child);
                    #[cfg(windows)]
                    {
                        c.job = Some(job);
                    }
                }
                _ => {
                    drop(d);
                    let _ = child.kill().await;
                    return;
                }
            }
        }
        let started = Instant::now();
        loop {
            let mut d = self.data.lock().await;
            let Some(c) = d.connections.get_mut(&id) else {
                return;
            };
            if c.generation != generation {
                return;
            }
            if Instant::now() >= c.expires {
                drop(d);
                self.disconnect(&id).await;
                return;
            }
            if let Some(child) = c.child.as_mut() {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    c.view.status = "failed".into();
                    if c.view.error.is_empty() {
                        c.view.error = "SSH 进程已结束，请检查 Agent 授权与网络".into();
                    }
                    return;
                }
            }
            let page = c.request.page_url(direct);
            let needs_open = !c.opened;
            drop(d);
            let responded = if needs_open { ready(&page).await } else { true };
            let mut d = self.data.lock().await;
            let Some(c) = d.connections.get_mut(&id) else {
                return;
            };
            if c.generation != generation {
                return;
            }
            if responded {
                c.view.status = if direct { "local" } else { "connected" }.into();
                if !c.opened {
                    match open_browser(&c.request, direct) {
                        Ok(()) => {
                            c.opened = true;
                            c.view.error.clear();
                        }
                        Err(e) => c.view.error = e,
                    }
                }
            } else if started.elapsed() > Duration::from_secs(90) {
                if let Some(child) = c.child.as_mut() {
                    let _ = child.start_kill();
                }
                c.view.status = "failed".into();
                c.view.error = "连接等待超过90秒，请检查签名是否批准及服务是否运行".into();
                return;
            }
            drop(d);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
pub async fn agent_available() -> Result<(), String> {
    let output = tokio::time::timeout(
        Duration::from_secs(8),
        command("ssh-add")
            .arg("-l")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "SSH Agent 检查超时，请检查 Agent 是否正常运行")?
    .map_err(|_| "找不到 ssh-add，请安装系统 OpenSSH")?;
    if output.status.success() {
        Ok(())
    } else {
        Err("SSH Agent 不可用或没有身份。请启用并解锁 Bitwarden / SSH Agent；不提供密码回退".into())
    }
}
async fn ready(page: &str) -> bool {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build();
    match client {
        Ok(c) => c
            .get(page)
            .send()
            .await
            .map(|r| r.status().as_u16() < 500)
            .unwrap_or(false),
        Err(_) => false,
    }
}
async fn is_this_machine(r: &Request) -> bool {
    let local = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.ip())
        .collect::<Vec<_>>();
    if let Ok(addrs) = tokio::net::lookup_host((r.host.as_str(), r.ssh_port)).await {
        if addrs
            .into_iter()
            .any(|a| a.ip().is_loopback() || local.contains(&a.ip()))
        {
            return true;
        }
    }
    if let Some(expected) = &r.fingerprint {
        #[cfg(windows)]
        let key = PathBuf::from(
            std::env::var_os("PROGRAMDATA").unwrap_or_else(|| "C:\\ProgramData".into()),
        )
        .join("ssh/ssh_host_ed25519_key.pub");
        #[cfg(not(windows))]
        let key = PathBuf::from("/etc/ssh/ssh_host_ed25519_key.pub");
        if let Ok(text) = std::fs::read_to_string(key) {
            if let Some(f) = fingerprint(&text) {
                return &f == expected;
            }
        }
    }
    false
}
fn fingerprint(line: &str) -> Option<String> {
    let value = line.split_whitespace().find(|v| v.starts_with("AAAA"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .ok()?;
    Some(format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(bytes))
    ))
}
async fn verify_known_host(r: &Request, expected: &str) -> Result<(), String> {
    let target = if r.ssh_port == 22 {
        r.host.clone()
    } else {
        format!("[{}]:{}", r.host, r.ssh_port)
    };
    let out = command("ssh-keygen")
        .args(["-F", &target])
        .output()
        .await
        .map_err(|_| "无法读取 OpenSSH 主机信任记录")?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(fingerprint)
        .any(|f| f == expected)
    {
        Ok(())
    } else {
        Err("目标指纹与已信任主机记录不匹配或记录缺失。请在系统终端核实主机指纹后建立信任；不会自动接受主机密钥".into())
    }
}
fn explain_ssh(line: &str) -> String {
    if line.contains("agent refused") || line.contains("sign_and_send_pubkey") {
        "Agent 拒绝签名；请解锁并允许本次请求".into()
    } else if line.contains("Permission denied") {
        "公钥认证失败；请检查 Agent 中的身份及服务端公钥授权".into()
    } else if line.contains("Host key verification failed")
        || line.contains("REMOTE HOST IDENTIFICATION")
    {
        "主机密钥校验失败；请人工核实，管理器不会绕过校验".into()
    } else {
        line.into()
    }
}
pub fn open_browser(r: &Request, direct: bool) -> Result<(), String> {
    let page = r.page_url(direct);
    #[cfg(target_os = "macos")]
    {
        let bundle = match r.browser.as_str() {
            "safari" => Some("com.apple.Safari"),
            "chrome" => Some("com.google.Chrome"),
            "edge" => Some("com.microsoft.edgemac"),
            "firefox" => Some("org.mozilla.firefox"),
            _ => None,
        };
        let mut cmd = std::process::Command::new("/usr/bin/open");
        if let Some(b) = bundle {
            cmd.args(["-b", b]);
        }
        cmd.arg(page)
            .spawn()
            .map(|_| ())
            .map_err(|_| "无法打开来源浏览器".into())
    }
    #[cfg(windows)]
    {
        let relative = match r.browser.as_str() {
            "edge" => Some("Microsoft/Edge/Application/msedge.exe"),
            "chrome" => Some("Google/Chrome/Application/chrome.exe"),
            "firefox" => Some("Mozilla Firefox/firefox.exe"),
            _ => None,
        };
        if let Some(relative) = relative {
            for root in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
                if let Some(root) = std::env::var_os(root) {
                    let exe = PathBuf::from(root).join(relative);
                    if exe.is_file() {
                        return std::process::Command::new(exe)
                            .arg(if r.browser == "firefox" {
                                "-new-tab"
                            } else {
                                "--new-tab"
                            })
                            .arg(&page)
                            .spawn()
                            .map(|_| ())
                            .map_err(|_| "无法打开来源浏览器".into());
                    }
                }
            }
        }
        open::that(page).map_err(|_| "无法打开默认浏览器".into())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        open::that(page).map_err(|_| "无法打开浏览器".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn engine() -> Engine {
        Engine::new(std::env::temp_dir().join(format!(
            "hometunnel-test-{}-{}.json",
            std::process::id(),
            unix()
        )))
    }
    fn request() -> Request {
        Request::parse("hometunnel://connect?host=example.invalid&user=test&sshPort=22&port=12345&remotePort=80&path=%2Fone").unwrap()
    }
    #[tokio::test]
    async fn untrusted_does_not_spawn() {
        let e = engine();
        assert!(e.submit(request()).await);
        let rows = e.snapshot().await;
        assert_eq!(rows[0].status, "awaitingTrust");
        assert_eq!(rows[0].pid, None);
        e.disconnect_all().await;
        assert!(e.snapshot().await.is_empty());
    }
    #[tokio::test]
    async fn duplicate_pending_is_one_row() {
        let e = engine();
        e.submit(request()).await;
        let mut r = request();
        r.path = "/two".into();
        e.submit(r).await;
        assert_eq!(e.snapshot().await.len(), 1);
        let d = e.data.lock().await;
        assert_eq!(d.connections.values().next().unwrap().request.path, "/two");
    }
    #[tokio::test]
    async fn disconnect_removes_and_renew_missing_fails() {
        let e = engine();
        let r = request();
        let id = r.key();
        e.submit(r).await;
        assert!(e.renew(&id).await.is_ok());
        e.disconnect(&id).await;
        assert!(e.snapshot().await.is_empty());
        assert!(e.renew(&id).await.is_err());
    }
    #[tokio::test]
    async fn validates_settings() {
        let e = engine();
        let s = Settings {
            minutes: 0,
            ..Settings::default()
        };
        assert!(e.save_settings(s).await.is_err());
    }
    #[test]
    fn fingerprint_is_public_key_digest() {
        let digest = fingerprint("ssh-ed25519 AAAA test").unwrap();
        assert!(digest.starts_with("SHA256:"));
        assert_eq!(digest.len(), 50);
    }
}
