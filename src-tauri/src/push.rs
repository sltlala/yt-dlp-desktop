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
//! 引框架的依赖树大一个数量级。协议是固定的，手写几十行 TCP 解析足够。
//!
//! ⚠️ 只监听回环地址**不等于**只有扩展能访问：任何网页都能对 `127.0.0.1`
//! 发一个不触发预检的 POST（`text/plain`），或借 DNS 重绑定让请求看起来同源。
//! 所以必须校验 `Origin`（网页无法伪造）与 `Host`，见 [`check_request`]。

use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 固定端口。扩展里写死同一个端口；被占用时启动失败并记日志，
/// 不影响主程序（扩展推送只是增强功能，不是下载主链路）。
pub const PUSH_PORT: u16 = 19090;

/// 请求头 + 正文的上限。一条 URL 用不了这么多，超出直接拒绝。
const MAX_REQUEST: usize = 64 * 1024;

/// 读完一条请求的时限。防止半开连接一直占着任务。
const READ_TIMEOUT: Duration = Duration::from_secs(5);

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

#[derive(Debug, PartialEq)]
struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, PartialEq)]
enum ReadError {
    TooLarge,
    Malformed,
}

/// 头部结束位置（`\r\n\r\n` 之后的下标）。
fn header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// 解析一条完整的请求。正文不够 `Content-Length` 时返回 `Ok(None)`，表示还要继续读。
fn parse_request(buf: &[u8]) -> Result<Option<Request>, ReadError> {
    let Some(end) = header_end(buf) else {
        return if buf.len() > MAX_REQUEST { Err(ReadError::TooLarge) } else { Ok(None) };
    };
    let head = std::str::from_utf8(&buf[..end]).map_err(|_| ReadError::Malformed)?;
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or("").split_whitespace();
    let method = parts.next().ok_or(ReadError::Malformed)?.to_string();
    let path = parts.next().ok_or(ReadError::Malformed)?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();

    let len = match headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")) {
        Some((_, v)) => v.parse::<usize>().map_err(|_| ReadError::Malformed)?,
        None => 0,
    };
    if end + len > MAX_REQUEST {
        return Err(ReadError::TooLarge);
    }
    if buf.len() < end + len {
        return Ok(None);
    }
    let body = String::from_utf8_lossy(&buf[end..end + len]).into_owned();
    Ok(Some(Request { method, path, headers, body }))
}

/// 按 `Content-Length` 读完整条请求：TCP 不保证一次 `read` 就拿到全部字节。
async fn read_request(sock: &mut tokio::net::TcpStream) -> Result<Request, ReadError> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        let n = match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => return Err(ReadError::Malformed),
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(req) = parse_request(&buf)? {
            return Ok(req);
        }
    }
}

/// 来源校验。返回 `Err(原因)` 表示拒绝。
///
/// - `Host` 必须是回环地址：挡住 DNS 重绑定（恶意域名解析到 127.0.0.1）。
/// - 带 `Origin` 时只认浏览器扩展；网页发起的请求一定带网页自己的 Origin，伪造不了。
/// - 不带 `Origin` 的是本机脚本 / curl 之类，它们本来就能直接操作本机，放行。
fn check_request(req: &Request) -> Result<(), &'static str> {
    let host = req.header("host").unwrap_or("");
    let hostname = host.rsplit_once(':').map_or(host, |(h, _)| h);
    if !matches!(hostname, "127.0.0.1" | "localhost" | "[::1]") {
        return Err("Host 不是本机地址");
    }
    match req.header("origin") {
        None => Ok(()),
        Some(o) if is_extension_origin(o) => Ok(()),
        Some(_) => Err("只接受浏览器扩展的请求"),
    }
}

fn is_extension_origin(origin: &str) -> bool {
    ["chrome-extension://", "moz-extension://", "safari-web-extension://"]
        .iter()
        .any(|p| origin.starts_with(p))
}

/// CORS 头：只回显扩展来源，不再用 `*`。
fn cors_headers(req: &Request) -> String {
    match req.header("origin").filter(|o| is_extension_origin(o)) {
        Some(o) => format!(
            "Access-Control-Allow-Origin: {o}\r\n\
             Vary: Origin\r\n\
             Access-Control-Allow-Methods: POST, OPTIONS\r\n\
             Access-Control-Allow-Headers: Content-Type\r\n"
        ),
        None => String::new(),
    }
}

async fn respond(
    sock: &mut tokio::net::TcpStream,
    status: &str,
    cors: &str,
    body: &str,
) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {status}\r\n{cors}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(resp.as_bytes()).await
}

fn error_body(msg: &str) -> String {
    format!("{{\"ok\":false,\"error\":{}}}", serde_json::to_string(msg).unwrap())
}

/// 处理一条连接：读请求 → 校验来源 → 解析 `POST /add` → 提取 URL → 回调。
async fn handle_conn(
    sock: &mut tokio::net::TcpStream,
    on_url: Arc<dyn Fn(String) + Send + Sync>,
) -> std::io::Result<()> {
    let req = match tokio::time::timeout(READ_TIMEOUT, read_request(sock)).await {
        Ok(Ok(r)) => r,
        Ok(Err(ReadError::TooLarge)) => {
            return respond(sock, "413 Payload Too Large", "", &error_body("请求太大")).await;
        }
        Ok(Err(ReadError::Malformed)) | Err(_) => return Ok(()),
    };
    let cors = cors_headers(&req);

    if let Err(why) = check_request(&req) {
        crate::logfile::warn(format!(
            "浏览器推送：拒绝请求（{why}），Origin={:?}",
            req.header("origin")
        ));
        return respond(sock, "403 Forbidden", "", &error_body(why)).await;
    }

    // 浏览器扩展跨源 POST 会先发 OPTIONS 预检，必须放行。
    if req.method.eq_ignore_ascii_case("OPTIONS") {
        let resp = format!("HTTP/1.1 204 No Content\r\n{cors}Content-Length: 0\r\n\r\n");
        return sock.write_all(resp.as_bytes()).await;
    }

    if req.method.eq_ignore_ascii_case("POST") && req.path == "/add" {
        return match extract_url(&req.body) {
            Some(u) if is_pushable(&u) => {
                on_url(u.clone());
                let body = format!("{{\"ok\":true,\"url\":{}}}", serde_json::to_string(&u).unwrap());
                respond(sock, "200 OK", &cors, &body).await
            }
            _ => respond(sock, "400 Bad Request", &cors, &error_body("无效链接")).await,
        };
    }

    respond(sock, "404 Not Found", &cors, &error_body("not found")).await
}

/// 浏览器给出的链接一定带协议头；`is_valid_url` 为了接住手打的短链刻意放宽，这里收紧。
fn is_pushable(u: &str) -> bool {
    let lower = u.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")) && ytdlp_core::is_valid_url(u)
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

    fn req(headers: &[(&str, &str)]) -> Request {
        Request {
            method: "POST".into(),
            path: "/add".into(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: String::new(),
        }
    }

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

    #[test]
    fn only_web_links_are_pushable() {
        assert!(is_pushable("https://www.youtube.com/watch?v=x"));
        assert!(is_pushable("HTTP://example.com/v"));
        assert!(!is_pushable("not-a-url"));
        assert!(!is_pushable("file:///C:/Windows/system32"));
        assert!(!is_pushable("magnet:?xt=urn:btih:abc"));
    }

    #[test]
    fn waits_for_full_body() {
        let head = "POST /add HTTP/1.1\r\nHost: 127.0.0.1:19090\r\nContent-Length: 10\r\n\r\n";
        assert_eq!(parse_request(head.as_bytes()), Ok(None));
        assert_eq!(parse_request(format!("{head}hello").as_bytes()), Ok(None));
        let full = parse_request(format!("{head}helloworld").as_bytes()).unwrap().unwrap();
        assert_eq!(full.body, "helloworld");
        assert_eq!(full.path, "/add");
    }

    #[test]
    fn waits_for_headers_split_across_reads() {
        assert_eq!(parse_request(b"POST /add HTTP/1.1\r\nHost: 127.0"), Ok(None));
    }

    #[test]
    fn rejects_oversized_requests() {
        let head = format!("POST /add HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_REQUEST);
        assert_eq!(parse_request(head.as_bytes()), Err(ReadError::TooLarge));
        assert_eq!(parse_request(&vec![b'a'; MAX_REQUEST + 1]), Err(ReadError::TooLarge));
    }

    #[test]
    fn rejects_bad_content_length() {
        let head = "POST /add HTTP/1.1\r\nContent-Length: abc\r\n\r\n";
        assert_eq!(parse_request(head.as_bytes()), Err(ReadError::Malformed));
    }

    #[test]
    fn accepts_extension_and_local_tools() {
        let host = ("Host", "127.0.0.1:19090");
        assert!(check_request(&req(&[host, ("Origin", "chrome-extension://abcdef")])).is_ok());
        assert!(check_request(&req(&[host, ("Origin", "moz-extension://1234")])).is_ok());
        assert!(check_request(&req(&[host])).is_ok());
        assert!(check_request(&req(&[("host", "localhost:19090")])).is_ok());
    }

    #[test]
    fn rejects_web_pages() {
        let host = ("Host", "127.0.0.1:19090");
        assert!(check_request(&req(&[host, ("Origin", "https://evil.example")])).is_err());
        assert!(check_request(&req(&[host, ("Origin", "null")])).is_err());
    }

    #[test]
    fn rejects_dns_rebinding() {
        assert!(check_request(&req(&[("Host", "evil.example:19090")])).is_err());
        assert!(check_request(&req(&[])).is_err());
    }

    #[test]
    fn cors_only_echoes_extension_origins() {
        let ext = req(&[("Origin", "chrome-extension://abc")]);
        assert!(cors_headers(&ext).contains("Allow-Origin: chrome-extension://abc"));
        let web = req(&[("Origin", "https://evil.example")]);
        assert_eq!(cors_headers(&web), "");
    }
}
