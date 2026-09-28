//! URL 相关的纯函数。
//!
//! 刻意不引入 `url` crate：yt-dlp 接受的链接里常有不规范的写法，
//! 我们只需要一个**尽量不 panic、永远返回可用字符串**的主机名提取，
//! 用于「每域名并发限制」（DESIGN §4.2）。

/// 提取主机名，用于同站点限流。
///
/// 规则：去掉 scheme、userinfo、端口、`www.` 前缀，转小写。
/// 解析不出来时返回 `"unknown"`——**调用方不需要处理 None**。
///
/// ```
/// # use ytdlp_core::url::host_of;
/// assert_eq!(host_of("https://www.bilibili.com/video/BV1xx"), "bilibili.com");
/// assert_eq!(host_of("http://user:pw@127.0.0.1:8080/a"), "127.0.0.1");
/// assert_eq!(host_of("不是链接"), "unknown");
/// ```
pub fn host_of(url: &str) -> String {
    let after_scheme = match url.split_once("://") {
        Some((_, rest)) => rest,
        // 没有 scheme 时，若含 `:` 且不像端口，多半是别的写法，仍尝试解析
        None => url,
    };
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    // 去掉 user:pass@
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    // 去掉端口（IPv6 的 [::1]:80 形式这里不追求完备，够用即可）
    let host = host_port.split(':').next().unwrap_or_default();
    let host = host.trim_start_matches("www.").trim();

    // 必须**看起来像主机名**：只允许 ASCII 字母数字、点、连字符、下划线。
    // 排除了「不是链接」这类中文输入被原样当成主机名。
    // IDN 域名在 URL 里通常是 punycode（xn--…），所以这条约束够用；
    // 万一漏判，代价只是几个不同站点共用一个限流桶（更保守，不会更激进）。
    let valid = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && host.chars().any(|c| c.is_ascii_alphanumeric());
    if !valid {
        return "unknown".into();
    }
    host.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_common_forms() {
        assert_eq!(host_of("https://www.bilibili.com/video/BV1xx"), "bilibili.com");
        assert_eq!(host_of("https://youtube.com/watch?v=1"), "youtube.com");
        assert_eq!(host_of("http://example.com"), "example.com");
        assert_eq!(host_of("https://example.com:8443/a/b"), "example.com");
        assert_eq!(host_of("http://user:pw@example.com/a"), "example.com");
        assert_eq!(host_of("https://example.com?q=1"), "example.com");
        assert_eq!(host_of("https://example.com#frag"), "example.com");
    }

    /// 同一站点的不同写法必须归一到同一个 key，否则限流形同虚设。
    #[test]
    fn normalizes_equivalent_urls() {
        let a = host_of("https://www.bilibili.com/video/1");
        let b = host_of("http://bilibili.com/video/2");
        let c = host_of("https://BILIBILI.COM/video/3");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn different_hosts_differ() {
        assert_ne!(host_of("https://a.com/x"), host_of("https://b.com/x"));
        assert_ne!(
            host_of("https://youtube.com/x"),
            host_of("https://youtu.be/x")
        );
    }

    #[test]
    fn falls_back_to_unknown() {
        assert_eq!(host_of(""), "unknown");
        assert_eq!(host_of("不是链接"), "unknown");
        assert_eq!(host_of("   "), "unknown");
        assert_eq!(host_of("https://"), "unknown");
        assert_eq!(host_of("https:///path"), "unknown");
        assert_eq!(host_of("随便写点什么 带空格"), "unknown");
    }

    /// 端口不要被误当成主机的一部分。
    #[test]
    fn strips_port_not_host() {
        assert_eq!(host_of("https://example.com:443/a"), "example.com");
        // 冒号后没内容也要能处理
        assert_eq!(host_of("https://example.com:/a"), "example.com");
    }

    /// punycode 的 IDN 域名应被接受（而不是一律 unknown）。
    #[test]
    fn accepts_punycode_idn() {
        assert_eq!(host_of("https://xn--fiqs8s.example/x"), "xn--fiqs8s.example");
    }

    #[test]
    fn handles_ip_and_localhost() {
        assert_eq!(host_of("http://127.0.0.1:8794/a.mp4"), "127.0.0.1");
        assert_eq!(host_of("http://localhost/x"), "localhost");
    }
}
