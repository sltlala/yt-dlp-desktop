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

mod config;
mod errors;

pub use config::*;
use errors::*;

use crate::paths;
use crate::state::{AppState, Progress, Task};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use ytdlp_core::args::DownloadSpec;
use ytdlp_core::{parse_line, Event, SkipReason};

/// 任务进入终态时弹系统通知（ROADMAP §F1）。
///
/// 由「设置里的三个开关」决定弹不弹：`notifyOnComplete` / `notifyOnFailure`
/// / `notifyOnSkip`。`canceled` 一律不弹（是用户自己取消的）。
/// 重试路径（Cloudflare 盲重试）把状态拨回 pending 后**不会**走到这里，
/// 所以不会把「重试前那次失败」误报成「最终失败」。
fn notify_terminal(app: &AppHandle, t: &Task) {
    let settings = app
        .state::<AppState>()
        .settings
        .lock()
        .map(|s| s.clone())
        .unwrap_or(Value::Null);
    let on = |key: &str| {
        settings
            .get(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };

    let (title, body) = match t.state.as_str() {
        "completed" if on("notifyOnComplete") => {
            ("下载完成".to_string(), truncate_title(&t.title))
        }
        "failed" if on("notifyOnFailure") => {
            let err = t
                .error
                .as_deref()
                .map(|e| truncate(error_summary(e), 60))
                .unwrap_or_else(|| "下载失败".to_string());
            ("下载失败".to_string(), err)
        }
        "skipped" if on("notifyOnSkip") => {
            ("已跳过".to_string(), truncate_title(&t.title))
        }
        _ => return,
    };

    let _ = app
        .notification()
        .builder()
        .title(&title)
        .body(&body)
        .show();
}

/// 标题截断到 60 字符，避免通知正文溢出。`truncate` 按字节边界截，中文会
/// 被切开半个字；这里按字符边界处理。
fn truncate_title(s: &str) -> String {
    truncate(s, 60)
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

/// 更新单个任务，并只把**这一个任务**广播给前端（`task://patch`）。
///
/// 进度每 0.2 秒一条、且可能多个任务并发：若每次都广播全量快照
/// （`task://update`），就要在锁内克隆全部历史任务（各自带着格式表、
/// 播放列表条目）再整体序列化，前端也会因整表替换而重绘每一行。
/// 增删任务这类改变列表结构的操作仍走全量快照。
///
/// `with_task` 会自动把该任务标记为「待写库」，所以这里不必手动 mark——
/// 这正是把写入口收窄成三个方法的目的。
pub(crate) fn update<F: FnOnce(&mut Task)>(app: &AppHandle, id: &str, f: F) {
    let changed = app.state::<AppState>().with_task(id, |t| {
        f(t);
        t.clone()
    });
    if let Some(t) = changed {
        let _ = app.emit("task://patch", &t);
    }
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

/// 落定 ffmpeg 的位置（`--ffmpeg-location`）。
///
/// 这次下载用得上它却找不到时给任务挂告警：下载命令带 `--no-warnings`，
/// yt-dlp 自己那句「无法合并」会被吞掉，而后果是**静默降级**成低画质，
/// 不说一声的话用户只会觉得「怎么选了 1080p 下来是 360p」。
fn prepare_ffmpeg(app: &AppHandle, task_id: &str, spec: &mut DownloadSpec) {
    match crate::paths::resolve_ffmpeg() {
        Some(p) => spec.ffmpeg_location = p.parent().map(PathBuf::from),
        None if spec.needs_ffmpeg() => push_warning(
            app,
            task_id,
            "未找到 ffmpeg：画质会降级，可在「设置 → 下载器」一键下载",
        ),
        None => {}
    }
}

/// 运行一个下载任务。返回最终状态字符串。
pub async fn run_download(app: AppHandle, task_id: String) {
    let (url, playlist_items, format_override, auto_impersonate, retry_flags, sections) = {
        let state = app.state::<AppState>();
        let found = state.tasks.lock().ok().and_then(|t| {
            t.iter().find(|x| x.id == task_id).map(|x| {
                (
                    x.url.clone(),
                    x.playlist_items.clone(),
                    x.format_override.clone(),
                    x.auto_impersonate,
                    x.retry_flags,
                    x.sections.clone(),
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
        spec.sections = sections;
        // 探测阶段如果已经因为 Cloudflare 开过模拟，这里直接沿用——
        // 否则要再撞一次 403 才发现，白跑一轮。
        spec.impersonate = spec.impersonate || auto_impersonate;
        // 自动降级重试（ROADMAP §F2）：上一轮失败后，本轮按位掩码关掉
        // 会诱发失败的那一项。
        if retry_flags & RETRY_NO_ARIA2C != 0 {
            spec.aria2c = false;
        }
        if retry_flags & RETRY_NO_IMPERSONATE != 0 {
            spec.impersonate = false;
        }
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
    prepare_ffmpeg(&app, &task_id, &mut spec);

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
            let err = t.error.take().unwrap_or_default();
            let msg = if err.is_empty() {
                "视频已下载，但部分附加文件（字幕、封面等）失败".to_string()
            } else {
                format!("视频已下载，但部分附加文件失败：{}", error_summary(&err))
            };
            if t.warnings.len() < 20 && !t.warnings.contains(&msg) {
                t.warnings.push(msg);
            }
        } else {
            t.state = "failed".into();
            if t.error.is_none() {
                t.error = Some(format!("下载失败（yt-dlp 退出码 {code}）"));
            }
        }
    });

    // 终态记一行：出问题时，日志里这一行往往就是唯一能说明「当时怎么了」的线索。
    let mut blind_retry = false;
    // 自动降级重试（ROADMAP §F2）：非终态时保持 None。
    let mut degrade_retry = RetryAction::None;
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

            // 降级重试独立于上面的反爬重试：针对网络抖动 / aria2c 特有失败。
            // 二者互斥——若已决定反爬重试，就不再做降级重试（一次只改一个变量，
            // 否则分不清是哪个动作起的作用）。
            if !blind_retry {
                degrade_retry = should_auto_retry(
                    &t.state,
                    t.retry_flags,
                    t.error.as_deref(),
                    t.used_aria2c,
                    spec.impersonate,
                );
            }
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
    } else if degrade_retry != RetryAction::None {
        // 降级重试：按动作设好位掩码与提示，重新入队。
        let (flag, hint) = match degrade_retry {
            RetryAction::RetrySame => (
                RETRY_SAME,
                "网络抖动，自动原样重试一次".to_string(),
            ),
            RetryAction::RetryNoAria2c => (
                RETRY_NO_ARIA2C,
                "aria2c 下载失败，自动改用内置下载器重试一次".to_string(),
            ),
            RetryAction::RetryNoImpersonate => (
                RETRY_NO_IMPERSONATE,
                "指纹模拟可能导致失败，自动关闭后重试一次".to_string(),
            ),
            RetryAction::None => unreachable!(),
        };
        crate::logfile::info(format!("[{task_id}] {hint}"));
        update(&app, &task_id, |t| {
            t.retry_flags |= flag;
            t.state = "pending".into();
            t.error = None;
            t.finished_at = None;
            t.queue_hint = Some(hint.clone());
        });
        push_warning(&app, &task_id, format!("{hint}（只对这个任务有效）"));
        let host = ytdlp_core::host_of(&url);
        app.state::<crate::scheduler::Scheduler>()
            .enqueue_download(task_id.to_string(), host);
    } else {
        // 真·终态：弹系统通知（如果设置允许）。
        // 注意 blind_retry / degrade_retry 分支会重新入队，这里不会误发「失败」通知。
        if let Ok(tasks) = app.state::<AppState>().tasks.lock() {
            if let Some(t) = tasks.iter().find(|x| x.id == task_id) {
                notify_terminal(&app, t);
            }
        }
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
            let msg = classify_error(&msg, None);
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
            format!("未找到可用的 yt-dlp：请在「设置 → 下载器」里更新 yt-dlp\n候选：{detail}")
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

/// 订阅检查用：只拿频道 / 播放列表**最新的 `limit` 条**扁平条目。
///
/// 频道动辄几千个视频，不限条数的话每次检查都要把整页翻完。
pub(crate) async fn probe_feed(
    url: &str,
    settings: &Value,
    limit: usize,
) -> Result<ytdlp_core::MediaInfo, String> {
    let exe = paths::resolve_ytdlp().ok_or("未找到可用的 yt-dlp")?;
    let mut args = ytdlp_core::build_probe_args(
        url,
        proxy_for(settings, url).as_deref(),
        cookies_of(settings).as_ref(),
        true,
        &js_of(settings),
        None,
        impersonate_of(settings),
    );
    // 选项必须在 `--` 之前，`--` 之后只能是 URL
    let at = args.iter().position(|a| a == "--").unwrap_or(args.len());
    args.splice(at..at, ["--playlist-end".to_string(), limit.to_string()]);
    log_command("订阅检查", "-", &exe, &args);

    let o = tokio::process::Command::new(&exe)
        .args(&args)
        .stdin(Stdio::null())
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .output()
        .await
        .map_err(|e| format!("无法启动 yt-dlp：{e}"))?;
    if !o.status.success() {
        let raw = crate::text::decode_console(&o.stderr);
        return Err(classify_error(&raw, o.status.code()));
    }
    ytdlp_core::parse_info_json(&crate::text::decode_console(&o.stdout))
}

/// 写入进度。
///
/// **不在这里切状态**：`finished` 只表示下载完成，后面还有合并与嵌入（HANDOFF §3.2），
/// 状态由后处理标记或进程退出决定。
/// 进度专用的增量事件（`task://progress`）。
///
/// 进度是最高频的事件；走 `task://patch` 的话每条都要克隆并序列化整个任务，
/// 包括格式表和播放列表条目。
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressPatch<'a> {
    id: &'a str,
    progress: Progress,
    used_aria2c: bool,
}

fn apply_progress(app: &AppHandle, task_id: &str, p: ytdlp_core::Progress, from_aria2c: bool) {
    let progress = Progress {
        downloaded: p.downloaded,
        total: p.total,
        speed: p.speed,
        eta: p.eta,
    };
    let used_aria2c = app.state::<AppState>().with_task(task_id, |t| {
        t.progress = progress.clone();
        if from_aria2c {
            // 界面据此提示「进度来自 aria2c，可能不精确」（DESIGN §11.6）。
            t.used_aria2c = true;
        }
        t.used_aria2c
    });
    if let Some(used_aria2c) = used_aria2c {
        let _ = app.emit(
            "task://progress",
            ProgressPatch { id: task_id, progress, used_aria2c },
        );
    }
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

/// 本地重复检测（ROADMAP §F17）：在历史任务里找「同 media id 且成品仍在磁盘」
/// 的条目，返回其成品路径。纯函数，便于单测。
///
/// 判定条件：别的任务、同 `video_id`、状态是 completed/skipped、`filepath` 存在。
fn find_local_duplicate(tasks: &[Task], task_id: &str, video_id: &str) -> Option<String> {
    tasks.iter().find_map(|other| {
        let hit = other.id != task_id
            && other.video_id.as_deref() == Some(video_id)
            && matches!(other.state.as_str(), "completed" | "skipped")
            && other
                .filepath
                .as_deref()
                .map(|p| std::path::Path::new(p).exists())
                .unwrap_or(false);
        hit.then(|| other.filepath.clone()).flatten()
    })
}

/// 探测成功后写回任务，并决定下一步：等用户勾选，还是直接排队下载。
fn apply_info(app: &AppHandle, task_id: &str, url: &str, info: ytdlp_core::MediaInfo) {
    let is_playlist = info.is_playlist && !info.entries.is_empty();

    {
        // 本地重复检测（ROADMAP §F17）：同 media id 且成品仍在磁盘 → 提示本地已有。
        // 只在**拿到 media id** 时做；播放列表没有单一 media id，跳过。
        let duplicate = if info.id.is_empty() {
            None
        } else {
            app.state::<AppState>()
                .tasks
                .lock()
                .ok()
                .and_then(|tasks| find_local_duplicate(&tasks, task_id, &info.id))
        };

        update(app, task_id, |t| {
            if !info.title.is_empty() {
                t.title = info.title.clone();
            }
            if !info.extractor.is_empty() {
                t.extractor = info.extractor.clone();
            }
            // 媒体 id 与任务 id 是两回事：归档里存的是 `<提取器> <媒体id>`，
            // 「从归档移除」要靠这个字段反查（§13.3）。
            if !info.id.is_empty() {
                t.video_id = Some(info.id.clone());
            }
            t.thumbnail = info.thumbnail.clone();
            t.duration_sec = info.duration;
            t.subtitle_langs = info.subtitle_langs.clone();
            // 预估大小：探测时带了 `-f`，yt-dlp 会给出实际选中格式的合计体积。
            // 拿不到就是 `None`——界面必须显示「未知」，不能显示 0。
            t.size_estimate = info.size_estimate;
            // 格式表可能很长（播放列表里每个条目都有自己的），截断避免数据库膨胀
            t.formats = info.formats.iter().take(120).cloned().collect();
            t.local_duplicate = duplicate;
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

    // ─────────── 通知正文截断（ROADMAP §F1）───────────

    #[test]
    fn truncate_respects_char_boundary() {
        // 短于上限原样返回
        assert_eq!(truncate("短标题", 60), "短标题");
        // 正好等于上限，不加省略号
        let exact = "字".repeat(60);
        assert_eq!(truncate(&exact, 60), exact);
        // 超长：按字符边界截断，加省略号（不会把中文切成半个字）
        let long = "字".repeat(61);
        let out = truncate(&long, 60);
        assert_eq!(out.chars().count(), 61); // 60 字 + 省略号
        assert!(out.ends_with('…'));
    }

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

    // ───────── 本地重复检测（ROADMAP §F17）─────────

    fn dup_task(id: &str, vid: &str, state: &str, filepath: Option<String>) -> Task {
        let mut t = Task::new(
            id.to_string(),
            format!("https://example.com/{vid}"),
            "C:/out".into(),
            "bv*+ba/b".into(),
        );
        t.video_id = Some(vid.to_string());
        t.state = state.to_string();
        t.filepath = filepath;
        t
    }

    #[test]
    fn finds_local_duplicate_when_file_exists() {
        let existing = std::env::temp_dir().join("ytdlp-dup-test.txt");
        std::fs::write(&existing, b"x").unwrap();
        let tasks = vec![
            dup_task("t-1", "vid123", "completed", Some(existing.to_string_lossy().into_owned())),
            dup_task("t-2", "other", "completed", None),
        ];
        // 用真实存在的文件路径
        let found = find_local_duplicate(&tasks, "t-new", "vid123");
        let expected = existing.to_string_lossy().into_owned();
        assert_eq!(found.as_deref(), Some(expected.as_str()));
        let _ = std::fs::remove_file(&existing);
    }

    #[test]
    fn no_duplicate_when_file_gone_or_state_active() {
        // 文件不存在 → 不算重复
        let tasks = vec![dup_task(
            "t-1",
            "vid123",
            "completed",
            Some("Z:/definitely/missing.mp4".into()),
        )];
        assert_eq!(find_local_duplicate(&tasks, "t-new", "vid123"), None);

        // 状态不是 completed/skipped → 不算（还在下载中的不算「已有成品」）
        let tasks2 = vec![dup_task("t-1", "vid123", "downloading", None)];
        assert_eq!(find_local_duplicate(&tasks2, "t-new", "vid123"), None);

        // 同一个任务自己不算重复
        let tasks3 = vec![dup_task("t-new", "vid123", "completed", None)];
        assert_eq!(find_local_duplicate(&tasks3, "t-new", "vid123"), None);
    }
}
