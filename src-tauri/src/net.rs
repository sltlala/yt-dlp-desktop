//! 共享的 HTTP 客户端与代理连通性检查。
//!
//! 抽出来是为了让「自更新」和「代理测试」用**同一套**代理处理逻辑——
//! 两处各写一份的话，很容易出现「测试说代理没问题、更新却连不上」。

use std::time::Duration;

/// 构造带可选代理的 agent。
pub fn agent(proxy: Option<&str>) -> Result<ureq::Agent, String> {
    let mut b = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(120))
        // GitHub 要求带 UA，否则会 403
        .user_agent(concat!("ytdlp-desktop/", env!("CARGO_PKG_VERSION")));
    if let Some(p) = proxy.filter(|p| !p.trim().is_empty()) {
        let parsed = ureq::Proxy::new(p).map_err(|e| format!("代理地址无效（{p}）：{e}"))?;
        b = b.proxy(parsed);
    }
    Ok(b.build())
}

/// 把网络错误翻译成可操作的提示。
///
/// 实测：本机直连 `github.com` 超时，必须走代理。不给这层提示的话，
/// 用户只会看到一句 `Connection Failed` 然后无从下手。
pub fn explain_net_err(msg: &str, proxy: Option<&str>) -> String {
    if proxy.is_none() {
        format!(
            "无法连接（{msg}）。\n\
             如果你在中国大陆，请在设置里配置代理后再试——\
             直连境外站点通常会被阻断。"
        )
    } else {
        format!("无法连接（{msg}）。请检查代理是否可用。")
    }
}

/// 代理连通性检查用的目标。
///
/// `generate_204` 只回一个 204、正文为空，是最轻的连通性探针；
/// 而且 gstatic 在国内需要代理才能访问，所以「能通」是有意义的结论。
const PROBE_URL: &str = "https://www.gstatic.com/generate_204";

/// 用一次真实的 HTTP 请求验证代理是否可用。
///
/// **直接走 HTTP 而不是启动 yt-dlp**：更快（省掉 ~1s 冷启动），
/// 而且结论无歧义——请求成功就是代理通，失败就是代理不通。
pub fn check_proxy(proxy: &str) -> Result<String, String> {
    let proxy = proxy.trim();
    if proxy.is_empty() {
        return Err("代理地址为空".into());
    }

    let agent = agent(Some(proxy))?;
    let started = std::time::Instant::now();
    match agent.get(PROBE_URL).call() {
        Ok(_) => {
            let ms = started.elapsed().as_millis();
            Ok(format!("✔ 代理可用（{proxy}，{ms} ms）"))
        }
        Err(ureq::Error::Status(code, _)) => Err(format!(
            "✘ 代理有响应但目标站点返回 HTTP {code}。代理本身是通的，\
             可能是目标站点不可达。"
        )),
        Err(e) => Err(format!(
            "✘ 无法通过该代理连接（{}）。请检查代理是否正在运行、\
             地址与端口是否正确。",
            e
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_agent_with_and_without_proxy() {
        assert!(agent(None).is_ok());
        assert!(agent(Some("http://127.0.0.1:7897")).is_ok());
        assert!(agent(Some("socks5://127.0.0.1:1080")).is_ok());
        // 空白串按「无代理」处理，不该报错
        assert!(agent(Some("   ")).is_ok());
    }

    #[test]
    fn empty_proxy_is_rejected_by_check() {
        let err = check_proxy("").unwrap_err();
        assert!(err.contains("为空"), "实际: {err}");
        assert!(check_proxy("   ").unwrap_err().contains("为空"));
    }

    /// 无代理与有代理的措辞必须不同：前者要教用户去配代理。
    #[test]
    fn error_wording_differs_by_proxy_presence() {
        let none = explain_net_err("Connection Failed", None);
        let some = explain_net_err("Connection Failed", Some("http://x:1"));
        assert!(none.contains("配置代理"));
        assert!(some.contains("检查代理"));
        assert_ne!(none, some);
    }

    /// 指向一个必定无人监听的端口，必须给出可读错误而不是 panic。
    #[test]
    fn unreachable_proxy_fails_gracefully() {
        let err = check_proxy("http://127.0.0.1:9").unwrap_err();
        assert!(err.starts_with("✘"), "实际: {err}");
    }
}
