//! 设置页用到的查询与诊断命令：Cookie、外部程序版本与更新、代理测试、格式预设等。

use crate::state::AppState;
use crate::{clipboard, cookies, ffmpeg, logfile, net, paths, runner, sysproxy, update};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};


/// 读一次 Windows 的系统代理设置，供设置页展示。
///
/// 只是**展示**：`跟随系统代理` 模式在每次探测/下载时会重新读，
/// 免得用户改了系统代理还得回来点一下。
#[tauri::command(async)]
pub fn system_proxy() -> Value {
    let p = sysproxy::current();
    json!({
        "enabled": p.enabled,
        "server": p.server,
        "bypass": p.bypass,
        "autoConfigUrl": p.auto_config_url,
        "resolved": p.proxy_url(),
        // yt-dlp **不支持 PAC**，检测到就得如实告诉用户，别让他以为配了就能用
        "usesPac": !p.auto_config_url.trim().is_empty(),
    })
}

/// 探测可用格式。
///
/// 返回**解析后的** `MediaInfo`（camelCase，且已滤掉 storyboard 之类的非媒体条目），
/// 而不是原始 JSON——否则前端要再写一份 snake_case → camelCase 的字段映射，
/// 两条解析路径必然分叉（DESIGN §3 的老问题）。
#[tauri::command]
pub async fn probe_formats(
    state: State<'_, AppState>,
    url: String,
) -> Result<ytdlp_core::MediaInfo, String> {
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or(Value::Null);
    let Some(exe) = paths::resolve_ytdlp() else {
        return Err("未找到 yt-dlp 可执行文件".into());
    };
    // Cookie 与 JS 运行时**必须在探测阶段就生效**：
    // 很多站点不登录连元数据都拿不到（DESIGN §6），
    // 而 YouTube 没有 JS 运行时就会返回降级结果。
    let proxy = runner::proxy_for(&settings, &url);
    let cookie = runner::cookies_of(&settings);
    let js = runner::js_of(&settings);

    // 这里只为了拿格式表，不需要预估大小（`None` = 不传 `-f`）。
    // 表达式不可满足时会让整个探测失败，这条路径没必要冒那个险。
    let args = ytdlp_core::build_probe_args(
        &url,
        proxy.as_deref(),
        cookie.as_ref(),
        false,
        &js,
        None,
        runner::impersonate_of(&settings),
    );
    let out = tokio::process::Command::new(exe)
        .args(&args)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .output()
        .await
        .map_err(|e| e.to_string())?;

    // `--dump-single-json` 的 JSON 走 stdout，是 UTF-8
    // （控制台乱码那件事只影响 stderr 的人读文本，见 HANDOFF §3.7）。
    ytdlp_core::parse_info_json(&String::from_utf8_lossy(&out.stdout))
        .map_err(|e| format!("解析探测结果失败：{e}"))
}

// ─────────────────────── Cookie（DESIGN §6）───────────────────────
//
// 多 profile 而不是单个输入框：用户必然有多套身份（B站账号 A / YouTube 账号 B）。
// 列表接口**不返回 cookie 内容**，只返回元数据。

#[tauri::command(async)]
pub fn list_cookie_profiles() -> Vec<cookies::CookieProfile> {
    cookies::list()
}

/// 导入一份 cookies.txt。**先校验格式再落盘**，格式错误带行号返回。
#[tauri::command(async)]
pub fn import_cookie_profile(
    name: String,
    content: String,
    origin: String,
) -> Result<cookies::CookieProfile, String> {
    cookies::save(&name, &content, &origin)
}

#[tauri::command(async)]
pub fn delete_cookie_profile(id: String) -> Result<(), String> {
    cookies::remove(&id)
}

/// 导入前预览：读一份外部 cookies.txt 并校验，返回 cookie 条数。
#[tauri::command(async)]
pub fn inspect_cookie_file(path: String) -> Result<usize, String> {
    let content = std::fs::read_to_string(&path).map_err(|e| format!("读取失败：{e}"))?;
    ytdlp_core::validate_netscape(&content).map_err(|e| e.message())
}

/// 预检已导入的 profile 是否仍可用（文件可能被外部删掉或改坏）。
#[tauri::command(async)]
pub fn test_cookie_profile(id: String) -> Value {
    let p = cookies::check_profile(&id);
    json!({ "ok": p.is_ok(), "summary": p.summary() })
}

/// 预检「从浏览器读取」。会真的跑一次 yt-dlp 并分类失败原因。
#[tauri::command]
pub async fn test_cookie_browser(browser: String) -> Value {
    // 先校验浏览器名——错误信息里能列出所有可选项，比让 yt-dlp 报
    // "unsupported browser" 有用得多。profile 名无从枚举，交给 yt-dlp。
    if let Err(e) = ytdlp_core::cookies::validate_browser_spec(&browser) {
        return json!({ "ok": false, "summary": format!("✘ {e}") });
    }
    let p = cookies::check_browser(&browser).await;
    json!({ "ok": p.is_ok(), "summary": p.summary() })
}

/// 枚举本机可用的浏览器及其 profile。
///
/// 界面上不再写死三个浏览器：本机可能装了别的，而且同一浏览器常有多个 profile
/// （工作与个人各一份，登录态不同）。检测用的路径与 yt-dlp 一致，
/// 保证「选得到的」就是「读得到的」。
#[tauri::command(async)]
pub fn list_browsers() -> Vec<cookies::BrowserChoice> {
    cookies::list_browsers()
}

// ─────────────────── yt-dlp 自更新（DESIGN §8）───────────────────
//
// 三条更新链路里唯一不需要重新发版宿主的那条。
// ureq 是阻塞的，用 spawn_blocking 包起来，别堵住主线程。

#[tauri::command]
pub async fn check_ytdlp_update(state: State<'_, AppState>) -> Result<Value, String> {
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or(Value::Null);
    let proxy = runner::proxy_of(&settings);
    let info = tauri::async_runtime::spawn_blocking(move || update::check(proxy.as_deref()))
        .await
        .map_err(|e| e.to_string())??;
    serde_json::to_value(info).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn apply_ytdlp_update(state: State<'_, AppState>) -> Result<String, String> {
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or(Value::Null);
    let proxy = runner::proxy_of(&settings);
    tauri::async_runtime::spawn_blocking(move || update::apply(proxy.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

/// 代理连通性检查（DESIGN §7）。
///
/// 必须真的发一次请求：只校验地址格式的话，用户配了个没启动的代理
/// 依然会看到「格式正确」，然后在任务失败时一头雾水。
///
/// 返回 `Result` 是硬性要求：async 命令只要含引用型输入（这里是 `State<'_, _>`），
/// Tauri 就要求返回值是 `Result`。
#[tauri::command]
pub async fn test_proxy(state: State<'_, AppState>) -> Result<Value, String> {
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or(Value::Null);
    let Some(proxy) = runner::proxy_of(&settings) else {
        return Ok(json!({
            "ok": false,
            "summary": match settings.get("proxyMode").and_then(|v| v.as_str()) {
                Some("system") => "系统没有启用代理（注册表里 ProxyEnable 为 0）。".to_string(),
                _ => "还没填代理地址。选「手动配置」并填写主机名与端口。".to_string(),
            }
        }));
    };
    let shown = proxy.clone();
    let res = tauri::async_runtime::spawn_blocking(move || net::check_proxy(&proxy))
        .await
        .map_err(|e| e.to_string())?;
    Ok(match res {
        Ok(summary) => json!({ "ok": true, "summary": summary }),
        // 代理串里可能含密码，报错信息里必须换成脱敏的那份
        Err(msg) => json!({ "ok": false, "summary": msg.replace(&shown, &runner::redact_proxy(&shown)) }),
    })
}

/// 检测本机可用的 JS 运行时。
///
/// **不是可选优化**：实测同一份 yt-dlp、同一个链接、同样的 cookie 与代理，
/// 只差 `--js-runtimes node` 就是「需要重载页面」与「成功」的区别。
/// yt-dlp 不会自动启用已安装的运行时。
#[tauri::command(async)]
pub fn detect_js_runtimes() -> Value {
    let candidates: Vec<Value> = paths::detect_js_runtimes_detailed()
        .into_iter()
        .map(|(name, path)| json!({ "name": name, "path": path }))
        .collect();
    json!({
        "detected": paths::detect_js_runtimes(),
        "candidates": candidates,
    })
}

/// 「优先选择」的编码候选，供设置页生成选择项。
///
/// **唯一真源是核心层的白名单**。让前端自己写一份选项列表，迟早会与后端
/// 接受的值分叉，而分叉的后果是静默退化（白名单外的值会被当成「不指定」），
/// 界面上完全看不出来。
#[tauri::command]
pub fn codec_choices() -> Value {
    let conv = |list: &[(&str, &str)]| {
        list.iter()
            .map(|(value, label)| json!({ "value": value, "label": label }))
            .collect::<Vec<_>>()
    };
    json!({
        "video": conv(ytdlp_core::VIDEO_CODEC_CHOICES),
        "audio": conv(ytdlp_core::AUDIO_CODEC_CHOICES),
    })
}

/// 格式选择器里的预设列表。
///
/// 表达式由核心层按**当前编码偏好**生成，所以从选择器里点「1080p」不会把
/// 设置里的编码偏好丢掉。这份列表原先硬编码在 `FormatPicker.vue` 里，
/// 等于把 `-f` 表达式抄了两份——DESIGN §3 反复强调过不要这样。
#[tauri::command]
pub fn format_presets(state: State<AppState>) -> Value {
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or(Value::Null);
    let codec = runner::codec_of(&settings);
    let item = |label: &str, note: &str, preset: ytdlp_core::Preset| {
        json!({
            "label": label,
            "note": note,
            "expr": ytdlp_core::preset_expression_with(&preset, &codec),
        })
    };
    json!([
        item("最佳画质", "自动挑最优视频轨与音频轨", ytdlp_core::Preset::Best),
        item("1080p", "不超过 1920×1080", ytdlp_core::Preset::MaxHeight(1080)),
        item("720p", "省流量", ytdlp_core::Preset::MaxHeight(720)),
        item(
            "仅音频 MP3",
            "提取音频并转码",
            ytdlp_core::Preset::AudioOnly(ytdlp_core::AudioFormat::Mp3),
        ),
    ])
}

/// 把「当前编辑中的设置」翻译成实际会传给 `-f` 的表达式。
///
/// 设置页要显示这一行，但又不能在前端重写一遍表达式拼装——`-f` 是**硬契约**
/// （存进数据库用于重放）。这里直接复用 `preset_of` + `codec_of`，
/// 也就是下载路径用的同一组读取器，所以预览不可能与实际命令分叉。
#[tauri::command]
pub fn preview_format_expression(settings: Value) -> String {
    ytdlp_core::preset_expression_with(
        &runner::preset_of(&settings),
        &runner::codec_of(&settings),
    )
}

/// 读系统剪贴板的纯文本，供右键菜单的「粘贴」用。
///
/// 前端做不了这件事：`navigator.clipboard.readText()` 在 WebView2 里会卡在
/// 权限弹窗上，`execCommand('paste')` 恒为 false。详见 `clipboard` 模块。
#[tauri::command(async)]
pub fn read_clipboard() -> Result<String, String> {
    clipboard::read_text()
}

/// 当前数据目录（设置、历史、cookie 都在这儿）+ 是否便携模式。
///
/// 用户最常问的就是「我的历史存哪了」，而 `%APPDATA%` 在资源管理器里
/// 默认是隐藏的——所以直接把路径摆到设置页上。
#[tauri::command]
pub fn data_dir() -> Value {
    let (root, portable) = paths::data_dir_info();
    json!({
        "root": root.to_string_lossy(),
        "portable": portable,
        // 便携模式的标记文件名，界面照着它提示用户
        "marker": paths::PORTABLE_MARKER,
        // 日志路径（`<数据目录>\logs\app.log`），出问题时用户要能直接找到它
        "log": logfile::path().map(|p| p.to_string_lossy().into_owned()),
    })
}

/// aria2c 可执行文件信息，供设置页展示。
///
/// 用户需要知道**当前用的是哪一份**：随包的出厂副本、还是 PATH 上那份旧的。
/// 两者行为可能不同（版本、连接数限制），出问题时这是第一个要看的信息。
#[tauri::command(async)]
pub fn aria2c_info() -> Value {
    let (path, version) = paths::aria2c_status();
    json!({
        "found": path.is_some(),
        "path": path,
        "version": version,
    })
}

/// ffmpeg 可执行文件信息，供设置页展示与主界面的「缺少 ffmpeg」提示。
#[tauri::command(async)]
pub fn ffmpeg_info() -> Value {
    let (path, version) = paths::ffmpeg_status();
    json!({
        "found": path.is_some(),
        "path": path,
        "version": version,
    })
}

/// 一键下载 ffmpeg 到 `<AppData>/bin/`。下载进度经 `ffmpeg://progress` 推给前端。
#[tauri::command]
pub async fn install_ffmpeg(app: AppHandle) -> Result<String, String> {
    let settings = app
        .state::<AppState>()
        .settings
        .lock()
        .map(|s| s.clone())
        .unwrap_or(Value::Null);
    let proxy = runner::proxy_of(&settings);
    tauri::async_runtime::spawn_blocking(move || {
        ffmpeg::install(proxy.as_deref(), |done, total| {
            let _ = app.emit("ffmpeg://progress", json!({ "done": done, "total": total }));
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// yt-dlp 可执行文件信息，供设置页展示与「检查更新」使用。
#[tauri::command]
pub async fn ytdlp_info(state: State<'_, AppState>) -> Result<Value, String> {
    let Some(exe) = paths::resolve_ytdlp() else {
        return Ok(json!({ "found": false }));
    };
    let out = tokio::process::Command::new(&exe)
        .arg("--version")
        .output()
        .await
        .map_err(|e| e.to_string())?;
    let version = crate::text::decode_console(&out.stdout).trim().to_string();
    Ok(json!({
        "found": true,
        "path": exe.to_string_lossy(),
        "version": version,
        // 替换前必须用它验证新文件：损坏的 exe 会让所有任务同时失败（DESIGN §8）。
        "versionLooksValid": ytdlp_core::looks_like_version(&version),
        // 升级副本目录里有什么（含上次更新留下的 .old），便于排查
        "appDataBin": update::installed_files()
            .iter()
            .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        "archivePath": state.settings.lock().ok()
            .and_then(|s| s.get("archivePath").and_then(|v| v.as_str()).map(str::to_string))
            .unwrap_or_default(),
    }))
}
