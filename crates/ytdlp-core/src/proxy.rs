//! 代理配置：拼 URL、判断是否走直连。
//!
//! 纯逻辑、零依赖，因此可以离线单测——代理配错的表现是「连不上」，
//! 很难从现象反推原因，所以这部分必须有测试兜着。
//!
//! ## 两件实测确认的事
//!
//! 1. **yt-dlp 支持 `socks5`**（官方 onefile 自带 PySocks，实测能连上）。
//! 2. **`no_proxy` 环境变量在给了 `--proxy` 时不起作用**。
//!    实测：`--proxy http://127.0.0.1:1` 配 `no_proxy=127.0.0.1` 依然连不上。
//!    所以「不为以下项使用代理」**只能由宿主自己判断**——
//!    命中绕过列表时干脆不传 `--proxy`，而不是指望 yt-dlp 去绕。

use std::fmt;

/// 代理协议。只暴露最常用的两种：HTTP(S) 与 SOCKS5。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyProtocol {
    Http,
    /// `socks5h` 而不是 `socks5`：**DNS 也走代理**。
    /// 国内直连 DNS 会被污染，域名解析留在本地等于白配代理。
    Socks5h,
}

impl ProxyProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProxyProtocol::Http => "http",
            ProxyProtocol::Socks5h => "socks5h",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "socks5" | "socks5h" | "socks" => ProxyProtocol::Socks5h,
            _ => ProxyProtocol::Http,
        }
    }

    /// 没填端口时的惯例值。
    pub fn default_port(&self) -> u16 {
        match self {
            ProxyProtocol::Http => 80,
            ProxyProtocol::Socks5h => 1080,
        }
    }
}

/// 一份手动代理配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    pub protocol: ProxyProtocol,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    /// 绕过列表，已拆分好。
    pub bypass: Vec<String>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            protocol: ProxyProtocol::Http,
            host: String::new(),
            port: ProxyProtocol::Http.default_port(),
            user: String::new(),
            password: String::new(),
            bypass: Vec::new(),
        }
    }
}

/// 把绕过列表拆成条目。分隔符同时接受 `;`（Windows 注册表的写法）、`,` 和空白。
pub fn parse_bypass(raw: &str) -> Vec<String> {
    raw.split([';', ',', '\n', '\r', '\t', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// **内建**的绕过列表：本机与内网永远直连。
///
/// 这不是可配置项，刻意如此：
///
/// - 把 `127.0.0.1` 丢给代理**必然失败**（实测本地 HTTP 服务直接连不上），
///   而配代理本来就是为了访问外网，本机流量走代理没有任何意义；
/// - 之前把它做成一个可编辑文本框 + 一个「从系统导入」按钮，
///   结果是绝大多数人不会碰的输入框占着版面，还要顺带解释 `*` 通配与
///   `<local>` 两套语法。
///
/// 语义与 Windows 的 `ProxyOverride` 一致，所以「跟随系统代理」时
/// 可以直接把系统那份也交给同一个匹配器。
pub const LOCAL_BYPASS: &str = "localhost,127.*,10.*,192.168.*,<local>";

/// userinfo 段要按 RFC 3986 转义，否则密码里的 `@`、`:`、`/` 会把 URL 拆坏。
fn encode_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl ProxyConfig {
    /// 拼成 `--proxy` 接受的形式。主机为空时返回 `None`（等于没配）。
    pub fn url(&self) -> Option<String> {
        let host = self.host.trim();
        if host.is_empty() || self.port == 0 {
            return None;
        }
        let creds = if self.user.trim().is_empty() {
            String::new()
        } else if self.password.is_empty() {
            format!("{}@", encode_userinfo(self.user.trim()))
        } else {
            format!(
                "{}:{}@",
                encode_userinfo(self.user.trim()),
                encode_userinfo(&self.password)
            )
        };
        Some(format!(
            "{}://{creds}{host}:{}",
            self.protocol.as_str(),
            self.port
        ))
    }

    /// 这个主机是否应当直连（命中绕过列表）。
    pub fn bypassed(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return false;
        }
        self.bypass
            .iter()
            .any(|p| matches_pattern(&host, &p.to_ascii_lowercase()))
    }
}

impl fmt::Display for ProxyConfig {
    /// 用于界面提示的简短形式：`http://127.0.0.1:7897`（**不含密码**）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = self.host.trim();
        if host.is_empty() {
            return write!(f, "（未填主机名）");
        }
        let auth = if self.user.trim().is_empty() {
            ""
        } else {
            "（含身份验证）"
        };
        write!(f, "{}://{host}:{}{auth}", self.protocol.as_str(), self.port)
    }
}

/// 一条「按站点分流」规则（ROADMAP §F18）。
///
/// `pattern` 是主机名通配（复用 [`matches_pattern`]，支持 `*` / `?`，
/// 语义与 Windows 代理绕过列表一致）。`proxy` 是目标代理 URL；
/// 空串 / `"direct"` 表示**直连**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyRule {
    pub pattern: String,
    /// 目标代理 URL。`""` 或 `"direct"` = 直连。
    pub proxy: String,
}

/// 在规则表里找第一条命中 host 的规则。
///
/// 返回 `Some(proxy)`：`proxy` 为 `""`/`"direct"` 时表示「这条命中了，直连」。
/// 无命中返回 `None`（走全局代理）。
///
/// 匹配优先级 = 规则顺序（界面上排在前面的优先）。这里只做纯匹配，
/// 「直接/代理」的语义由调用方翻译。
///
/// 语义补充（区别于 [`matches_pattern`] 的 Windows 绕过语义）：分流规则里
/// `*.example.com` **也匹配裸域 `example.com`**——用户写分流规则时的直觉是
/// 「整个站」，不该因为少写一个 `*` 就漏掉裸域。
pub fn match_proxy_rule(rules: &[ProxyRule], host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    rules
        .iter()
        .find(|r| rule_matches(&host, &r.pattern))
        .map(|r| r.proxy.trim().to_string())
}

/// 一条规则是否命中 host：`*.x` 额外匹配裸域 `x`。
fn rule_matches(host: &str, pattern: &str) -> bool {
    let pattern = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
    if pattern.is_empty() {
        return false;
    }
    if matches_pattern(host, &pattern) {
        return true;
    }
    // `*.example.com` 额外匹配裸域 `example.com`
    if let Some(rest) = pattern.strip_prefix("*.") {
        if matches_pattern(host, rest) {
            return true;
        }
    }
    false
}

/// 把设置里的 `proxyRules` 解析成规则表。非法条目跳过，不让一条烂数据
/// 破坏整张表。规则格式：`{ "host": "...", "proxy": "..." }`。
pub fn parse_proxy_rules(value: &serde_json::Value) -> Vec<ProxyRule> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| {
            let pattern = v.get("host")?.as_str()?.trim().to_string();
            let proxy = v.get("proxy")?.as_str()?.trim().to_string();
            if pattern.is_empty() {
                return None;
            }
            Some(ProxyRule { pattern, proxy })
        })
        .collect()
}

/// 通配匹配：`*` 任意多字符，`?` 任意一个字符。
///
/// 语义与 Windows 的「不为以下项使用代理」一致——**`*.example.com` 只匹配子域，
/// 不匹配 `example.com` 本身**。保持一致是为了能直接粘贴系统里那份列表。
pub fn matches_pattern(host: &str, pattern: &str) -> bool {
    let pattern = pattern.trim().trim_end_matches('.');
    if pattern.is_empty() {
        return false;
    }
    if pattern == "<local>" {
        // Windows 的写法：不带点的主机名（局域网机器名）
        return !host.contains('.');
    }
    if !pattern.contains(['*', '?']) {
        return host == pattern;
    }
    wildcard_match(pattern.as_bytes(), host.as_bytes())
}

/// 经典的双指针通配匹配，O(n·m) 最坏但常数极小，且不分配。
fn wildcard_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;

    while i < s.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == s[i]) {
            p += 1;
            i += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some(p);
            mark = i;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// 从一段可能是 `http://user:pass@host:port` 的旧配置里还原出结构。
///
/// 早期版本只有一个 `proxyUrl` 输入框，升级时要把它迁移成结构化配置，
/// 不能把用户已经填好的代理丢掉。
pub fn parse_proxy_url(raw: &str) -> Option<ProxyConfig> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (scheme, rest) = match raw.split_once("://") {
        Some((s, r)) => (s, r),
        // 没写协议时按 http 处理（`127.0.0.1:7897` 是最常见的写法）
        None => ("http", raw),
    };
    // 末段可能是路径，`--proxy` 用不到，切掉
    let rest = rest.split('/').next().unwrap_or(rest);

    let (creds, hostport) = match rest.rsplit_once('@') {
        Some((c, h)) => (Some(c), h),
        None => (None, rest),
    };

    // IPv6 写成 [::1]:7897
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        match rest.split_once("]:") {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().ok()),
            None => (rest.trim_end_matches(']').to_string(), None),
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().ok()),
            None => (hostport.to_string(), None),
        }
    };

    if host.is_empty() {
        return None;
    }
    let protocol = ProxyProtocol::from_str(scheme);
    let (user, password) = match creds {
        Some(c) => match c.split_once(':') {
            Some((u, p)) => (percent_decode(u), percent_decode(p)),
            None => (percent_decode(c), String::new()),
        },
        None => (String::new(), String::new()),
    };

    Some(ProxyConfig {
        protocol,
        host,
        port: port.unwrap_or_else(|| protocol.default_port()),
        user,
        password,
        bypass: Vec::new(),
    })
}

/// 只处理我们自己转义过的那些字符，够用且不会误解码 `+`。
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ProxyConfig {
        ProxyConfig {
            protocol: ProxyProtocol::Http,
            host: "127.0.0.1".into(),
            port: 7897,
            ..Default::default()
        }
    }

    #[test]
    fn builds_plain_url() {
        assert_eq!(cfg().url().as_deref(), Some("http://127.0.0.1:7897"));
    }

    /// 空主机名 = 没配代理，不能拼出一个 `http://:0` 这种垃圾传下去。
    #[test]
    fn empty_host_yields_none() {
        let c = ProxyConfig::default();
        assert_eq!(c.url(), None);
        assert_eq!(ProxyConfig { host: "  ".into(), port: 80, ..Default::default() }.url(), None);
        assert_eq!(ProxyConfig { host: "h".into(), port: 0, ..Default::default() }.url(), None);
    }

    #[test]
    fn socks5h_is_used_for_socks() {
        let c = ProxyConfig {
            protocol: ProxyProtocol::Socks5h,
            host: "127.0.0.1".into(),
            port: 1080,
            ..Default::default()
        };
        assert_eq!(c.url().as_deref(), Some("socks5h://127.0.0.1:1080"));
    }

    /// 密码里的 `@` / `:` / `/` 不转义会把 URL 拆坏——这是最容易踩的一个。
    #[test]
    fn credentials_are_percent_encoded() {
        let c = ProxyConfig {
            host: "proxy.local".into(),
            port: 8080,
            user: "user@corp".into(),
            password: "p@ss:w/rd".into(),
            ..Default::default()
        };
        assert_eq!(
            c.url().as_deref(),
            Some("http://user%40corp:p%40ss%3Aw%2Frd@proxy.local:8080")
        );
    }

    #[test]
    fn user_without_password_still_encoded() {
        let c = ProxyConfig { user: "a b".into(), ..cfg() };
        assert_eq!(c.url().as_deref(), Some("http://a%20b@127.0.0.1:7897"));
    }

    /// 界面提示里**绝不能出现密码**。
    #[test]
    fn display_never_leaks_password() {
        let c = ProxyConfig {
            user: "u".into(),
            password: "supersecret".into(),
            ..cfg()
        };
        let s = c.to_string();
        assert!(!s.contains("supersecret"), "实际: {s}");
        assert!(s.contains("身份验证"));
    }

    #[test]
    fn parse_bypass_accepts_all_separators() {
        assert_eq!(
            parse_bypass("localhost;127.*, 192.168.*\n10.*"),
            vec!["localhost", "127.*", "192.168.*", "10.*"]
        );
        assert!(parse_bypass("  ;;  ").is_empty());
    }

    #[test]
    fn bypass_exact_and_wildcard() {
        let c = ProxyConfig {
            bypass: parse_bypass("localhost,127.*,192.168.*,*.internal"),
            ..cfg()
        };
        assert!(c.bypassed("localhost"));
        assert!(c.bypassed("127.0.0.1"));
        assert!(c.bypassed("192.168.1.20"));
        assert!(c.bypassed("a.internal"));
        assert!(c.bypassed("a.b.internal"));
        assert!(!c.bypassed("example.com"));
        assert!(!c.bypassed("10.0.0.1"));
        // `*.internal` 按 Windows 语义**不**匹配裸域
        assert!(!c.bypassed("internal"));
    }

    #[test]
    fn bypass_is_case_insensitive_and_ignores_trailing_dot() {
        let c = ProxyConfig { bypass: vec!["Example.COM".into()], ..cfg() };
        assert!(c.bypassed("example.com"));
        assert!(c.bypassed("example.com."));
        assert!(c.bypassed("EXAMPLE.COM"));
    }

    /// Windows 的 `<local>` = 不带点的主机名。
    #[test]
    fn local_token_matches_dotless_hosts() {
        let c = ProxyConfig { bypass: vec!["<local>".into()], ..cfg() };
        assert!(c.bypassed("my-nas"));
        assert!(!c.bypassed("example.com"));
    }

    #[test]
    fn empty_bypass_and_empty_host() {
        let c = cfg();
        assert!(!c.bypassed("example.com"));
        assert!(!c.bypassed(""));
        // 绕过列表里的空条目不该匹配任何东西（否则会全部直连）
        let c2 = ProxyConfig { bypass: parse_bypass(";;"), ..cfg() };
        assert!(!c2.bypassed("example.com"));
    }

    #[test]
    fn wildcard_match_edge_cases() {
        assert!(matches_pattern("abc", "a*c"));
        assert!(matches_pattern("abc", "*"));
        assert!(matches_pattern("abc", "???"));
        assert!(matches_pattern("abc", "abc*"));
        assert!(!matches_pattern("abc", "ab"));
        assert!(!matches_pattern("abc", "?"));
        assert!(!matches_pattern("abcdef", "a?d*"));
    }

    // ───────── 旧配置迁移 ─────────

    #[test]
    fn parses_legacy_proxy_urls() {
        let c = parse_proxy_url("http://127.0.0.1:7897").unwrap();
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 7897);
        assert_eq!(c.protocol, ProxyProtocol::Http);

        // 不带协议也能认（最常见的写法）
        let c = parse_proxy_url("127.0.0.1:7897").unwrap();
        assert_eq!((c.host.as_str(), c.port), ("127.0.0.1", 7897));

        // 协议大小写
        assert_eq!(parse_proxy_url("SOCKS5://h:1080").unwrap().protocol, ProxyProtocol::Socks5h);
        assert_eq!(parse_proxy_url("socks://h:1080").unwrap().protocol, ProxyProtocol::Socks5h);

        // 带认证
        let c = parse_proxy_url("http://u:p%40w@proxy.local:8080").unwrap();
        assert_eq!((c.user.as_str(), c.password.as_str()), ("u", "p@w"));
        // 转义后能拼回同一个 URL
        assert_eq!(c.url().as_deref(), Some("http://u:p%40w@proxy.local:8080"));
    }

    #[test]
    fn parses_legacy_without_port_and_defaults_by_scheme() {
        assert_eq!(parse_proxy_url("http://h").unwrap().port, 80);
        assert_eq!(parse_proxy_url("socks5://h").unwrap().port, 1080);
    }

    #[test]
    fn parses_ipv6_host() {
        let c = parse_proxy_url("http://[::1]:7897").unwrap();
        assert_eq!(c.host, "::1");
        assert_eq!(c.port, 7897);
    }

    #[test]
    fn rejects_junk_legacy_values() {
        assert!(parse_proxy_url("").is_none());
        assert!(parse_proxy_url("   ").is_none());
        assert!(parse_proxy_url("://").is_none());
        assert!(parse_proxy_url("http://:8080").is_none());
    }

    // ───────── 按站点分流（ROADMAP §F18）─────────

    #[test]
    fn match_proxy_rule_first_match_wins() {
        let rules = vec![
            ProxyRule { pattern: "*.bilibili.com".into(), proxy: "http://a:1".into() },
            ProxyRule { pattern: "bilibili.com".into(), proxy: "http://b:2".into() },
        ];
        // 第一个规则命中子域
        assert_eq!(match_proxy_rule(&rules, "www.bilibili.com").as_deref(), Some("http://a:1"));
        // 精确匹配
        assert_eq!(match_proxy_rule(&rules, "bilibili.com").as_deref(), Some("http://a:1"));
        // 无命中
        assert_eq!(match_proxy_rule(&rules, "youtube.com"), None);
    }

    #[test]
    fn match_proxy_rule_direct_means_match() {
        let rules = vec![ProxyRule { pattern: "*.corp".into(), proxy: "direct".into() }];
        // 命中「直连」也要返回 Some（区别于「未命中走全局」）
        assert_eq!(match_proxy_rule(&rules, "git.corp").as_deref(), Some("direct"));
        assert_eq!(match_proxy_rule(&rules, "other.com"), None);
    }

    #[test]
    fn match_proxy_rule_ignores_empty_host_and_case() {
        let rules = vec![ProxyRule { pattern: "Example.COM".into(), proxy: "p".into() }];
        assert_eq!(match_proxy_rule(&rules, "example.com").as_deref(), Some("p"));
        assert_eq!(match_proxy_rule(&rules, ""), None);
    }

    #[test]
    fn parse_proxy_rules_skips_bad_entries() {
        let v = serde_json::json!([
            { "host": "*.bili.com", "proxy": "http://a:1" },
            { "host": "", "proxy": "http://x" },          // 空 host，跳过
            { "host": "youtube.com" },                     // 缺 proxy，跳过
            { "not": "an object" },                        // 非对象，跳过
            { "host": "*.corp", "proxy": "direct" },
        ]);
        let rules = parse_proxy_rules(&v);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern, "*.bili.com");
        assert_eq!(rules[1].proxy, "direct");
    }
}
