//! 任务执行器：启动 yt-dlp 子进程、流式解析输出、推进状态机。
//!
//!
//! ## 为什么不用 Tauri 的 `shell().sidecar()`
//!
//! 不是因为路径解析（`sidecar()` 其实接受绝对路径），而是因为
//! `CommandChild::kill()` 底层是 `SharedChild::kill()`：**只杀直接子进程、不递归**。
//! 而 ffmpeg 是 yt-dlp 的子进程：取消时若不连带杀掉它，残留的 ffmpeg 会占住
//! 输出文件句柄，下次续传必然失败（DESIGN §2.3 / §5.5）。
//!
//! 这里统一用 `tokio::process::Command` + 显式路径 + 进程树终止。
use crate::paths;
use crate::state::{AppState, Progress, Task};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use ytdlp_core::args::{
    AudioFormat, CodecPreference, Container, CookieSource, DownloadSpec, EmbedOptions,
    JsRuntimeOptions, Preset,
};
use ytdlp_core::{parse_line, Event, SkipReason};

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
    spec
}

fn emit(app: &AppHandle, tasks: &[Task]) {
    let _ = app.emit("task://update", tasks);
}

fn refresh(app: &AppHandle) {
    let state = app.state::<AppState>();
    let snap = state.snapshot();
    emit(app, &snap);
}

/// 更新单个任务并广播。
///
/// `with_task` 会自动把该任务标记为「待写库」，所以这里不必手动 mark——
/// 这正是把写入口收窄成三个方法的目的。
fn update<F: FnOnce(&mut Task)>(app: &AppHandle, id: &str, f: F) {
    app.state::<AppState>().with_task(id, f);
    refresh(app);
}

/// 追加一条告警（去重）并广播。
///
/// 告警是任务详情里唯一能看到「出了什么事」的地方；同一条重复堆叠只会淹掉别的。
fn push_warning(app: &AppHandle, id: &str, msg: impl Into<String>) {
    let msg = msg.into();
    update(app, id, |t| {
        if !t.warnings.contains(&msg) {
            t.warnings.push(msg);
        }
    });
}

/// 终止进程树。
///
/// ffmpeg 是 yt-dlp 的子进程，只杀父进程会留下它占着输出文件句柄，
/// 导致下次续传失败（DESIGN §5.5）。
fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        // 负的 pid 表示进程组；spawn 时已经用 process_group(0) 建组了
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

/// 落定 aria2c 的可用性，并把它所在目录交给调用方去改 PATH。
///
/// **为什么不在参数里写路径**（实测，见 DESIGN §11.7）：
/// `--downloader <存在的绝对路径>` 会被 yt-dlp **静默忽略**——不报错，
/// 直接回落到内置下载器。只有裸名 `aria2c` 才会真正调用。所以这里返回
/// 目录，由调用方前置到子进程的 `PATH`。
///
/// 找不到可用 aria2c 时**就地关掉**它并给任务挂一条告警：
/// yt-dlp 在找不到 aria2c 时同样是静默回落的，不主动说一声，
/// 用户会以为自己在用多线程下载，其实一直用的是内置的。
fn prepare_aria2c(app: &AppHandle, task_id: &str, spec: &mut DownloadSpec) -> Option<PathBuf> {
    if !spec.aria2c {
        return None;
    }
    match crate::paths::resolve_aria2c() {
        Some(p) => p.parent().map(PathBuf::from),
        None => {
            spec.aria2c = false;
            push_warning(
                app,
                task_id,
                "未找到可用的 aria2c，本次改用内置下载器（速度可能慢一些）",
            );
            None
        }
    }
}

/// 运行一个下载任务。返回最终状态字符串。
pub async fn run_download(app: AppHandle, task_id: String) {
    let (url, playlist_items, format_override) = {
        let state = app.state::<AppState>();
        let found = state.tasks.lock().ok().and_then(|t| {
            t.iter()
                .find(|x| x.id == task_id)
                .map(|x| (x.url.clone(), x.playlist_items.clone(), x.format_override.clone()))
        });
        match found {
            Some(v) => v,
            None => return,
        }
    };

    let mut spec = {
        let state = app.state::<AppState>();
        let settings = state.settings.lock().map(|s| s.clone()).unwrap_or(Value::Null);
        let mut spec = spec_from_settings(&settings, &url, &task_id);
        // 播放列表选集与格式覆盖都是**任务级**的，不在设置里。
        spec.playlist_items = playlist_items;
        spec.format_override = format_override;
        spec
    };
    let Some(exe) = paths::resolve_ytdlp() else {
        update(&app, &task_id, |t| {
            t.state = "failed".into();
            t.error = Some("未找到 yt-dlp 可执行文件".into());
        });
        return;
    };

    // temp 目录必须存在，且**每次派发前截断 filepath.txt**：
    // --print-to-file 是 append 模式，不截断会读到上一次的旧路径（HANDOFF §3.3）。
    let _ = std::fs::create_dir_all(&spec.temp_dir);
    let _ = std::fs::remove_file(spec.filepath_file());

    // aria2c 的落点必须在拼参数**之前**确定（见 DESIGN §11.7）。
    let aria2c_dir = prepare_aria2c(&app, &task_id, &mut spec);

    let args = ytdlp_core::build_download_args(&spec);

    update(&app, &task_id, |t| {
        t.state = "downloading".into();
        t.error = None;
    });

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        // 非 ASCII 必炸：中文标题在 GBK 控制台会被打乱（HANDOFF §3.7）
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1");

    // 把自带 aria2c 所在目录**前置**到子进程 PATH：
    // yt-dlp 的 `--downloader` 传绝对路径会被**静默忽略**（见 §11.7），
    // 只能传裸名靠 PATH，所以用改 PATH 的方式保证命中的是我们自带那份。
    if let Some(dir) = aria2c_dir {
        let old = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{};{old}", dir.display()));
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            update(&app, &task_id, |t| {
                t.state = "failed".into();
                t.error = Some(format!("无法启动 yt-dlp：{e}"));
            });
            return;
        }
    };

    let pid = child.id().unwrap_or(0);
    let cancel = Arc::new(AtomicBool::new(false));
    app.state::<AppState>().set_running(
        task_id.clone(),
        crate::state::RunningProc {
            pid,
            cancel: cancel.clone(),
        },
    );

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut handles = Vec::new();
    if let Some(out) = stdout {
        let app2 = app.clone();
        let id = task_id.clone();
        handles.push(tokio::spawn(async move {
            stream_lines(app2, id, out).await;
        }));
    }
    if let Some(err) = stderr {
        let app2 = app.clone();
        let id = task_id.clone();
        handles.push(tokio::spawn(async move {
            stream_lines(app2, id, err).await;
        }));
    }
    for h in handles {
        let _ = h.await;
    }

    let status = child.wait().await;
    app.state::<AppState>().clear_running(&task_id);

    let canceled = cancel.load(Ordering::SeqCst);
    let code = status.as_ref().ok().and_then(|s| s.code()).unwrap_or(-1);

    // --print-to-file 取最后一行（append 模式，播放列表会累积多行）。
    let filepath = std::fs::read_to_string(spec.filepath_file())
        .ok()
        .and_then(|c| ytdlp_core::parse_filepath_file(&c));

    update(&app, &task_id, |t| {
        t.post_process = None;
        t.progress.speed = None;
        t.progress.eta = Some(0);
        if let Some(p) = filepath {
            t.filepath = Some(p);
        }
        // 实际大小**以磁盘为准**。
        //
        // ⚠️ 不能图省事复用进度里的 `total`：那是下载前的**预估**，
        // 而合并音视频轨、嵌入字幕/缩略图之后成品会变大，两者经常对不上。
        let actual = t
            .filepath
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len());
        t.size_actual = actual;
        if canceled {
            t.state = "canceled".into();
        } else if code == 0 {
            // 注意：exit=0 也可能是「已跳过」（归档命中 / 文件已存在）。
            // 那种情况已在流式解析里被标成 skipped，不要覆盖（DESIGN §13.1）。
            if t.state != "skipped" {
                t.state = "completed".into();
                t.finished_at = Some(crate::state::now_ms());
            }
        } else {
            t.state = "failed".into();
            if t.error.is_none() {
                t.error = Some(format!("yt-dlp 退出码 {code}"));
            }
        }
    });
}

/// 逐行读取子进程输出。
///
/// **不能用 `BufReader::lines()`**：它要求每行都是合法 UTF-8，遇到不合法就
/// 返回 `Err`。而 yt-dlp 的控制台输出按系统代码页（本机 GBK）编码，
/// 一条含中文路径的 `[Merger] Merging formats into "D:\...\中文.mp4"` 就会
/// 让循环终止——**该任务之后所有进度与后处理事件全部丢失**。
///
/// 这里改成按字节读到 `\n`，再交给 `text::decode_console` 解码。
async fn stream_lines<R>(app: AppHandle, task_id: String, reader: R)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        let line = crate::text::decode_line(&buf);
        if line.is_empty() {
            continue;
        }
        if let Some(ev) = parse_line(&line) {
            handle_event(&app, &task_id, ev);
        }
    }
}

fn handle_event(app: &AppHandle, task_id: &str, ev: Event) {
    match ev {
        Event::Progress(p) => apply_progress(app, task_id, p, false),
        // 用 aria2c 时 yt-dlp 一条进度都不发（实测 0 条），进度只能来自这里（DESIGN §11.2）
        Event::Aria2cProgress(p) => apply_progress(app, task_id, p, true),
        Event::PostProcess(pp) => {
            let key = pp.key.clone();
            let msg = pp.message.clone();
            update(app, task_id, |t| {
                t.state = "postprocessing".into();
                t.post_process = Some(key.clone());
                // 嵌入失败时 yt-dlp 只警告不报错，必须留下痕迹（DESIGN §14.3）
                if msg.contains("cannot be properly embedded")
                    || msg.contains("cannot be embedded")
                {
                    t.warnings.push(msg.clone());
                }
            });
        }
        Event::Destination(_) => { /* temp 路径，最终路径以 filepath.txt 为准 */ }
        Event::Skipped(reason) => {
            let label = match reason {
                SkipReason::Archive => "archive",
                SkipReason::FileExists => "fileExists",
            };
            update(app, task_id, |t| {
                t.state = "skipped".into();
                t.skip_reason = Some(label.to_string());
            });
        }
        Event::Error(msg) => {
            update(app, task_id, |t| {
                t.error = Some(msg);
            });
        }
        Event::Warning(msg) => {
            update(app, task_id, |t| {
                if t.warnings.len() < 20 {
                    t.warnings.push(msg);
                }
            });
        }
    }
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
            let protocol = ProxyProtocol::from_str(&s("proxyProtocol"));

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
/// 绕过列表在**宿主侧**判断，命中就干脆不传 `--proxy`。
/// ⚠️ 不能指望 yt-dlp 自己绕：实测在给了 `--proxy` 的情况下，
/// `no_proxy` 环境变量**不起作用**（死代理 + `no_proxy=127.0.0.1` 依然连不上）。
pub fn proxy_for(settings: &Value, url: &str) -> Option<String> {
    let cfg = proxy_config_of(settings)?;
    let host = ytdlp_core::host_of(url);
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

/// 从设置里取「优先选择」的编码。
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

/// 从设置里取 JS 运行时配置。
///
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

/// 探测元数据。
///
/// 探测与下载是**两个独立并发池**（DESIGN §4），由 `scheduler` 分别调度。
/// Cookie 与代理在此阶段就必须生效——很多站点不登录连元数据都拿不到。
pub async fn run_probe(app: AppHandle, task_id: String) {
    let (url, settings, format_override) = {
        let state = app.state::<AppState>();
        let settings = state.settings.lock().map(|s| s.clone()).unwrap_or(Value::Null);
        let found = state.tasks.lock().ok().and_then(|t| {
            t.iter()
                .find(|x| x.id == task_id)
                .map(|x| (x.url.clone(), x.format_override.clone()))
        });
        match found {
            Some((u, f)) => (u, settings, f),
            None => return,
        }
    };

    let Some(exe) = paths::resolve_ytdlp() else {
        // 说明候选都试过但没一个能跑——把候选列出来，否则用户无从下手
        let detail = paths::diagnose_ytdlp()
            .iter()
            .map(|(p, exists, usable)| {
                format!(
                    "{} [{}]",
                    p,
                    if *usable {
                        "可用"
                    } else if *exists {
                        "存在但无法运行"
                    } else {
                        "不存在"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("；");
        let msg = if detail.is_empty() {
            "未找到 yt-dlp 可执行文件".to_string()
        } else {
            format!("未找到可用的 yt-dlp。候选：{detail}")
        };
        update(&app, &task_id, |t| {
            t.state = "failed".into();
            t.error = Some(msg);
        });
        return;
    };

    update(&app, &task_id, |t| {
        t.state = "probing".into();
        t.error = None;
        // 告警描述的是**上一次**尝试的结果（后处理失败、上次的格式表达式不可用……），
        // 探测开始意味着新一轮尝试，留着旧的会让人以为是刚发生的事。
        t.warnings.clear();
        t.queue_hint = Some("正在解析".into());
    });

    // 探测用的表达式必须与**下载时**完全一致，否则预估大小对不上实际下载。
    // 所以走同一组读取器（`spec_from_settings` + 任务级 override）。
    let expression = {
        let mut spec = spec_from_settings(&settings, &url, &task_id);
        spec.format_override = format_override;
        spec.effective_format_expression()
    };

    // `--flat-playlist`：实测在单个视频上同样返回完整的 formats，
    // 所以一次探测同时覆盖「单视频」与「播放列表」两种情况。
    // JS 运行时在探测阶段同样必需——否则拿到的就是降级结果。
    //
    // 带上 `-f` 是为了拿 `requested_downloads`（预估大小）。实测带 `-f` 时
    // `formats` 数组依然完整，格式表不受影响。
    let info = match probe_once(&exe, &url, &settings, Some(&expression)).await {
        Ok(i) => i,
        Err(first) => {
            // ⚠️ 表达式不可满足时 yt-dlp **整个探测都会失败**（exit=1，连 JSON 都不给）。
            // 预估大小可以让步，元数据与格式表不能让——退回不带 `-f` 再探一次。
            match probe_once(&exe, &url, &settings, None).await {
                Ok(i) => {
                    // 只有确认是「格式不可用」才告警：如果是偶发网络抖动导致的失败，
                    // 提示「你的格式表达式不可用」就是误导。
                    if first.format_unavailable {
                        push_warning(
                            &app,
                            &task_id,
                            format!(
                                "无法按当前格式表达式预估大小——该表达式对这个站点不可用，\
                                 下载时很可能同样失败。表达式：{expression}"
                            ),
                        );
                    }
                    i
                }
                Err(second) => {
                    fail(&app, &task_id, &second.message);
                    return;
                }
            }
        }
    };

    apply_info(&app, &task_id, &url, info);
}

/// 一次探测的失败信息。
struct ProbeFailure {
    /// 已经过 `classify_error` 的、可直接展示给用户的文本。
    message: String,
    /// 失败原因是「所选格式不可用」——用于区分「用户的表达式有问题」和偶发故障。
    format_unavailable: bool,
}

/// 跑一次探测。`format` 为 `Some` 时会带上 `-f`，从而拿到预估大小。
async fn probe_once(
    exe: &std::path::Path,
    url: &str,
    settings: &Value,
    format: Option<&str>,
) -> Result<ytdlp_core::MediaInfo, ProbeFailure> {
    let js = js_of(settings);
    let args = ytdlp_core::build_probe_args(
        url,
        proxy_for(settings, url).as_deref(),
        cookies_of(settings).as_ref(),
        true,
        &js,
        format,
    );

    let out = tokio::process::Command::new(exe)
        .args(&args)
        .stdin(Stdio::null())
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .output()
        .await;

    match out {
        Ok(o) if o.status.success() => {
            let json = crate::text::decode_console(&o.stdout);
            ytdlp_core::parse_info_json(&json).map_err(|e| ProbeFailure {
                message: format!("探测结果解析失败：{e}"),
                format_unavailable: false,
            })
        }
        Ok(o) => {
            let raw = crate::text::decode_console(&o.stderr);
            Err(ProbeFailure {
                message: classify_error(&raw, o.status.code()),
                format_unavailable: raw.contains("Requested format is not available"),
            })
        }
        Err(e) => Err(ProbeFailure {
            message: format!("无法启动 yt-dlp：{e}"),
            format_unavailable: false,
        }),
    }
}

/// 写入进度。
///
/// **不在这里切状态**：`finished` 只表示下载完成，后面还有合并与嵌入（HANDOFF §3.2），
/// 状态由后处理标记或进程退出决定。
fn apply_progress(app: &AppHandle, task_id: &str, p: ytdlp_core::Progress, from_aria2c: bool) {
    let (downloaded, total, speed, eta) = (p.downloaded, p.total, p.speed, p.eta);
    update(app, task_id, |t| {
        t.progress = Progress {
            downloaded,
            total,
            speed,
            eta,
        };
        if from_aria2c {
            // 界面据此提示「进度来自 aria2c，可能不精确」（DESIGN §11.6）。
            t.used_aria2c = true;
        }
    });
}

fn fail(app: &AppHandle, task_id: &str, msg: &str) {
    let msg = msg.to_string();
    update(app, task_id, |t| {
        t.state = "failed".into();
        t.error = Some(msg);
    });
}

/// 把 yt-dlp 的原始报错归类成用户看得懂的原因（DESIGN §6、§14）。
///
/// 不做这层归类，用户只会看到一段 stderr 然后反复重试。
fn classify_error(stderr: &str, code: Option<i32>) -> String {
    let pick = |needle: &str| stderr.lines().find(|l| l.contains(needle));

    // PyInstaller 引导器报错。最常见的成因是 onedir 构建被单独复制，
    // 丢了同级的 `_internal\` 目录——报错文本里只有一句 DLL 加载失败，很难猜。
    if let Some(l) = pick("Failed to load Python DLL").or_else(|| pick("[PYI-")) {
        return format!(
            "yt-dlp 副本不完整（onedir 构建缺少 _internal 目录）。\
             请改用官方发布的 onefile 版 yt-dlp.exe：{}",
            l.trim()
        );
    }
    if let Some(l) = pick("Sign in to confirm") {
        return format!(
            "需要登录验证。两种常见原因，按可能性排序：\n\
             ① 没配 cookie —— 去「设置 → 网络与账号 → Cookie」，选「从浏览器」并选 Firefox；\n\
             ② 代理出口 IP 被判定为机房 / 机器人 —— 换用住宅 IP 的节点。\n\
             原始信息：{}",
            l.trim()
        );
    }
    if let Some(l) = pick("Private video")
        .or_else(|| pick("members-only"))
        .or_else(|| pick("This video is available to this channel's members"))
    {
        return format!("需要登录或会员权限：{}", l.trim());
    }
    if let Some(l) = pick("Unable to download webpage").or_else(|| pick("Failed to resolve")) {
        return format!("网络不可达（检查代理设置）：{}", l.trim());
    }
    // YouTube 对「可疑请求」的两种降级响应。
    if let Some(l) = pick("needs to be reloaded") {
        return format!(
            "YouTube 返回了「需要重载页面」——站点对可疑请求的降级响应。\n\
             与具体视频无关。常见原因（按经验排序）：\n\
             ① **短时间请求过多被限流** —— 等几分钟再试往往就恢复；\n\
             ② 代理出口是机房 IP（Azure/AWS 等）—— 换住宅 IP 的节点；\n\
             ③ cookie 对应的会话被判定异常 —— 到浏览器里重新登录一次。\n\
             原始信息：{}",
            l.trim()
        );
    }
    if let Some(l) = pick("Requested format is not available") {
        return format!(
            "所选格式不可用。\n\
             注意：如果链接来自 YouTube，这通常**不是**格式设置的问题——\n\
             拿不到真实流时（只返回 storyboard 预览图）也会报这个错。\n\
             可以用 `yt-dlp -F <链接>` 看看实际列出了什么。\n\
             原始信息：{}",
            l.trim()
        );
    }
    if let Some(l) = pick("Unsupported URL") {
        return format!("不支持的链接：{}", l.trim());
    }
    if let Some(l) = stderr.lines().find(|l| l.starts_with("ERROR:")) {
        return l.trim().to_string();
    }
    format!("yt-dlp 退出码 {}", code.unwrap_or(-1))
}

/// 探测成功后写回任务，并决定下一步：等用户勾选，还是直接排队下载。
fn apply_info(app: &AppHandle, task_id: &str, url: &str, info: ytdlp_core::MediaInfo) {
    let is_playlist = info.is_playlist && !info.entries.is_empty();

    {
        let st = app.state::<AppState>();
        st.with_task(task_id, |t| {
            if !info.title.is_empty() {
                t.title = info.title.clone();
            }
            if !info.extractor.is_empty() {
                t.extractor = info.extractor.clone();
            }
            t.thumbnail = info.thumbnail.clone();
            t.duration_sec = info.duration;
            t.subtitle_langs = info.subtitle_langs.clone();
            // 预估大小：探测时带了 `-f`，yt-dlp 会给出实际选中格式的合计体积。
            // 拿不到就是 `None`——界面必须显示「未知」，不能显示 0。
            t.size_estimate = info.size_estimate;
            // 格式表可能很长（播放列表里每个条目都有自己的），截断避免数据库膨胀
            t.formats = info.formats.iter().take(120).cloned().collect();
            if is_playlist {
                t.state = "selecting".into();
                t.playlist_entries = info.entries.clone();
                t.queue_hint = Some("等待选择要下载的集数".into());
            } else {
                t.state = "queued".into();
                t.queue_hint = Some("排队中".into());
            }
        });
    }

    let st = app.state::<AppState>();
    let _ = app.emit("task://update", st.snapshot());

    if is_playlist {
        // 等用户在 UI 上勾选，不自动排队下载——否则 500 集的合集会被直接下满硬盘。
        return;
    }

    let host = ytdlp_core::host_of(url);
    app.state::<crate::scheduler::Scheduler>()
        .enqueue_download(task_id.to_string(), host);
}

/// 取消一个正在运行的任务（连带子进程树）。
pub fn cancel_task(app: &AppHandle, task_id: &str) -> bool {
    let Some((pid, cancel)) = app.state::<AppState>().running_handle(task_id) else {
        return false;
    };
    cancel.store(true, Ordering::SeqCst);
    kill_tree(pid);
    true
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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