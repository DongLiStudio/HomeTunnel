use crate::request::Request;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, TcpListener},
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
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            minutes: 30,
            autostart: true,
            theme: "system".into(),
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
    forward_ready: bool,
    port_conflict: bool,
}
struct Data {
    settings: Settings,
    connections: BTreeMap<String, Connection>,
    notices: Vec<Notice>,
    generation: u64,
    local_hosts: BTreeMap<String, (Instant, bool)>,
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
        let socket = std::env::var("SSH_AUTH_SOCK").unwrap_or_default();
        if socket.is_empty() || socket.contains("com.apple.launchd") {
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
                local_hosts: BTreeMap::new(),
            })),
            config,
        }
    }
    pub async fn settings(&self) -> Settings {
        self.data.lock().await.settings.clone()
    }
    pub async fn save_settings(&self, value: Settings) -> Result<(), String> {
        if !(1..=240).contains(&value.minutes)
            || !["system", "light", "dark"].contains(&value.theme.as_str())
        {
            return Err("设置值无效".into());
        }
        let mut d = self.data.lock().await;
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
    pub async fn submit(&self, request: Request) -> Result<(), String> {
        let name = request.name.clone();
        if let Some((id, generation)) = self.enqueue(request).await? {
            self.notice(format!("{name}：开始连接")).await;
            self.start(id, generation);
        }
        Ok(())
    }
    async fn enqueue(&self, request: Request) -> Result<Option<(String, u64)>, String> {
        let id = request.key();
        let mut d = self.data.lock().await;
        let ttl = Duration::from_secs(d.settings.minutes as u64 * 60);
        if let Some(c) = d.connections.get_mut(&id) {
            if c.request.fingerprint != request.fingerprint {
                return Err("同一路由的主机指纹发生变化；先断开并核实身份".into());
            }
            c.request = request.clone();
            c.view.name = request.name.clone();
            c.expires = Instant::now() + ttl;
            c.opened = false;
            if c.view.status != "failed" {
                return Ok(None);
            }
        }
        d.generation += 1;
        let generation = d.generation;
        let view = View {
            id: id.clone(),
            name: request.name.clone(),
            host: request.host.clone(),
            user: request.user.clone(),
            port: request.port,
            remote_port: request.remote_port,
            status: "connecting".into(),
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
                forward_ready: false,
                port_conflict: false,
            },
        );
        drop(d);
        Ok(Some((id, generation)))
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
        open_browser(&c.effective_request(), c.view.status == "local")
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
    async fn launch_tunnel(
        &self,
        id: &str,
        generation: u64,
        request: &Request,
        retry: bool,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, String> {
        // Keep the OS reservation alive until immediately before SSH starts.
        // In-flight connections also reserve their chosen port in our state.
        let listener = {
            let mut d = self.data.lock().await;
            if !matches!(d.connections.get(id), Some(c) if c.generation == generation) {
                return Ok(None);
            }
            let reserved = d
                .connections
                .iter()
                .filter(|(key, c)| key.as_str() != id && c.view.status != "failed")
                .map(|(_, c)| c.view.port)
                .collect::<Vec<_>>();
            let listener = local_listener(
                if retry { 0 } else { request.port },
                request.fixed_port,
                &reserved,
            )?;
            let port = listener.local_addr().map_err(|e| e.to_string())?.port();
            let c = d.connections.get_mut(id).unwrap();
            c.view.port = port;
            c.view.error.clear();
            c.forward_ready = false;
            c.port_conflict = false;
            listener
        };
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        if port != request.port {
            self.notice(format!(
                "{}：本机端口 {} 不可用，已自动选择 {}",
                request.name, request.port, port
            ))
            .await;
        }
        let mut effective = request.clone();
        effective.port = port;
        let mut cmd = command("ssh");
        cmd.args(effective.ssh_args())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        drop(listener);
        let mut child = cmd
            .spawn()
            .map_err(|_| "无法启动系统 OpenSSH；请安装 SSH 客户端")?;
        #[cfg(windows)]
        let job = match crate::platform::OwnedJob::attach(child.id().unwrap_or(0)) {
            Ok(job) => job,
            Err(e) => {
                let _ = child.kill().await;
                return Err(format!("无法建立进程生命周期保护：{e}"));
            }
        };
        let stderr = child.stderr.take().ok_or("无法读取 SSH 连接状态")?;
        {
            let mut d = self.data.lock().await;
            match d.connections.get_mut(id) {
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
                    return Ok(None);
                }
            }
        }
        let engine = self.clone();
        let key = id.to_string();
        Ok(Some(tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.chars().take(1200).collect::<String>();
                let mut d = engine.data.lock().await;
                if let Some(c) = d.connections.get_mut(&key) {
                    if c.generation != generation || c.view.port != port {
                        return;
                    }
                    if line
                        == format!("debug1: Local forwarding listening on 127.0.0.1 port {port}.")
                    {
                        c.forward_ready = true;
                    } else if !line.starts_with("debug") {
                        c.port_conflict |= is_port_conflict(&line);
                        c.view.error = explain_ssh(&line);
                    }
                }
            }
        })))
    }
    async fn connect(&self, id: String, generation: u64) {
        let connect_started = Instant::now();
        let request = {
            let d = self.data.lock().await;
            match d.connections.get(&id) {
                Some(c) if c.generation == generation => c.request.clone(),
                _ => return,
            }
        };
        let host_key = format!(
            "{}:{}:{:?}",
            request.host, request.ssh_port, request.fingerprint
        );
        let cached = self
            .data
            .lock()
            .await
            .local_hosts
            .get(&host_key)
            .copied()
            .filter(|(time, _)| time.elapsed() < Duration::from_secs(30));
        let direct = match cached {
            Some((_, local)) => local,
            None => {
                let local = is_this_machine(&request).await;
                let mut data = self.data.lock().await;
                if data.local_hosts.len() >= 128 {
                    data.local_hosts.clear();
                }
                data.local_hosts.insert(host_key, (Instant::now(), local));
                local
            }
        };
        let mut stderr_task = None;
        let mut bind_retries = 0;
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
            if let Some(expected) = &request.fingerprint {
                if let Err(e) = verify_known_host(&request, expected).await {
                    self.fail(&id, generation, e).await;
                    return;
                }
            }
            match self.launch_tunnel(&id, generation, &request, false).await {
                Ok(Some(task)) => stderr_task = Some(task),
                Ok(None) => return,
                Err(e) => {
                    self.fail(&id, generation, e).await;
                    return;
                }
            }
        }
        let started = connect_started;
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
                    drop(d);
                    if let Some(mut task) = stderr_task.take() {
                        if tokio::time::timeout(Duration::from_secs(1), &mut task)
                            .await
                            .is_err()
                        {
                            task.abort();
                        }
                    }
                    let mut d = self.data.lock().await;
                    let Some(c) = d.connections.get_mut(&id) else {
                        return;
                    };
                    if c.generation != generation {
                        return;
                    }
                    c.child.take();
                    #[cfg(windows)]
                    {
                        c.job.take();
                    }
                    c.view.pid = None;
                    if c.port_conflict && !request.fixed_port && bind_retries < 3 {
                        bind_retries += 1;
                        c.view.status = "connecting".into();
                        drop(d);
                        match self.launch_tunnel(&id, generation, &request, true).await {
                            Ok(Some(task)) => stderr_task = Some(task),
                            Ok(None) => return,
                            Err(e) => {
                                self.fail(&id, generation, e).await;
                                return;
                            }
                        }
                        continue;
                    }
                    c.view.status = "failed".into();
                    if c.view.error.is_empty() {
                        c.view.error = "SSH 进程已结束，请检查 Agent 授权与网络".into();
                    }
                    return;
                }
            }
            let page = c.effective_request().page_url(direct);
            let needs_open = !c.opened;
            let forward_ready = direct || c.forward_ready;
            drop(d);
            let responded = forward_ready && (!needs_open || ready(&page).await);
            let mut d = self.data.lock().await;
            let Some(c) = d.connections.get_mut(&id) else {
                return;
            };
            if c.generation != generation {
                return;
            }
            let mut ready_notice = None;
            if responded {
                c.view.status = if direct { "local" } else { "connected" }.into();
                if !c.opened {
                    match open_browser(&c.effective_request(), direct) {
                        Ok(()) => {
                            c.opened = true;
                            c.view.error.clear();
                            ready_notice = Some(format!(
                                "{}：{}已就绪（{} ms）",
                                c.view.name,
                                if direct { "本机网页" } else { "隧道" },
                                started.elapsed().as_millis()
                            ));
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
            if let Some(notice) = ready_notice {
                self.notice(notice).await;
            }
            tokio::time::sleep(if needs_open {
                Duration::from_millis(150)
            } else {
                Duration::from_secs(1)
            })
            .await;
        }
    }
}
impl Connection {
    fn effective_request(&self) -> Request {
        let mut request = self.request.clone();
        request.port = self.view.port;
        request
    }
}

fn local_listener(preferred: u16, fixed: bool, reserved: &[u16]) -> Result<TcpListener, String> {
    if preferred != 0 && !reserved.contains(&preferred) {
        if let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, preferred)) {
            return Ok(listener);
        }
    }
    if fixed {
        return Err(format!(
            "固定本机端口 {preferred} 已被占用或不可用，请释放端口后重试"
        ));
    }
    for _ in 0..16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|e| format!("无法分配空闲本机端口：{e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        if !reserved.contains(&port) {
            return Ok(listener);
        }
    }
    Err("暂时无法分配空闲本机端口，请重试".into())
}

fn is_port_conflict(line: &str) -> bool {
    line.starts_with("bind [127.0.0.1]:")
        || line.contains("Address already in use")
        || line.contains("Only one usage of each socket address")
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
    let local = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.ip())
        .collect::<Vec<_>>();
    if let Ok(Ok(addrs)) = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::lookup_host((r.host.as_str(), r.ssh_port)),
    )
    .await
    {
        if addrs
            .into_iter()
            .any(|a| a.ip().is_loopback() || local.contains(&a.ip()))
        {
            return true;
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
        "公钥认证失败；请启用或解锁 SSH Agent，检查其中身份及服务端公钥授权".into()
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
    async fn new_request_is_ready_without_extra_approval() {
        let e = engine();
        assert!(e.enqueue(request()).await.unwrap().is_some());
        let rows = e.snapshot().await;
        assert_eq!(rows[0].status, "connecting");
        assert_eq!(rows[0].pid, None);
        e.disconnect_all().await;
        assert!(e.snapshot().await.is_empty());
    }
    #[tokio::test]
    async fn duplicate_request_reuses_one_record() {
        let e = engine();
        e.enqueue(request()).await.unwrap();
        let mut r = request();
        r.path = "/two".into();
        e.enqueue(r).await.unwrap();
        assert_eq!(e.snapshot().await.len(), 1);
        let d = e.data.lock().await;
        assert_eq!(d.connections.values().next().unwrap().request.path, "/two");
    }
    #[tokio::test]
    async fn reopening_original_link_keeps_allocated_port_and_current_path() {
        let e = engine();
        let original = request();
        let id = original.key();
        e.enqueue(original.clone()).await.unwrap();
        {
            let mut d = e.data.lock().await;
            let c = d.connections.get_mut(&id).unwrap();
            c.view.port = 49152;
            c.view.status = "connected".into();
        }
        let mut repeated = original;
        repeated.path = "/two".into();
        repeated.port = 23456;
        assert!(e.enqueue(repeated).await.unwrap().is_none());
        let d = e.data.lock().await;
        assert_eq!(d.connections.len(), 1);
        let c = d.connections.get(&id).unwrap();
        assert_eq!(
            c.effective_request().page_url(false),
            "http://127.0.0.1:49152/two"
        );
    }
    #[tokio::test]
    async fn simultaneous_requests_start_only_one_connection() {
        let e = engine();
        let (a, b) = tokio::join!(e.enqueue(request()), e.enqueue(request()));
        assert_ne!(a.unwrap().is_some(), b.unwrap().is_some());
        assert_eq!(e.snapshot().await.len(), 1);
    }
    #[tokio::test]
    async fn changed_fingerprint_does_not_reuse_an_existing_connection() {
        let e = engine();
        e.enqueue(request()).await.unwrap();
        let mut changed = request();
        changed.fingerprint = Some("different".into());
        assert!(e.enqueue(changed).await.is_err());
    }
    #[test]
    fn occupied_port_selects_another_loopback_listener_and_preserves_owner() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let selected = local_listener(port, false, &[]).unwrap();
        assert_ne!(selected.local_addr().unwrap().port(), port);
        assert!(selected.local_addr().unwrap().ip().is_loopback());
        assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err());
        assert_eq!(occupied.local_addr().unwrap().port(), port);
    }
    #[test]
    fn available_preferred_port_is_used() {
        let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let selected = local_listener(port, false, &[]).unwrap();
        assert_eq!(selected.local_addr().unwrap().port(), port);
    }
    #[test]
    fn in_flight_port_is_not_reassigned() {
        let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let selected = local_listener(port, false, &[port]).unwrap();
        assert_ne!(selected.local_addr().unwrap().port(), port);
    }
    #[test]
    fn fixed_port_conflict_does_not_silently_change_the_callback_origin() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        assert!(local_listener(port, true, &[]).is_err());
    }
    #[test]
    fn only_bind_errors_are_eligible_for_port_retry() {
        assert!(is_port_conflict(
            "bind [127.0.0.1]:18082: Address already in use"
        ));
        assert!(is_port_conflict(
            "bind [127.0.0.1]:18082: Permission denied"
        ));
        assert!(!is_port_conflict("Permission denied (publickey)."));
        assert!(!is_port_conflict("Host key verification failed."));
        assert!(!is_port_conflict("agent refused operation"));
    }
    #[tokio::test]
    async fn disconnect_removes_and_renew_missing_fails() {
        let e = engine();
        let r = request();
        let id = r.key();
        e.enqueue(r).await.unwrap();
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
