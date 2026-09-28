//! 读取 Windows 的系统代理设置（「设置 → 网络和 Internet → 代理」里那一页）。
//!
//! 走 `reg query` 而不是注册表 FFI：`HKCU` 不需要管理员权限，`reg.exe` 在
//! System32 里必然存在，而且输出格式稳定、解析逻辑可以单测。
//! 为了读两个字符串引入一套 `RegOpenKeyExW`/`RegQueryValueExW` 的 unsafe 不划算。
//!
//! 对应注册表项：
//! ```text
//! HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings
//!   ProxyEnable     REG_DWORD  0/1
//!   ProxyServer     REG_SZ     127.0.0.1:7897  或  http=h:p;https=h:p;socks=h:p
//!   ProxyOverride   REG_SZ     localhost;127.*;192.168.*;<local>
//!   AutoConfigURL   REG_SZ     http://.../proxy.pac
//! ```

use std::process::Command;

/// 系统代理的快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemProxy {
    pub enabled: bool,
    /// 原始 `ProxyServer` 值。
    pub server: String,
    /// 原始 `ProxyOverride` 值（绕过列表）。
    pub bypass: String,
    /// PAC 地址。**yt-dlp 不支持 PAC**，这个字段只用于给用户一句诚实的解释。
    pub auto_config_url: String,
}

impl SystemProxy {
    /// 从 `host:port` 或 `http=h:p;https=h:p;socks=h:p` 里挑出最合适的一条。
    ///
    /// 优先级 `https` > `http` > `socks`：我们下载的都是 https 站点，
    /// 而且 `https=` 那一项指的也是「用一个 HTTP 代理去访问 https」。
    pub fn pick_server(&self) -> Option<String> {
        let raw = self.server.trim();
        if raw.is_empty() {
            return None;
        }
        if !raw.contains('=') {
            return Some(raw.to_string());
        }
        let (mut http, mut socks) = (None, None);
        for part in raw.split(';') {
            let Some((k, v)) = part.split_once('=') else {
                continue;
            };
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            match k.trim().to_ascii_lowercase().as_str() {
                "https" => return Some(v.to_string()),
                "http" => http = Some(v.to_string()),
                "socks" | "socks5" => socks = Some(v.to_string()),
                _ => {}
            }
        }
        http.or(socks)
    }

    /// 拼成 `--proxy` 能用的形式。
    ///
    /// 注册表里的 `ProxyServer` **不带协议**，得自己补 `http://`。
    pub fn proxy_url(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let server = self.pick_server()?;
        if server.contains("://") {
            Some(server)
        } else {
            Some(format!("http://{server}"))
        }
    }
}

/// `reg query` 里的 REG_DWORD 写成 `0x1`。
fn parse_dword(v: &str) -> u32 {
    let v = v.trim();
    let hex = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X"));
    match hex {
        Some(h) => u32::from_str_radix(h, 16).unwrap_or(0),
        None => v.parse().unwrap_or(0),
    }
}

/// 解析 `reg query` 的输出。
///
/// 形如（值之间是多个空格，REG_SZ 的值本身可能含空格）：
/// ```text
///     ProxyEnable    REG_DWORD    0x1
///     ProxyServer    REG_SZ    127.0.0.1:7897
/// ```
pub fn parse_reg_output(out: &str) -> SystemProxy {
    let mut p = SystemProxy::default();
    for line in out.lines() {
        let line = line.trim();
        let Some((name, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let rest = rest.trim_start();
        // 跳过类型列（REG_DWORD / REG_SZ / …），剩下的是值。
        // ⚠️ 值为空时整行就是 `AutoConfigURL    REG_SZ`（类型后面没东西），
        // 这时 split_once 返回 None，**不能**把类型名本身当成值。
        let value = match rest.split_once(char::is_whitespace) {
            Some((kind, v)) if kind.starts_with("REG_") => v.trim(),
            None if rest.starts_with("REG_") => "",
            _ => rest,
        };
        match name {
            "ProxyEnable" => p.enabled = parse_dword(value) != 0,
            "ProxyServer" => p.server = value.to_string(),
            "ProxyOverride" => p.bypass = value.to_string(),
            "AutoConfigURL" => p.auto_config_url = value.to_string(),
            _ => {}
        }
    }
    p
}

/// 读一次系统代理。非 Windows 或读取失败时返回全空（等于「没配」）。
pub fn current() -> SystemProxy {
    #[cfg(not(windows))]
    {
        SystemProxy::default()
    }
    #[cfg(windows)]
    {
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
        let out = Command::new("reg")
            .args(["query", key])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        match out {
            Some(s) => parse_reg_output(&s),
            None => SystemProxy::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Internet Settings
    ProxyEnable    REG_DWORD    0x1
    ProxyServer    REG_SZ    127.0.0.1:7897
    ProxyOverride    REG_SZ    localhost;127.*;192.168.*;<local>
    AutoConfigURL    REG_SZ
    MigrateProxy    REG_DWORD    0x1
"#;

    #[test]
    fn parses_typical_output() {
        let p = parse_reg_output(SAMPLE);
        assert!(p.enabled);
        assert_eq!(p.server, "127.0.0.1:7897");
        assert_eq!(p.bypass, "localhost;127.*;192.168.*;<local>");
        assert_eq!(p.auto_config_url, "");
    }

    #[test]
    fn proxy_enable_zero_means_disabled() {
        let p = parse_reg_output("    ProxyEnable    REG_DWORD    0x0\n    ProxyServer    REG_SZ    h:1");
        assert!(!p.enabled);
        // 关掉了就不该再给出代理
        assert_eq!(p.proxy_url(), None);
    }

    #[test]
    fn missing_entries_are_empty_not_panic() {
        let p = parse_reg_output("");
        assert_eq!(p, SystemProxy::default());
        assert_eq!(p.proxy_url(), None);
    }

    /// 注册表里的 ProxyServer **不带协议**，必须补上 http://
    #[test]
    fn adds_scheme_when_missing() {
        let p = SystemProxy {
            enabled: true,
            server: "127.0.0.1:7897".into(),
            ..Default::default()
        };
        assert_eq!(p.proxy_url().as_deref(), Some("http://127.0.0.1:7897"));
    }

    #[test]
    fn keeps_scheme_when_present() {
        let p = SystemProxy {
            enabled: true,
            server: "socks5://127.0.0.1:1080".into(),
            ..Default::default()
        };
        assert_eq!(p.proxy_url().as_deref(), Some("socks5://127.0.0.1:1080"));
    }

    /// 按协议分开写时优先 https，其次 http，最后 socks。
    #[test]
    fn picks_per_protocol_server() {
        let mk = |s: &str| SystemProxy {
            enabled: true,
            server: s.into(),
            ..Default::default()
        };
        assert_eq!(
            mk("http=web:80;https=sec:443;ftp=ft:21").pick_server().as_deref(),
            Some("sec:443")
        );
        assert_eq!(
            mk("http=web:80;ftp=ft:21").pick_server().as_deref(),
            Some("web:80")
        );
        assert_eq!(mk("socks=sk:1080").pick_server().as_deref(), Some("sk:1080"));
        assert_eq!(mk("ftp=ft:21").pick_server(), None);
        assert_eq!(mk("  ").pick_server(), None);
    }

    /// `0x1` / `0x0` / 十进制都要认。
    #[test]
    fn parses_proxy_enable_variants() {
        assert_eq!(parse_dword("0x1"), 1);
        assert_eq!(parse_dword("0x0"), 0);
        assert_eq!(parse_dword("1"), 1);
        assert_eq!(parse_dword("0"), 0);
        assert_eq!(parse_dword("junk"), 0);
    }

    /// 值里带空格时不能只取到第一个词。
    #[test]
    fn keeps_spaces_inside_value() {
        let p = parse_reg_output("    ProxyOverride    REG_SZ    a b;c d\n");
        assert_eq!(p.bypass, "a b;c d");
    }
}
