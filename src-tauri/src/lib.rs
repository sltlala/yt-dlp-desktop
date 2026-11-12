//! Tauri 2 外壳：命令面、状态装配与事件推送。
//!
//! 设计上刻意保持「薄」：所有参数构造与输出解析都在 `ytdlp-core` 里，
//! 这里只负责进程生命周期、持久化与前后端桥接。

// `settings::default_settings` 的 `json!` 字面量键很多，默认的 128 层宏展开不够用
#![recursion_limit = "256"]

mod afterall;
mod app_update;
mod clipboard;
mod cookies;
mod ffmpeg;
mod files;
mod logfile;
mod net;
mod paths;
mod push;
mod runner;
mod scheduler;
mod settings;
mod shell;
mod state;
mod store;
mod subscribe;
mod sysproxy;
mod text;
mod tools;
mod tray;
mod update;

use files::{sweep_orphan_temp_dirs, temp_root_of};
use scheduler::Scheduler;
use serde_json::{json, Value};
use settings::{default_settings, load_settings, normalize_settings};
use state::{AppState, Task};
use tauri::{AppHandle, Emitter, Manager, State};

// ─────────────────────────── 任务命令 ───────────────────────────

#[tauri::command]
fn list_tasks(state: State<AppState>) -> Vec<Task> {
    state.snapshot()
}

#[tauri::command]
fn add_url(app: AppHandle, url: String) -> Task {
    add_url_inner(&app, &url).expect("add_url 单条必定成功")
}

/// 批量添加的结果（ROADMAP §F3）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchAddResult {
    added: usize,
    failed: Vec<BatchAddFailure>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchAddFailure {
    url: String,
    reason: String,
}

/// 批量添加：多行粘贴 / 拖入 URL 列表一次进队。
///
/// 逐条去重（按 URL 字符串），无效文本收集到 `failed` 里反馈给前端，
/// 而不是静默丢弃——用户想知道「哪几条没认出来」。
#[tauri::command]
fn add_urls(app: AppHandle, urls: Vec<String>) -> BatchAddResult {
    let mut seen = std::collections::HashSet::new();
    let mut added = 0usize;
    let mut failed = Vec::new();

    for raw in urls {
        let url = raw.trim();
        if url.is_empty() {
            continue;
        }
        if !ytdlp_core::is_valid_url(url) {
            failed.push(BatchAddFailure {
                url: url.to_string(),
                reason: "不是有效链接".into(),
            });
            continue;
        }
        if !seen.insert(url.to_string()) {
            continue; // 重复，静默跳过
        }
        match add_url_inner(&app, url) {
            Ok(_) => added += 1,
            Err(e) => failed.push(BatchAddFailure {
                url: url.to_string(),
                reason: e,
            }),
        }
    }

    // 批量完成后一次性广播，而不是每条各广播一次。
    let _ = app.emit("task://update", app.state::<AppState>().snapshot());
    BatchAddResult { added, failed }
}

/// `add_url` / `add_urls` 共用的建任务逻辑。
/// 返回任务本体；`Err` 只在「极端情况下」出现（当前实现不会，为批量场景预留）。
pub(crate) fn add_url_inner(app: &AppHandle, url: &str) -> Result<Task, String> {
    let settings = {
        let state = app.state::<AppState>();
        state.settings.lock().map(|s| s.clone()).unwrap_or_else(|_| default_settings())
    };
    let output_dir = settings
        .get("outputDir")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| paths::app_data_root().join("downloads").to_string_lossy().into_owned());

    let id = state::unique_id("t-");
    let spec = runner::spec_from_settings(&settings, url, &id);

    let mut task = Task::new(
        id.clone(),
        url.to_string(),
        output_dir,
        // 显示的就是**实际会用的**那个表达式（含编码偏好），不是裸预设——
        // 否则任务详情里那行会与实际命令行对不上。
        spec.effective_format_expression(),
    );
    task.container = ytdlp_core::resolve_container(spec.container, &spec.embed)
        .as_str()
        .to_string();
    task.queue_hint = Some("等待探测".into());

    app.state::<AppState>().insert_task(task.clone());

    // 立即广播，再交给调度器——**不直接启动下载**：
    // 探测与下载是两个池，链路是 probe → (勾选) → queue → download。
    let _ = app.emit("task://update", app.state::<AppState>().snapshot());
    app.state::<Scheduler>().enqueue_probe(id);

    Ok(task)
}

/// 浏览器扩展推送的入口（ROADMAP §F19）：与 `add_url` 走同一条链路，
/// 只是多了合法性过滤与「推送源」标记。
///
/// 返回 `Ok(())` 表示已入队；`Err` 表示链接无效（推送服务据此回 400）。
pub(crate) fn push_url(app: &AppHandle, url: &str) -> Result<(), String> {
    let url = url.trim();
    if url.is_empty() || !ytdlp_core::is_valid_url(url) {
        return Err("不是有效链接".into());
    }
    // 去重：已经在任务列表里的 URL 不重复入队。
    let dup = app
        .state::<AppState>()
        .tasks
        .lock()
        .map(|tasks| tasks.iter().any(|t| t.url == url))
        .unwrap_or(false);
    if dup {
        return Ok(());
    }
    let _ = add_url_inner(app, url)?;
    logfile::info(format!("浏览器推送入队：{url}"));
    Ok(())
}

/// 播放列表勾选完成后开始下载。
///
/// 用 `--playlist-items` 把选集交给 yt-dlp，而不是在宿主侧为每集建一条任务——
/// 后者会打乱 `-f` 语义、也会让任务列表被几百条记录淹没（DESIGN §9）。
#[tauri::command]
fn start_playlist(app: AppHandle, id: String, indices: Vec<usize>) -> Result<(), String> {
    let items = ytdlp_core::playlist_items_spec(&indices);
    if items.is_none() {
        return Err("没有勾选任何条目".into());
    }

    let url = app
        .state::<AppState>()
        .with_task(&id, |t| {
            t.playlist_items = items.clone();
            t.state = "queued".into();
            t.queue_hint = Some("排队中".into());
            t.url.clone()
        })
        .ok_or_else(|| "任务不存在".to_string())?;

    let _ = app.emit("task://update", app.state::<AppState>().snapshot());
    app.state::<Scheduler>()
        .enqueue_download(id, ytdlp_core::host_of(&url));
    Ok(())
}

/// 调度器状态，供界面显示「探测中 / 排队中」数量。
#[tauri::command]
fn scheduler_stats(app: AppHandle) -> Value {
    let (probing, probe_queued, downloading, dl_queued) = app.state::<Scheduler>().stats();
    let (probe_limit, dl_limit, host_limit) = scheduler::limits(&app);
    // 数据库里的条数：与内存列表对照，能看出「有没有漏写库」
    let stored = app
        .state::<AppState>()
        .stored_count()
        .map(|n| n as i64)
        .unwrap_or(-1);
    json!({
        "probing": probing,
        "probeQueued": probe_queued,
        "downloading": downloading,
        "downloadQueued": dl_queued,
        "probeLimit": probe_limit,
        "downloadLimit": dl_limit,
        "perHostLimit": host_limit,
        "storedTasks": stored,
    })
}

#[tauri::command]
fn pause_task(app: AppHandle, id: String) {
    runner::cancel_task(&app, &id);
    // 从调度器里摘掉，否则槽位会被永久占用
    app.state::<Scheduler>().forget(&id);
    app.state::<AppState>().with_task(&id, |t| {
        t.state = "paused".into();
        t.post_process = None;
        t.progress.speed = None;
        t.progress.eta = None;
        t.queue_hint = None;
    });
    let _ = app.emit("task://update", app.state::<AppState>().snapshot());
}

#[tauri::command]
fn resume_task(app: AppHandle, id: String) {
    app.state::<AppState>().with_task(&id, |t| {
        t.state = "pending".into();
        t.error = None;
        t.queue_hint = Some("等待探测".into());
    });
    // 重新走一遍探测：设置可能已经变了（cookie/代理/格式），
    // 而且原任务的探测结果可能已过期。
    app.state::<Scheduler>().enqueue_probe(id);
    let _ = app.emit("task://update", app.state::<AppState>().snapshot());
}

/// 设置单个任务的 `-f` 表达式，并**立刻按新格式重下**。
///
/// 这是「可用格式」对话框的落点。之所以要重下而不是只存下来：
/// 存下来但什么都不发生，用户会以为按钮坏了——DESIGN 反复强调的
/// 「静默无反应」就是这么来的。确认按钮的文案已经写明会重新下载。
#[tauri::command]
fn set_task_format(app: AppHandle, id: String, expression: String) {
    let expr = expression.trim().to_string();
    // 空串 = **恢复成设置里的预设**，而不是留一个空表达式——
    // 否则任务详情里那行会显示成空白，用户以为坏了。
    let (override_, shown) = if expr.is_empty() {
        let settings = app
            .state::<AppState>()
            .settings
            .lock()
            .map(|s| s.clone())
            .unwrap_or(Value::Null);
        (
            None,
            ytdlp_core::preset_expression_with(
                &runner::preset_of(&settings),
                &runner::codec_of(&settings),
            ),
        )
    } else {
        (Some(expr.clone()), expr)
    };

    app.state::<AppState>().with_task(&id, |t| {
        t.format_expression = shown.clone();
        t.format_override = override_.clone();
    });
    // 走和「重新下载」完全一样的路径：先探测再下载。
    // 探测会顺带刷新格式表，用户能立刻看到新表达式带来的变化。
    app.state::<AppState>().with_task(&id, |t| {
        t.state = "pending".into();
        t.error = None;
        t.queue_hint = Some("等待探测".into());
    });
    app.state::<Scheduler>().enqueue_probe(id);
    let _ = app.emit("task://update", app.state::<AppState>().snapshot());
}

/// 把排队中的任务挪到队首（下一个开始）。不在排队时什么都不做。
#[tauri::command]
fn prioritize_task(app: AppHandle, id: String) -> bool {
    let moved = app.state::<Scheduler>().prioritize(&id);
    if moved {
        runner::update(&app, &id, |t| t.queue_hint = Some("已优先，下一个开始".into()));
    }
    moved
}

/// 设置单个任务的片段（`--download-sections`），并立刻按新设置重下。
///
/// `input` 为空 = 恢复下载完整视频。输入不合法时返回可读的原因，任务不动。
#[tauri::command]
fn set_task_sections(app: AppHandle, id: String, input: String) -> Result<Vec<String>, String> {
    let duration = app
        .state::<AppState>()
        .with_task(&id, |t| t.duration_sec)
        .ok_or("任务不存在")?;
    let sections = ytdlp_core::parse_sections(&input, duration)?;
    app.state::<AppState>().with_task(&id, |t| {
        t.sections = sections.clone();
        t.state = "pending".into();
        t.error = None;
        t.queue_hint = Some("等待探测".into());
    });
    app.state::<Scheduler>().enqueue_probe(id);
    let _ = app.emit("task://update", app.state::<AppState>().snapshot());
    Ok(sections)
}

/// 移除任务记录，并清理该任务的临时目录。
///
/// **不删除成品文件** —— 那是独立的「删除文件」动作（DESIGN §13）。
#[tauri::command(async)]
fn remove_record(app: AppHandle, id: String) {
    runner::cancel_task(&app, &id);
    app.state::<Scheduler>().forget(&id);
    // 用当前设置里的 temp 根，而不是默认根——否则改过 tempDir 的任务
    // 删记录后会留下整个临时目录。
    let root = temp_root_of(&app.state::<AppState>());
    let _ = std::fs::remove_dir_all(paths::task_temp_dir_in(&root, &id));
    app.state::<AppState>().remove_tasks(&[id]);
    let _ = app.emit("task://update", app.state::<AppState>().snapshot());
}

#[tauri::command(async)]
fn remove_many(app: AppHandle, ids: Vec<String>) {
    let root = temp_root_of(&app.state::<AppState>());
    for id in &ids {
        runner::cancel_task(&app, id);
        app.state::<Scheduler>().forget(id);
        let _ = std::fs::remove_dir_all(paths::task_temp_dir_in(&root, id));
    }
    app.state::<AppState>().remove_tasks(&ids);
    let _ = app.emit("task://update", app.state::<AppState>().snapshot());
}


#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // 把窗口夹到当前显示器能容纳的范围。
            //
            // ⚠️ 必须**基于 `tauri.conf.json` 里已经生效的尺寸**来夹，不能再写一份常量。
            // 原来这里硬编码 `1440x920`，于是改 conf 里的 `width`/`height` 完全没效果——
            // 排查了半天才发现在 setup 里被覆盖掉了。
            //
            // 只往下夹、不往上撑：用户配的小窗口不该被这里的下限放大。
            // 下限由 conf 里的 `minWidth`/`minHeight` 负责（Tauri 会交给系统约束）。
            if let Some(win) = app.get_webview_window("main") {
                if let Ok(Some(mon)) = win.current_monitor() {
                    let logical = mon.size().to_logical::<f64>(mon.scale_factor());
                    if let Ok(cur) = win.inner_size() {
                        let cur = cur.to_logical::<f64>(win.scale_factor().unwrap_or(1.0));
                        // 留出任务栏与窗口边框的余量
                        let w = cur.width.min(logical.width - 32.0);
                        let h = cur.height.min(logical.height - 72.0);
                        if w < cur.width || h < cur.height {
                            let _ = win.set_size(tauri::LogicalSize::new(w, h));
                        }
                    }
                }
                let _ = win.center();
            }

            let state = AppState::default();
            if let Ok(mut s) = state.settings.lock() {
                *s = normalize_settings(load_settings());
            }

            // 日志要在**任何可能出错的步骤之前**初始化，否则早期失败就没记录。
            // 放在数据目录下：便携模式下它就在程序目录里（DESIGN §5.6）。
            logfile::init(paths::app_data_root().join("logs"));
            let (data_root, portable) = paths::data_dir_info();
            logfile::info("================ 启动 ================");
            logfile::info(format!(
                "版本 {}  数据目录 {}  便携模式 {}",
                env!("CARGO_PKG_VERSION"),
                data_root.display(),
                if portable { "是" } else { "否" }
            ));
            match paths::resolve_ytdlp() {
                Some(p) => logfile::info(format!("yt-dlp: {}", p.display())),
                None => logfile::error(format!(
                    "没找到可用的 yt-dlp，候选：{}",
                    paths::diagnose_ytdlp()
                        .iter()
                        .map(|(p, e, u)| format!(
                            "{}[{}{}]",
                            p,
                            if *e { "存在" } else { "不存在" },
                            if *u { "可用" } else { "" }
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
            match paths::resolve_aria2c() {
                Some(p) => logfile::info(format!("aria2c: {}", p.display())),
                None => logfile::info("aria2c: 未找到（会用内置下载器）"),
            }
            match paths::resolve_ffmpeg() {
                Some(p) => logfile::info(format!("ffmpeg: {}", p.display())),
                None => logfile::warn("ffmpeg: 未找到（无法合并高画质音视频，可在设置里一键下载）"),
            }

            // 恢复下载历史，并把崩溃时残留的进行中任务重置为 paused（DESIGN §5.3）。
            if let Err(e) = state.init_db(&paths::db_file()) {
                // 数据库坏掉不该让应用起不来：空列表继续跑，用户还能重新下载
                logfile::error(format!("初始化任务数据库失败，本次以空历史启动：{e}"));
            }
            sweep_orphan_temp_dirs(&state);
            // 上次更新留下的 .old 此时进程已退出，可以安全删除（DESIGN §8）
            update::sweep_old_binaries();
            app.manage(state);
            app.manage(Scheduler::default());

            // 探测与下载由两个独立并发池调度（DESIGN §4）。
            scheduler::spawn(app.handle().clone());

            // 浏览器扩展一键推送（ROADMAP §F19）：本机回环端口接收扩展发来的 URL。
            push::spawn(app.handle().clone());

            app.manage(app_update::PendingUpdate::default());
            app.manage(afterall::AfterAll::default());
            afterall::spawn(app.handle().clone());
            app.manage(subscribe::Subscriptions::load());
            subscribe::spawn(app.handle().clone());
            if let Err(e) = tray::init(app.handle()) {
                logfile::error(format!("创建托盘图标失败：{e}"));
            }

            // 后台节流落库。
            //
            // 进度更新频率很高（实测每秒数条），**不能每次变更都写库**；
            // 这里每 3 秒把「变过的」和「删掉的」那几条在一个事务里写下去
            // （DESIGN §5.2）。只写变化项，不做全量重写。
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    let st = handle.state::<AppState>();
                    if st.has_pending() {
                        if let Err(e) = st.flush() {
                            logfile::error(format!("写入任务历史失败：{e}"));
                        }
                    }
                }
            });
            Ok(())
        })
        .on_window_event(tray::on_window_event)
        .invoke_handler(tauri::generate_handler![
            app_update::check_app_update,
            app_update::install_app_update,
            afterall::get_after_all,
            afterall::set_after_all,
            subscribe::list_subscriptions,
            subscribe::add_subscription,
            subscribe::remove_subscription,
            subscribe::set_subscription_enabled,
            subscribe::check_subscriptions,
            settings::get_settings,
            settings::save_settings,
            list_tasks,
            add_url,
            add_urls,
            start_playlist,
            scheduler_stats,
            pause_task,
            resume_task,
            set_task_format,
            set_task_sections,
            prioritize_task,
            remove_record,
            remove_many,
            files::remove_from_archive,
            files::delete_file,
            files::open_file,
            files::reveal_file,
            files::relocate_file,
            files::pick_folder,
            files::pick_file,
            files::disk_space_check,
            tools::probe_formats,
            tools::ytdlp_info,
            tools::aria2c_info,
            tools::ffmpeg_info,
            tools::install_ffmpeg,
            tools::list_cookie_profiles,
            tools::import_cookie_profile,
            tools::delete_cookie_profile,
            tools::inspect_cookie_file,
            tools::test_cookie_profile,
            tools::test_cookie_browser,
            tools::list_browsers,
            tools::detect_js_runtimes,
            tools::codec_choices,
            tools::data_dir,
            files::temp_dir_in_use,
            tools::format_presets,
            tools::preview_format_expression,
            tools::read_clipboard,
            tools::check_ytdlp_update,
            tools::apply_ytdlp_update,
            tools::test_proxy,
            tools::system_proxy,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
