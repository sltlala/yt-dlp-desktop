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
    // 过 Cloudflare 拦截（默认关，见 DownloadSpec::impersonate）
    spec.impersonate = impersonate_of(settings);
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
    let (url, playlist_items, format_override, auto_impersonate) = {
        let state = app.state::<AppState>();
        let found = state.tasks.lock().ok().and_then(|t| {
            t.iter().find(|x| x.id == task_id).map(|x| {
                (
                    x.url.clone(),
                    x.playlist_items.clone(),
                    x.format_override.clone(),
                    x.auto_impersonate,
                )
            })
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
        // 探测阶段如果已经因为 Cloudflare 开过模拟，这里直接沿用——
        // 否则要再撞一次 403 才发现，白跑一轮。
        spec.impersonate = spec.impersonate || auto_impersonate;
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
    log_command("下载", &task_id, &exe, &args);

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
            crate::logfile::error(format!("[{task_id}] 无法启动 yt-dlp：{e}"));
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

    // 「成品是否真的落地了」只用**这一轮**的 filepath.txt 判断，不能看 `t.filepath`：
    // 它可能还留着上一次尝试的旧路径。filepath.txt 每轮派发前都被截断（见上），
    // 所以这里读到 None 就意味着这一轮没有产出成品。
    let produced = filepath
        .as_deref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len() > 0)
        .unwrap_or(false);
    // 选集任务（`--playlist-items`）多集共用一个 filepath.txt，最后一行只代表最后一集，
    // 用它判断「整单是否都下好了」会漏掉中间某一集失败，所以非零退出码一律判 failed。
    let multi_item = spec.playlist_items.is_some();

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
        } else if should_complete_despite_exit(produced, multi_item) {
            // 成品文件已经在磁盘上：媒体本身下载成功，非零退出码只是附加项失败
            // （字幕/弹幕/缩略图/元数据等）。不能判 failed，否则用户明明拿到了
            // 完整视频，任务却红成失败（DESIGN §11.8，B站弹幕那起事故）。
            t.state = "completed".into();
            t.finished_at = Some(crate::state::now_ms());
            let err = t.error.take().unwrap_or_else(|| format!("yt-dlp 退出码 {code}"));
            let msg = format!(
                "成品文件已生成，但 yt-dlp 以退出码 {code} 结束：{err}（多为附加文件失败，不影响视频本体）"
            );
            if t.warnings.len() < 20 && !t.warnings.contains(&msg) {
                t.warnings.push(msg);
            }
        } else {
            t.state = "failed".into();
            if t.error.is_none() {
                t.error = Some(format!("yt-dlp 退出码 {code}"));
            }
        }
    });

    // 终态记一行：出问题时，日志里这一行往往就是唯一能说明「当时怎么了」的线索。
    let mut blind_retry = false;
    if let Ok(tasks) = app.state::<AppState>().tasks.lock() {
        if let Some(t) = tasks.iter().find(|x| x.id == task_id) {
            let detail = t
                .error
                .as_deref()
                .map(|e| format!("  错误：{}", e.replace('\n', " ")))
                .or_else(|| {
                    t.warnings
                        .last()
                        .map(|w| format!("  提示：{}", w.replace('\n', " ")))
                })
                .unwrap_or_default();
            let line = format!("[{task_id}] 结束：{}  exit={code}{detail}", t.state);
            if t.state == "failed" || t.state == "canceled" {
                crate::logfile::warn(line);
            } else {
                crate::logfile::info(line);
            }

            // 撞上 Cloudflare 拦截、而且**还没试过**指纹模拟 -> 自动重试一次。
            //
            // 重试走的是重新入队（`enqueue_download`），不是在这里循环：
            // 那条路会把 temp 目录、filepath.txt 这些该重置的都重置好。
            // `auto_impersonate` 这个旗标保证**只重试一次**——再失败就是真失败。
            blind_retry = should_retry_with_impersonate(&t.state, t.auto_impersonate, t.error.as_deref());
        }
    }

    if blind_retry {
        crate::logfile::info(format!(
            "[{task_id}] 下载被站点拦截（HTTP 403），自动开启指纹模拟重试一次"
        ));
        update(&app, &task_id, |t| {
            t.auto_impersonate = true;
            t.state = "pending".into();
            t.error = None;
            t.finished_at = None;
            t.queue_hint = Some("被站点拦截，已开启指纹模拟重试".into());
        });
        push_warning(
            &app,
            &task_id,
            "站点有反爬拦截（HTTP 403），已自动开启浏览器指纹模拟重试（只对这个任务有效）".to_string(),
        );
        let host = ytdlp_core::host_of(&url);
        app.state::<crate::scheduler::Scheduler>()
            .enqueue_download(task_id.to_string(), host);
    }
}

/// 把命令行渲染成一行可读文本（带空格/引号的参数加引号，代理解码打码）。
///
/// 抽成纯函数是为了能单测**打码**这件事——日志文件是要发给别人看的，
/// 里面出现代理密码就是事故。
fn render_command_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            // 只对代理参数打码：其他参数里没有密码。
            // 判据用「像不像代理 URL」，而不是「含不含 @」——后者会把
            // `--cookies user@example.com.txt` 这类路径也误伤。
            if a.starts_with("http://") || a.starts_with("https://") || a.starts_with("socks5") {
                redact_proxy(a)
            } else if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 把一次 yt-dlp 调用的完整命令行写进日志。
///
/// **这是排查问题时最想看的一行**：应用到底怎么调用 yt-dlp 的、带了哪些参数，
/// 光看界面上的「下载失败」永远推不出来。
fn log_command(kind: &str, task_id: &str, exe: &std::path::Path, args: &[String]) {
    crate::logfile::info(format!(
        "[{task_id}] {kind} 命令：{} {}",
        exe.display(),
        render_command_args(args)
    ));
}

/// 一条子进程输出里**没被解析器认领**的行。
///
/// `stream_lines` 会把认不出的行直接丢掉——而 yt-dlp 的 `ERROR:` / `WARNING:` /
/// 提取器自己的说明恰好都在里面。丢掉就等于出问题时毫无线索，所以至少留一份在日志里。
fn log_raw_line(task_id: &str, line: &str) {
    // 别把整份日志淹掉：只记有信息量的
    let l = line.trim();
    if l.is_empty() {
        return;
    }
    let interesting = l.starts_with("ERROR")
        || l.starts_with("WARNING")
        || l.starts_with("[")
        || l.contains("Error")
        || l.contains("error");
    if interesting {
        crate::logfile::info(format!("[{task_id}] {l}"));
    }
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
        } else {
            // 认不出的行以前是直接丢掉的——那正是 yt-dlp 自己的报错与说明
            log_raw_line(&task_id, &line);
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
fn settings_with_impersonate(settings: &Value) -> Value {
    let mut v = settings.clone();
    if let Some(o) = v.as_object_mut() {
        o.insert("impersonate".into(), Value::Bool(true));
    }
    v
}

/// 这段输出是不是 Cloudflare 反爬拦截。
///
/// 认的是 yt-dlp 自己那句话（`generic.py` 里写死的），
/// 分类后的中文提示里也原样带着它，所以两种来源都能认出。
fn is_cloudflare_challenge(text: &str) -> bool {
    text.contains("Cloudflare anti-bot challenge")
}

/// 是不是「generic 提取器被 HTTP 403 挡了」。
///
/// yt-dlp 只在**认出**那是 Cloudflare 挑战时才给那句建议——它要求响应头有
/// `cf-mitigated: challenge`，或者页面标题正好是 `Attention Required! | Cloudflare`。
/// 很多站点用的是自家规则（或者 CDN 没带那个头），同样是 403 却只报一句
/// `Unable to download webpage: HTTP Error 403`。
///
/// 这两种对**指纹模拟**的反应是一样的，所以自动重试的条件把两种都收进来。
/// 实测用户遇到的正是后一种：报错里根本没有 Cloudflare 字样。
fn is_blocked_generic_403(text: &str) -> bool {
    text.contains("[generic]") && text.contains("HTTP Error 403")
}

/// 值不值得为它开指纹模拟再试一次。
fn worth_impersonating(text: &str) -> bool {
    is_cloudflare_challenge(text) || is_blocked_generic_403(text)
}

/// 这次下载失败要不要**自动**带着指纹模拟重试一次。
///
/// 三个条件缺一不可：
/// - 确实失败了（不是 canceled，也不是正常跳过）；
/// - **还没试过**自动模拟——`already` 保证只重试一次，否则会死循环；
/// - 失败原因是站点把我们挡了（Cloudflare 挑战，或没带标记的普通 403）。
///
/// 抽成纯函数是因为这个判断错了的代价很大：漏判 = 用户继续看到那个
/// 看不懂的 403；误判 = 每个失败都白跑一遍。
fn should_retry_with_impersonate(state: &str, already: bool, error: Option<&str>) -> bool {
    state == "failed"
        && !already
        && error.map(worth_impersonating).unwrap_or(false)
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
    let info = match probe_once(&exe, &task_id, &url, &settings, Some(&expression)).await {
        Ok(i) => i,
        Err(first) => {
            // ① 被站点拦截（403）：**带着指纹模拟原地重试一次**。
            //    放在「退回不带 -f」之前——那种退让解决不了 403，
            //    只会把同一个失败再走一遍。
            if first.blocked && !impersonate_of(&settings) {
                crate::logfile::info(format!(
                    "[{task_id}] 探测被站点拦截（HTTP 403），自动开启指纹模拟重试"
                ));
                let forced = settings_with_impersonate(&settings);
                match probe_once(&exe, &task_id, &url, &forced, Some(&expression)).await {
                    Ok(i) => {
                        // 记住「这个站点要模拟」，下载阶段直接沿用，
                        // 不用再撞一次才发现。**不写进用户设置**。
                        update(&app, &task_id, |t| t.auto_impersonate = true);
                        push_warning(
                            &app,
                            &task_id,
                            "站点有反爬拦截（HTTP 403），已自动开启浏览器指纹模拟（只对这个任务有效）"
                                .to_string(),
                        );
                        apply_info(&app, &task_id, &url, i);
                        return;
                    }
                    Err(_) => {
                        crate::logfile::warn(format!(
                            "[{task_id}] 开启指纹模拟后仍被拦截"
                        ));
                        // 模拟也没用，继续走原来的退让逻辑
                    }
                }
            }

            // ⚠️ 表达式不可满足时 yt-dlp **整个探测都会失败**（exit=1，连 JSON 都不给）。
            // 预估大小可以让步，元数据与格式表不能让——退回不带 `-f` 再探一次。
            match probe_once(&exe, &task_id, &url, &settings, None).await {
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
    /// 失败原因是站点把我们挡了（Cloudflare 挑战，或没带标记的普通 403）——
    /// 这两种都值得带着指纹模拟重试一次。
    blocked: bool,
}

/// 跑一次探测。`format` 为 `Some` 时会带上 `-f`，从而拿到预估大小。
async fn probe_once(
    exe: &std::path::Path,
    task_id: &str,
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
        impersonate_of(settings),
    );
    log_command("探测", task_id, exe, &args);

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
                blocked: false,
            })
        }
        Ok(o) => {
            let raw = crate::text::decode_console(&o.stderr);
            Err(ProbeFailure {
                message: classify_error(&raw, o.status.code()),
                format_unavailable: raw.contains("Requested format is not available"),
                blocked: worth_impersonating(&raw),
            })
        }
        Err(e) => Err(ProbeFailure {
            message: format!("无法启动 yt-dlp：{e}"),
            format_unavailable: false,
            blocked: false,
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

/// 非零退出码下，成品文件已落地、且不是多集选集任务时，说明媒体本身下载成功，
/// 剩余的错误只是附加项失败（字幕/弹幕/缩略图/元数据等）——应当记「已完成 + 警告」
/// 而不是 failed。抽成纯函数好单测（DESIGN §11.8）。
fn should_complete_despite_exit(produced: bool, multi_item: bool) -> bool {
    produced && !multi_item
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
    // 站点拒绝了我们。**必须排在「网络不可达」之前**：那句
    // `Unable to download webpage` 里经常裹着 HTTP 状态码，
    // 而「站点答了、只是不肯给」和「根本连不上」是两件完全不同的事。
    if let Some(l) = pick("Cloudflare anti-bot challenge") {
        return format!(
            "这个站点有 Cloudflare 反爬拦截（HTTP 403）。\n\
             已经自动带着浏览器指纹重试过一次了；还是不行的话，换一个代理出口 IP，\
             或者到「设置 → 网络与账号」配上 cookie 再试。\n\
             原始信息：{}",
            l.trim()
        );
    }
    if let Some(l) = pick("Unable to download webpage").or_else(|| pick("Failed to resolve")) {
        // ⚠️ 实测踩到：`ERROR: [generic] …: Unable to download webpage: HTTP Error 403`
        // 被判成「网络不可达（检查代理设置）」——站点明明答了，只是拒绝了我们，
        // 这句提示会把人引到完全相反的方向去查代理。
        if l.contains("HTTP Error 4") || l.contains("HTTP Error 5") {
            return format!(
                "站点拒绝了这次请求（HTTP 错误，不是网络不通）。\n\
                 403 / 451 常见于反爬或地区限制：可以到「设置 → 网络与账号」\
                 打开「绕过 Cloudflare 拦截」再试，或者换一个代理出口 IP。\n\
                 原始信息：{}",
                l.trim()
            );
        }
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
    // 站点用了 Cloudflare 反爬：yt-dlp 自己给出的解法是开指纹模拟，
    // 但那是个命令行参数，界面用户够不着——所以这里直接指到那个开关上。
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

    // ─────────── 成品已落地时的终态判定 ───────────

    /// DESIGN §11.8：成品文件在磁盘上 + 非多集选集 → 非零退出码仍算「已完成」。
    #[test]
    fn produced_but_failed_should_complete() {
        assert!(should_complete_despite_exit(true, false));
        // 没有产出成品 → 仍判失败
        assert!(!should_complete_despite_exit(false, false));
        // 选集任务 → 保守，仍判失败（多集共用一个 filepath.txt，最后一行不代表全单）
        assert!(!should_complete_despite_exit(true, true));
        assert!(!should_complete_despite_exit(false, true));
    }

    // ─────────── 日志里的命令行渲染 ───────────

    /// 认 Cloudflare 拦截靠的是 yt-dlp 自己那句话——**分类前后的文本都要认**：
    /// 探测走 `classify_error`（中文多行），下载走到的是原始 ERROR 行。
    #[test]
    fn cloudflare_challenge_is_recognized_in_both_forms() {        // 原始形式
        assert!(is_cloudflare_challenge(
            "ERROR: [generic] Got HTTP Error 403 caused by Cloudflare anti-bot challenge; \
             try again with --extractor-args \"generic:impersonate\""
        ));
        // 分类后的中文提示里原样带着那句话
        let classified = classify_error(
            "ERROR: [generic] Got HTTP Error 403 caused by Cloudflare anti-bot challenge",
            Some(1),
        );
        assert!(classified.contains("Cloudflare 反爬拦截"), "实际: {classified}");
        assert!(is_cloudflare_challenge(&classified));
        // 别的 403 不能误判
        assert!(!is_cloudflare_challenge("ERROR: HTTP Error 403: Forbidden"));
        assert!(!is_cloudflare_challenge("ERROR: Unable to download webpage"));
    }

    /// ⚠️ 实测踩到的误判：`Unable to download webpage` 里裹着 HTTP 状态码时，
    /// 那**不是**网络不通——站点答了，只是拒绝了我们。说成「网络不可达（检查代理设置）」
    /// 会把人引到完全相反的方向去查代理。
    #[test]
    fn http_error_is_not_reported_as_unreachable() {
        let real = "ERROR: [generic] watch?v=407946: Unable to download webpage: \
                    HTTP Error 403: Forbidden";
        let msg = classify_error(real, Some(1));
        assert!(!msg.contains("网络不可达"), "403 不该说成网络不可达：{msg}");
        assert!(msg.contains("拒绝了这次请求"), "实际: {msg}");

        // 真的连不上时仍然要说「检查代理设置」
        let dead = "ERROR: Unable to download webpage: <urlopen error [Errno 111] Connection refused>";
        let msg = classify_error(dead, Some(1));
        assert!(msg.contains("网络不可达"), "实际: {msg}");
    }

    /// Cloudflare 那句话里也含 `Unable to download webpage` 之外的形式，
    /// 但**必须优先于**网络那一支——否则会被抢答成「网络不可达」。
    #[test]
    fn cloudflare_takes_priority_over_unreachable() {
        let both = "ERROR: Unable to download webpage: HTTP Error 403: Forbidden\n\
                    ERROR: [generic] Got HTTP Error 403 caused by Cloudflare anti-bot challenge";
        let msg = classify_error(both, Some(1));
        assert!(msg.contains("Cloudflare 反爬拦截"), "实际: {msg}");
        assert!(!msg.contains("网络不可达"));
    }

    /// 没带 Cloudflare 标记、但确实是 generic 提取器被 403 挡了——
    /// **用户遇到的正是这一种**（报错里根本没有 Cloudflare 字样）。
    /// 这种也值得开指纹模拟试一次。
    #[test]
    fn unmarked_generic_403_also_worth_impersonating() {
        let real = "ERROR: [generic] watch?v=407946: Unable to download webpage: \
                    HTTP Error 403: Forbidden";
        assert!(is_blocked_generic_403(real));
        assert!(worth_impersonating(real));

        // 但**非 generic** 的 403 不碰：那是别的提取器的事，模拟帮不上
        assert!(!is_blocked_generic_403("ERROR: [youtube] x: HTTP Error 403: Forbidden"));
        assert!(!worth_impersonating("ERROR: [youtube] x: HTTP Error 403: Forbidden"));
        // 404 之类的也不是反爬
        assert!(!is_blocked_generic_403("ERROR: [generic] x: HTTP Error 404: Not Found"));
    }

    /// 自动重试的三个条件，逐个钉住。
    #[test]
    fn auto_retry_requires_all_three_conditions() {
        let cf = "ERROR: [generic] Got HTTP Error 403 caused by Cloudflare anti-bot challenge";
        // 正常触发
        assert!(should_retry_with_impersonate("failed", false, Some(cf)));
        // 已经试过一次 —— **不能再试**，否则死循环
        assert!(!should_retry_with_impersonate("failed", true, Some(cf)));
        // 不是失败（跳过/取消/完成）就不该重试
        assert!(!should_retry_with_impersonate("skipped", false, Some(cf)));
        assert!(!should_retry_with_impersonate("canceled", false, Some(cf)));
        assert!(!should_retry_with_impersonate("completed", false, Some(cf)));
        // 别的失败原因不重试（每个失败都白跑一遍才是最糟的）
        assert!(!should_retry_with_impersonate("failed", false, Some("HTTP Error 403: Forbidden")));
        assert!(!should_retry_with_impersonate("failed", false, Some("网络不可达")));
        assert!(!should_retry_with_impersonate("failed", false, None));
    }

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

    /// ⚠️ 最重要的一条：**代理密码不能进日志**。
    /// 日志文件是用户要发出来给人定位问题的，里面出现密码就是事故。
    #[test]
    fn logged_command_redacts_proxy_password() {
        let args: Vec<String> = ["--proxy", "http://alice:s3cr3t@127.0.0.1:7897", "--", "https://x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = render_command_args(&args);
        assert!(!out.contains("s3cr3t"), "密码泄露了：{out}");
        assert!(out.contains("***"), "应当打码成 ***：{out}");
        // 主机端口要留着——排查问题时正需要它
        assert!(out.contains("127.0.0.1:7897"), "主机端口不该被抹掉：{out}");
        assert!(out.contains("alice"), "用户名可以留：{out}");
    }

    /// 含 @ 的**路径**不能被误当成代理打码。
    #[test]
    fn logged_command_does_not_mangle_non_proxy_args() {
        let args: Vec<String> = ["--cookies", "C:\\u@home\\cookies.txt", "--output", "%(title)s.%(ext)s"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = render_command_args(&args);
        assert!(out.contains("C:\\u@home\\cookies.txt"), "路径被改坏了：{out}");
        assert!(!out.contains("***"), "不该出现打码：{out}");
    }

    /// 带空格的参数要加引号，否则日志里看不出那是一个参数。
    #[test]
    fn logged_command_quotes_args_with_spaces() {
        let args: Vec<String> = ["--paths", "home:E:\\My Videos\\out"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = render_command_args(&args);
        assert!(out.contains("\"home:E:\\My Videos\\out\""), "实际: {out}");
        // 不含空格的参数不该被加引号（加了反而不好读）
        assert!(out.contains("--paths "), "实际: {out}");
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