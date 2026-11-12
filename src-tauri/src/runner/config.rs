//! 从设置 JSON 读出下载要用的参数：格式预设、代理、Cookie、JS 运行时、编码偏好。

use crate::paths;
use serde_json::Value;
use ytdlp_core::args::{
    AudioFormat, CodecPreference, Container, CookieSource, DownloadSpec, EmbedOptions,
    JsRuntimeOptions, Preset,
};

/// 从设置里取格式预设。
///
/// 抽出来是因为「恢复默认格式」也要用同一个解析——两处各写一遍必然分叉。
pub fn preset_of(settings: &Value) -> Preset {
    let s = |k: &str| settings.get(k).and_then(|v| v.as_str()).map(str::to_string);
    match s("preset").as_deref() {
        Some("maxHeight") => Preset::MaxHeight(
            settings
                .get("maxHeight")
                .and_then(|v| v.as_u64())
                .unwrap_or(1080) as u32,
        ),
        Some("audioOnly") => Preset::AudioOnly(match s("audioFormat").as_deref() {
            Some("m4a") => AudioFormat::M4a,
            Some("opus") => AudioFormat::Opus,
            Some("flac") => AudioFormat::Flac,
            Some("wav") => AudioFormat::Wav,
            _ => AudioFormat::Mp3,
        }),
        Some("subsOnly") => Preset::SubsOnly,
        Some("thumbnailOnly") => Preset::ThumbnailOnly,
        _ => Preset::Best,
    }
}

/// 从设置 JSON 构造下载规格。前端是设置结构的真源，这里只按需抽取。
pub fn spec_from_settings(settings: &Value, url: &str, task_id: &str) -> DownloadSpec {
    let s = |k: &str| settings.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let b = |k: &str| settings.get(k).and_then(|v| v.as_bool()).unwrap_or(false);

    let output_dir = s("outputDir").unwrap_or_else(|| ".".into());
    let temp_dir = paths::task_temp_dir_in(&paths::temp_root(settings), task_id);

    let preset = preset_of(settings);

    let container = match s("container").as_deref() {
        Some("mp4") => Container::Mp4,
        Some("mkv") => Container::Mkv,
        Some("webm") => Container::Webm,
        _ => Container::Auto,
    };

    let e = settings.get("embed").cloned().unwrap_or(Value::Null);
    let eb = |k: &str| e.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let embed = EmbedOptions {
        subs: eb("subs"),
        sub_langs: e
            .get("subLangs")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        auto_subs: eb("autoSubs"),
        keep_sub_files: eb("keepSubFiles"),
        thumbnail: eb("thumbnail"),
        keep_thumbnail_file: eb("keepThumbnailFile"),
        metadata: eb("metadata"),
        chapters: eb("chapters"),
        info_json: eb("infoJson"),
    };

    // Cookie / 代理 / JS 运行时的取法在探测与下载两处必须一致，统一走这一组 helper。
    let cookies = cookies_of(settings);
    // 代理要带上 URL：绕过列表是**按主机名**判断的（见 `proxy_for` 的说明）。
    let proxy = proxy_for(settings, url);
    let js = js_of(settings);

    let mut spec = DownloadSpec::new(url, task_id, output_dir, temp_dir);
    spec.preset = preset;
    spec.container = container;
    spec.embed = embed;
    spec.cookies = cookies;
    spec.proxy = proxy;
    spec.aria2c = b("aria2c");
    spec.limit_rate = s("limitRate").filter(|r| !r.trim().is_empty());
    if let Some(t) = s("filenameTemplate").filter(|t| !t.trim().is_empty()) {
        spec.filename_template = t;
    }
    if b("archiveEnabled") {
        spec.archive = Some(
            s("archivePath")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(paths::archive_file),
        );
    }
    // JS 运行时：不给的话 YouTube 拿不到真实格式（见 JsRuntimeOptions 的说明）
    spec.js = js;
    // 编码偏好：只影响预设表达式（见 CodecPreference 的说明）
    spec.codec = codec_of(settings);
    // 过 Cloudflare 拦截（默认关，见 DownloadSpec::impersonate）
    spec.impersonate = impersonate_of(settings);
    // 章节切分（ROADMAP §F15）
    spec.split_chapters = b("splitChapters");
    spec.sponsorblock = ytdlp_core::SponsorBlock::from_setting(s("sponsorblock").as_deref().unwrap_or(""));
    // 下载后执行（ROADMAP §F16）。用户显式填的命令，宿主侧已做过安全确认。
    spec.exec = s("execCommand").filter(|c| !c.trim().is_empty());
    spec
}

/// 从设置里取出代理配置（不管绕过列表）。
///
/// `proxyMode` 三种取值：
/// - `none`    不使用代理
/// - `system`  跟随 Windows 系统代理（读注册表）
/// - `manual`  手动填写的 host/port/协议/认证
///
/// 兼容旧配置：早期只有一个 `proxyUrl` 字符串，这里会在 `proxyMode` 缺失时
/// 尝试把它解析出来，不让用户已经填好的代理丢掉。
pub fn proxy_config_of(settings: &Value) -> Option<ytdlp_core::ProxyConfig> {
    use ytdlp_core::{ProxyConfig, ProxyProtocol};

    let mode = settings
        .get("proxyMode")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mode = match mode {
        Some(m) => m.to_string(),
        None => {
            // ── 旧配置迁移 ──
            let enabled = settings.get("proxyEnabled").and_then(|v| v.as_bool()).unwrap_or(false);
            if !enabled {
                return None;
            }
            "manual".to_string()
        }
    };

    match mode.as_str() {
        "manual" => {
            let s = |k: &str| settings.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let host = s("proxyHost");
            let port = settings
                .get("proxyPort")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u16;
            let protocol = ProxyProtocol::parse_lossy(&s("proxyProtocol"));

            // 结构化字段还空着但旧字段有值 → 用旧字段兜底
            let (host, port, protocol, user, password) = if host.trim().is_empty() {
                match settings
                    .get("proxyUrl")
                    .and_then(|v| v.as_str())
                    .and_then(ytdlp_core::parse_proxy_url)
                {
                    Some(old) => (old.host, old.port, old.protocol, old.user, old.password),
                    None => (host, port, protocol, String::new(), String::new()),
                }
            } else {
                (
                    host,
                    port,
                    protocol,
                    s("proxyUser"),
                    s("proxyPassword"),
                )
            };

            Some(ProxyConfig {
                protocol,
                host,
                port: if port == 0 { protocol.default_port() } else { port },
                user,
                password,
                // 内建的本机 / 内网直连，**不读设置**（见 `LOCAL_BYPASS` 的说明）
                bypass: ytdlp_core::parse_bypass(ytdlp_core::proxy::LOCAL_BYPASS),
            })
        }
        "system" => {
            let sp = crate::sysproxy::current();
            let url = sp.proxy_url()?;
            let mut cfg = ytdlp_core::parse_proxy_url(&url)?;
            // 系统的绕过列表也一并尊重——用户既然选「跟随系统」，就该跟完整。
            // 它为空时回落到内建的本机/内网直连，免得把自己也代理出去。
            cfg.bypass = if sp.bypass.trim().is_empty() {
                ytdlp_core::parse_bypass(ytdlp_core::proxy::LOCAL_BYPASS)
            } else {
                ytdlp_core::parse_bypass(&sp.bypass)
            };
            Some(cfg)
        }
        _ => None,
    }
}

/// 本次请求**实际**要用的代理。`None` = 直连。
///
/// 优先级（ROADMAP §F18）：
/// 1. **按站点分流规则**（`proxyRules`）——命中 `host → direct` 直连，
///    命中 `host → proxy` 用指定代理；
/// 2. 全局代理 + 绕过列表（`proxy_for` 原有逻辑）。
///
/// 绕过列表在**宿主侧**判断，命中就干脆不传 `--proxy`。
/// ⚠️ 不能指望 yt-dlp 自己绕：实测在给了 `--proxy` 的情况下，
/// `no_proxy` 环境变量**不起作用**（死代理 + `no_proxy=127.0.0.1` 依然连不上）。
pub fn proxy_for(settings: &Value, url: &str) -> Option<String> {
    let host = ytdlp_core::host_of(url);

    // ① 分流规则优先：界面上排前面的规则先匹配。
    if !host.is_empty() {
        if let Some(rules) = settings.get("proxyRules") {
            let rules = ytdlp_core::parse_proxy_rules(rules);
            if let Some(proxy) = ytdlp_core::match_proxy_rule(&rules, &host) {
                // 空串 / "direct" 表示「这条规则命中，直连」。
                if proxy.is_empty() || proxy.eq_ignore_ascii_case("direct") {
                    return None;
                }
                return Some(proxy);
            }
        }
    }

    // ② 全局代理 + 绕过列表。
    let cfg = proxy_config_of(settings)?;
    if !host.is_empty() && cfg.bypassed(&host) {
        return None;
    }
    cfg.url()
}

/// 仅供「测试连通性」用：忽略绕过列表，直接测配置里那个代理。
pub fn proxy_of(settings: &Value) -> Option<String> {
    proxy_config_of(settings).and_then(|c| c.url())
}

/// 把代理串里的密码换成 `***`，用于任何可能展示给用户的文本。
///
/// `http://user:pass@host:port` 这种串会出现在错误信息里，
/// 不打码就等于把密码写进界面。
pub fn redact_proxy(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    match rest.split_once('@') {
        Some((creds, host)) => {
            let user = creds.split(':').next().unwrap_or("");
            format!("{scheme}://{user}:***@{host}")
        }
        None => url.to_string(),
    }
}

/// 「优先选择」的编码，见 `CodecPreference`。
///
/// ⚠️ 走 `CodecPreference::sanitized` 的白名单，**不能直接把设置里的字符串拼进
/// `-f`**：实测任何非法过滤器都会让 yt-dlp 抛 `SyntaxError` 并打印 Python
/// traceback（见 `CodecPreference` 的说明）。白名单外的值一律当「不指定」。
pub fn codec_of(settings: &Value) -> CodecPreference {
    let g = |k: &str| {
        settings
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    CodecPreference::sanitized(&g("preferVcodec"), &g("preferAcodec"))
}

/// 是否对 generic 提取器开启指纹模拟（过 Cloudflare 拦截）。
///
/// 探测与下载两处**必须一致**：只有一边开的话，探测能拿到元数据、下载却 403
/// （或反过来），报的错会完全指不到原因。
pub fn impersonate_of(settings: &Value) -> bool {
    settings
        .get("impersonate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// 一份「强制打开指纹模拟」的设置副本。
///
/// 撞上 Cloudflare 时用它重试一次——**不改用户自己的设置**，
/// 只影响这一次调用。
pub(crate) fn settings_with_impersonate(settings: &Value) -> Value {
    let mut v = settings.clone();
    if let Some(o) = v.as_object_mut() {
        o.insert("impersonate".into(), Value::Bool(true));
    }
    v
}

/// 从设置里取 JS 运行时配置。///
/// 设置为空时**自动检测**：用户不该为了一个「不给就下不了 YouTube」的必需参数
/// 去手工配置。见 `JsRuntimeOptions` 的说明。
pub fn js_of(settings: &Value) -> JsRuntimeOptions {
    let override_ = settings
        .get("jsRuntime")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    let runtimes: Vec<String> = if override_.is_empty() {
        crate::paths::detect_js_runtimes()
    } else {
        override_
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    // 界面**不再暴露**这个开关（官方 exe 自带组件，实测它只会让一次探测多花 40 秒），
    // 但保留读取：需要的人可以在 config.json 里写 `"jsRemoteComponents": true`。
    // 它不在 `default_settings` 里，所以设置页不会因此多出一个勾选框。
    let remote = settings
        .get("jsRemoteComponents")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    JsRuntimeOptions::new(runtimes, remote)
}

/// 从设置里取 cookie 来源。
///
/// 三种模式：
/// - `file`    直接用某个 cookies.txt 路径
/// - `profile` 用已导入的 profile（多账号场景，DESIGN §6）
/// - `browser` 从浏览器读取（实测 Windows 上只有 Firefox 可用）
pub fn cookies_of(settings: &Value) -> Option<CookieSource> {
    let s = |k: &str| settings.get(k).and_then(|v| v.as_str());
    match s("cookieMode") {
        Some("file") => s("cookieFile")
            .filter(|p| !p.trim().is_empty())
            .map(|p| CookieSource::File(std::path::PathBuf::from(p))),
        // profile 存的是 id，落盘位置由宿主决定，用户不用关心路径
        Some("profile") => s("cookieProfileId")
            .filter(|id| !id.trim().is_empty())
            .and_then(crate::cookies::path_of)
            .map(CookieSource::File),
        Some("browser") => s("cookieBrowser").map(|b| CookieSource::Browser(b.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    /// 强制模拟只改这一个键，用户其他设置原样不动。
    #[test]
    fn forced_impersonate_only_flips_that_key() {
        let s = json!({ "impersonate": false, "proxyMode": "manual", "proxyHost": "127.0.0.1" });
        let forced = settings_with_impersonate(&s);
        assert_eq!(forced["impersonate"], json!(true));
        assert_eq!(forced["proxyMode"], json!("manual"));
        assert_eq!(forced["proxyHost"], json!("127.0.0.1"));
        // 原对象不能被改（它是从 state 里 clone 出来的，但别依赖这点）
        assert_eq!(s["impersonate"], json!(false));
        // 原本就是 true 也不受影响
        assert_eq!(settings_with_impersonate(&json!({ "impersonate": true }))["impersonate"], json!(true));
    }

    #[test]
    fn manual_mode_builds_url() {
        let s = json!({
            "proxyMode": "manual",
            "proxyProtocol": "http",
            "proxyHost": "127.0.0.1",
            "proxyPort": 7897,
        });
        assert_eq!(proxy_of(&s).as_deref(), Some("http://127.0.0.1:7897"));
    }

    #[test]
    fn socks_protocol_maps_to_socks5h() {
        let s = json!({ "proxyMode": "manual", "proxyProtocol": "socks5",
                        "proxyHost": "h", "proxyPort": 1080 });
        assert_eq!(proxy_of(&s).as_deref(), Some("socks5h://h:1080"));
    }

    #[test]
    fn none_mode_yields_no_proxy() {
        let s = json!({ "proxyMode": "none", "proxyHost": "h", "proxyPort": 1 });
        assert_eq!(proxy_of(&s), None);
        assert_eq!(proxy_for(&s, "https://x.com/a"), None);
    }

    // ───────── 按站点分流（ROADMAP §F18）─────────

    #[test]
    fn proxy_rule_overrides_global() {
        let s = json!({
            "proxyMode": "manual", "proxyHost": "127.0.0.1", "proxyPort": 7897,
            "proxyRules": [
                { "host": "*.bilibili.com", "proxy": "socks5://10.0.0.1:1080" },
            ],
        });
        // 命中分流规则 → 用规则代理，而不是全局代理
        assert_eq!(
            proxy_for(&s, "https://www.bilibili.com/video/BV1").as_deref(),
            Some("socks5://10.0.0.1:1080")
        );
        // 裸域也命中（*.bilibili.com 额外匹配裸域）
        assert_eq!(
            proxy_for(&s, "https://bilibili.com/video/BV1").as_deref(),
            Some("socks5://10.0.0.1:1080")
        );
        // 未命中的站点走全局代理
        assert_eq!(
            proxy_for(&s, "https://www.youtube.com/watch?v=x").as_deref(),
            Some("http://127.0.0.1:7897")
        );
    }

    #[test]
    fn proxy_rule_direct_bypasses_even_with_global() {
        let s = json!({
            "proxyMode": "manual", "proxyHost": "127.0.0.1", "proxyPort": 7897,
            "proxyRules": [
                { "host": "*.corp", "proxy": "direct" },
            ],
        });
        assert_eq!(proxy_for(&s, "https://git.corp/repo"), None);
        // 其他站点照常走全局
        assert_eq!(
            proxy_for(&s, "https://example.com/x").as_deref(),
            Some("http://127.0.0.1:7897")
        );
    }

    #[test]
    fn proxy_rule_ignored_when_no_rules() {
        let s = json!({ "proxyMode": "manual", "proxyHost": "h", "proxyPort": 1 });
        // 没有 proxyRules 键时行为不变
        assert_eq!(proxy_for(&s, "https://x.com/a").as_deref(), Some("http://h:1"));
    }

    /// 空主机名 / 端口 0 时的行为。
    #[test]
    fn blank_host_or_zero_port_is_none() {
        let s = json!({ "proxyMode": "manual", "proxyHost": "  ", "proxyPort": 7897 });
        assert_eq!(proxy_of(&s), None);
        let s = json!({ "proxyMode": "manual", "proxyHost": "h", "proxyPort": 0 });
        // 端口 0 回落到协议默认值，而不是当没配。
        assert_eq!(proxy_of(&s).as_deref(), Some("http://h:80"));
    }

    /// **这是绕过列表的关键**：命中就返回 None（直连），而不是把 proxy 传下去。
    /// 实测 yt-dlp 在给了 --proxy 时不会理 no_proxy。
    ///
    /// 名单是内建常量 `proxy::LOCAL_BYPASS`。这里**故意塞一个 `proxyBypass` 键**
    /// 来固定「老配置残留的键不再有任何作用」——它既不能扩大也不能收窄名单。
    #[test]
    fn bypass_makes_request_go_direct() {
        let s = json!({
            "proxyMode": "manual", "proxyHost": "127.0.0.1", "proxyPort": 7897,
            "proxyBypass": "example.com",
        });
        assert_eq!(proxy_for(&s, "http://127.0.0.1:8797/clip.mp4"), None);
        assert_eq!(proxy_for(&s, "http://192.168.1.9/x"), None);
        assert_eq!(proxy_for(&s, "http://localhost:8797/clip.mp4"), None);
        // `example.com` 曾被写进那个废弃键里，现在不生效：照常走代理。
        assert_eq!(
            proxy_for(&s, "https://example.com/x").as_deref(),
            Some("http://127.0.0.1:7897")
        );
        // 外部站点照常走代理。
        assert_eq!(
            proxy_for(&s, "https://www.youtube.com/watch?v=x").as_deref(),
            Some("http://127.0.0.1:7897")
        );
    }

    #[test]
    fn url_without_host_is_not_bypassed() {
        let s = json!({ "proxyMode": "manual", "proxyHost": "h", "proxyPort": 1 });
        // host 解析不出来时不该被内建名单命中而变成直连。
        assert_eq!(proxy_of(&s).as_deref(), Some("http://h:1"));
    }

    // ───────── 旧配置（proxyEnabled + proxyUrl）─────────

    #[test]
    fn legacy_enabled_url_migrates() {
        let s = json!({ "proxyEnabled": true, "proxyUrl": "http://127.0.0.1:7897" });
        assert_eq!(proxy_of(&s).as_deref(), Some("http://127.0.0.1:7897"));
    }

    #[test]
    fn legacy_disabled_means_no_proxy() {
        let s = json!({ "proxyEnabled": false, "proxyUrl": "http://127.0.0.1:7897" });
        assert_eq!(proxy_of(&s), None);
    }

    #[test]
    fn legacy_url_with_credentials_migrates() {
        let s = json!({ "proxyEnabled": true, "proxyUrl": "http://u:p%40w@h:8080" });
        assert_eq!(proxy_of(&s).as_deref(), Some("http://u:p%40w@h:8080"));
    }

    /// 代理串里的密码不能出现在任何给用户看的文本里。
    #[test]
    fn redact_hides_password() {
        assert_eq!(redact_proxy("http://u:secret@h:8080"), "http://u:***@h:8080");
        assert_eq!(redact_proxy("http://h:8080"), "http://h:8080");
        assert_eq!(redact_proxy("garbage"), "garbage");
    }
}
