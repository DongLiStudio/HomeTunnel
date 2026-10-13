use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub host: String,
    pub user: String,
    pub ssh_port: u16,
    pub port: u16,
    pub remote_port: u16,
    pub name: String,
    pub path: String,
    pub fingerprint: Option<String>,
    pub browser: String,
}

impl Request {
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.len() > 8192 {
            return Err("连接请求过长".into());
        }
        let u = Url::parse(raw).map_err(|_| "连接请求格式无效")?;
        if !["home-service", "hometunnel"].contains(&u.scheme())
            || u.host_str() != Some("connect")
            || !u.username().is_empty()
            || u.password().is_some()
            || u.port().is_some()
            || u.fragment().is_some()
            || !["", "/"].contains(&u.path())
        {
            return Err("仅接受结构化的 connect 入口".into());
        }
        let mut q = BTreeMap::new();
        for (k, v) in u.query_pairs() {
            if ![
                "host",
                "user",
                "sshPort",
                "port",
                "remotePort",
                "name",
                "path",
                "fingerprint",
                "browser",
            ]
            .contains(&k.as_ref())
                || q.insert(k.into_owned(), v.into_owned()).is_some()
            {
                return Err("请求有未知或重复字段".into());
            }
        }
        let get = |k: &str| q.get(k).cloned().ok_or_else(|| format!("缺少字段 {k}"));
        let port = |k: &str| {
            get(k)?
                .parse::<u16>()
                .ok()
                .filter(|v| *v > 0)
                .ok_or_else(|| format!("{k} 端口无效"))
        };
        let host = get("host")?;
        let user = get("user")?;
        let host_ip = host.parse::<std::net::IpAddr>().is_ok();
        if !host_ip
            && (host.len() > 253
                || host.split('.').any(|p| {
                    p.is_empty()
                        || p.len() > 63
                        || p.starts_with('-')
                        || p.ends_with('-')
                        || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                }))
        {
            return Err("目标主机格式无效".into());
        }
        if user.is_empty()
            || user.len() > 64
            || user.starts_with('-')
            || !user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err("SSH 用户名格式无效".into());
        }
        let path = q.get("path").cloned().unwrap_or_else(|| "/".into());
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains('\\')
            || path.contains('#')
            || path.chars().any(char::is_control)
        {
            return Err("网页路径无效".into());
        }
        let name = q.get("name").cloned().unwrap_or_else(|| host.clone());
        if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
            return Err("服务名称无效".into());
        }
        let browser = q
            .get("browser")
            .cloned()
            .unwrap_or_else(|| "default".into());
        if !["default", "edge", "chrome", "firefox", "safari"].contains(&browser.as_str()) {
            return Err("来源浏览器无效".into());
        }
        let fingerprint = q.get("fingerprint").cloned();
        if let Some(f) = &fingerprint {
            if !f.starts_with("SHA256:")
                || f.len() != 50
                || !f[7..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
            {
                return Err("主机指纹无效".into());
            }
        }
        Ok(Self {
            host,
            user,
            ssh_port: port("sshPort")?,
            port: port("port")?,
            remote_port: port("remotePort")?,
            name,
            path,
            fingerprint,
            browser,
        })
    }
    // The listening port is an owned resource; paths do not create another SSH process.
    pub fn key(&self) -> String {
        format!(
            "{}@{}:{}/{}:{}",
            self.user,
            self.host.to_ascii_lowercase(),
            self.ssh_port,
            self.port,
            self.remote_port
        )
    }
    pub fn page_url(&self, direct: bool) -> String {
        format!(
            "http://127.0.0.1:{}{}",
            if direct { self.remote_port } else { self.port },
            self.path
        )
    }
    pub fn ssh_args(&self) -> Vec<String> {
        [
            "-o",
            "BatchMode=yes",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "PreferredAuthentications=publickey",
            "-o",
            "IdentityFile=none",
            "-o",
            "IdentitiesOnly=no",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ConnectTimeout=15",
            "-N",
            "-L",
        ]
        .into_iter()
        .map(String::from)
        .chain([
            format!("127.0.0.1:{}:127.0.0.1:{}", self.port, self.remote_port),
            "-p".into(),
            self.ssh_port.to_string(),
            format!("{}@{}", self.user, self.host),
        ])
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const LINK: &str="home-service://connect?host=example.invalid&user=test&sshPort=22&port=7979&remotePort=7979&name=Certificates&path=%2Fprivate";
    #[test]
    fn complete_path_and_key() {
        let r = Request::parse(LINK).unwrap();
        assert_eq!(r.page_url(false), "http://127.0.0.1:7979/private");
        assert_eq!(
            r.key(),
            Request::parse(&LINK.replace("%2Fprivate", "%2Fother"))
                .unwrap()
                .key()
        );
    }
    #[test]
    fn excludes_password_command_and_legacy() {
        for field in [
            "password=secret",
            "command=whoami",
            "pathKey=legacy",
            "host=another.invalid",
        ] {
            assert!(Request::parse(&format!("{LINK}&{field}")).is_err());
        }
    }
    #[test]
    fn rejects_injection() {
        for value in ["-oProxyCommand=whoami", "host%0Aevil", "bad%20host"] {
            assert!(Request::parse(&LINK.replace("example.invalid", value)).is_err());
        }
    }
    #[test]
    fn prevents_external_page_url() {
        for path in [
            "%2F%2Fevil.invalid",
            "%2F%5Cevil",
            "%2Fok%0Abad",
            "%2Fok%23fragment",
        ] {
            assert!(Request::parse(&LINK.replace("%2Fprivate", path)).is_err());
        }
    }
    #[test]
    fn only_agent_authentication() {
        let args = Request::parse(LINK).unwrap().ssh_args();
        for opt in [
            "BatchMode=yes",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "IdentityFile=none",
            "StrictHostKeyChecking=yes",
        ] {
            assert!(args.contains(&opt.into()));
        }
    }
}
