//! 浏览器扩展一键推送（ROADMAP §F19）。
//!
//! 浏览器扩展用 `fetch("http://127.0.0.1:19090/add", { method: "POST", body: url })`
//! 把当前页 URL 推给本机应用。这里起一个**只监听回环地址**的极简 HTTP 服务，
//! 收到后交给宿主建任务、进探测池。
//!
//! 为什么不用 deep-link / Tauri 插件：那两条路都要注册协议或装系统级 handler，
//! 而浏览器扩展发一个本地 HTTP 请求是**跨平台零配置**的最小方案。
//!
//! 为什么手写 HTTP 而不是引一个 web 框架：只需要解析一个 `POST /add`，
//! 引框架的依赖树大一个数量级。协议是固定的，手写十几行 TCP 解析足够。

use std::sync::Arc;
use tauri::AppHandle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 固定端口。扩展里写死同一个端口；被占用时启动失败并记日志，
/// 不影响主程序（扩展推送只是增强功能，不是下载主链路）。
pub const PUSH_PORT: u16 = 19090;

/// 后台启动推送接收服务。`on_url` 是「收到合法 URL」后的回调。
///
/// 永不返回（内部死循环），要丢进 `tauri::async_runtime::spawn` 里跑。
pub async fn serve(on_url: Arc<dyn Fn(String) + Send + Sync>) {
    let listener = match TcpListener::bind(("127.0.0.1", PUSH_PORT)).await {
        Ok(l) => l,
        Err(e) => {
            crate::logfile::error(format!("浏览器推送服务启动失败（端口 {PUSH_PORT}）：{e}"));
            return;
        }
    };
    crate::logfile::info(format!("浏览器推送服务已监听 127.0.0.1:{PUSH_PORT}"));

    loop {
        let Ok((mut sock, _)) = listener.accept().await else {
            continue;
        };
        let on_url = Arc::clone(&on_url);
        // 每个连接独立处理，简单请求互不阻塞。
        tauri::async_runtime::spawn(async move {
            let _ = handle_conn(&mut sock, on_url).await;
        });
    }
}

/// 处理一条连接：读请求 → 解析 `POST /add` → 提取 URL → 回调 → 回 CORS 响应。
async fn handle_conn(
    sock: &mut tokio::net::TcpStream,
    on_url: Arc<dyn Fn(String) + Send + Sync>,
) -> std::io::Result<()> {
    let mut buf = [0u8; 8192];
    let n = sock.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]).into_owned();

    // 只要请求行是 `POST /add` 就处理；其余路径回 404。
    let mut lines = req.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    // 浏览器扩展跨源 POST 会先发 OPTIONS 预检，必须放行。
    let cors = "Access-Control-Allow-Origin: *\r\n\
                Access-Control-Allow-Methods: POST, OPTIONS\r\n\
                Access-Control-Allow-Headers: Content-Type\r\n";

    if method.eq_ignore_ascii_case("OPTIONS") {
        let resp = format!("HTTP/1.1 204 No Content\r\n{cors}Content-Length: 0\r\n\r\n");
        let _ = sock.write_all(resp.as_bytes()).await;
        return Ok(());
    }

    if method.eq_ignore_ascii_case("POST") && path == "/add" {
        // body 在空行之后。body 可能是纯文本 URL，或 JSON {"url": "..."}。
        let body = req.split("\r\n\r\n").nth(1).unwrap_or("").trim().to_string();
        let url = extract_url(&body);

        match url {
            Some(u) if ytdlp_core::is_valid_url(&u) => {
                on_url(u.clone());
                let body = format!("{{\"ok\":true,\"url\":{}}}", serde_json::to_string(&u).unwrap());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\n{cors}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
            _ => {
                let msg = "{\"ok\":false,\"error\":\"无效链接\"}";
                let resp = format!(
                    "HTTP/1.1 400 Bad Request\r\n{cors}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    msg.len(),
                    msg
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        }
        return Ok(());
    }

    let msg = "not found";
    let resp = format!(
        "HTTP/1.1 404 Not Found\r\n{cors}Content-Length: {}\r\n\r\n{}",
        msg.len(),
        msg
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    Ok(())
}

/// 从 body 里提取 URL：优先按 JSON 的 `url` 字段，否则把整段文本当 URL。
fn extract_url(body: &str) -> Option<String> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    // 尝试 JSON
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
            return Some(u.trim().to_string());
        }
    }
    // 否则整段文本就是 URL（去掉可能的引号）
    let u = body.trim().trim_matches('"').trim();
    if u.is_empty() {
        None
    } else {
        Some(u.to_string())
    }
}

/// 在 setup 里启动服务，收到 URL 后走与 `add_url` 相同的链路。
pub fn spawn(app: AppHandle) {
    let handle = app.clone();
    let on_url = Arc::new(move |url: String| {
        let _ = crate::push_url(&handle, &url);
    });
    tauri::async_runtime::spawn(async move {
        serve(on_url).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_json_url_field() {
        assert_eq!(
            extract_url(r#"{"url":"https://example.com/v"}"#).as_deref(),
            Some("https://example.com/v")
        );
    }

    #[test]
    fn extracts_plain_text_url() {
        assert_eq!(
            extract_url("https://example.com/v").as_deref(),
            Some("https://example.com/v")
        );
        // 带引号包裹的裸 URL
        assert_eq!(
            extract_url("\"https://example.com/v\"").as_deref(),
            Some("https://example.com/v")
        );
    }

    #[test]
    fn rejects_empty_body() {
        assert_eq!(extract_url(""), None);
        assert_eq!(extract_url("   "), None);
    }

    #[test]
    fn rejects_non_url_json() {
        // 合法 JSON 但没有 url 字段，整段当文本 URL —— is_valid_url 会在上层拦掉
        assert_eq!(extract_url(r#"{"foo":1}"#).as_deref(), Some(r#"{"foo":1}"#));
    }
}
