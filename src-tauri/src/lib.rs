//! Tauri 2 外壳：命令面、状态装配与事件推送。
//!
//! 设计上刻意保持「薄」：所有参数构造与输出解析都在 `ytdlp-core` 里，
//! 这里只负责进程生命周期、持久化与前后端桥接。

mod clipboard;
mod cookies;
mod logfile;
mod net;
mod paths;
mod push;
mod runner;
mod scheduler;
mod shell;
mod state;
mod store;
mod sysproxy;
mod text;
mod update;

use serde_json::{json, Value};
use scheduler::Scheduler;
use state::{AppState, Task};
use tauri::{AppHandle, Emitter, Manager, State};

/// 与 `src/mock.ts` 的 `defaultSettings` 保持同构。
fn default_settings() -> Value {
    json!({
        // 必须是真实可用的目录：空串会让设置页与侧栏都显示空白
        "outputDir": paths::default_output_dir().to_string_lossy(),
        "tempDir": paths::default_temp_root().to_string_lossy(),
        "probeConcurrency": 6,
        "downloadConcurrency": 2,
        "perHostConcurrency": 1,
        "preset": "best",
        "maxHeight": 1080,
        "audioFormat": "mp3",
        "container": "auto",
        "embed": {
            "subs": false, "subLangs": ytdlp_core::DEFAULT_SUB_LANGS, "autoSubs": false,
            "keepSubFiles": false, "thumbnail": false, "keepThumbnailFile": false,
            "metadata": false, "chapters": false, "infoJson": false
        },
        "cookieMode": "none",
        "cookieFile": "",
        "cookieBrowser": "firefox",
        // ── 代理（对应 Windows「设置 → 网络和 Internet → 代理」那一页）──
        // none / system（读注册表）/ manual
        "proxyMode": "none",
        "proxyProtocol": "http",
        "proxyHost": "",
        "proxyPort": 8080,
        "proxyAuth": false,
        "proxyUser": "",
        "proxyPassword": "",
        // 不勾「记住」时密码只留在内存，写盘前会被抹掉（见 save_settings）
        "proxyRemember": true,
        // ── 按站点分流（ROADMAP §F18）──
        // per-host 规则表：{ host, proxy }。proxy 为 "" 或 "direct" 表示直连。
        // 匹配优先级 = 数组顺序（界面上排前面的优先）。
        "proxyRules": [],
        "aria2c": false,
        "archiveEnabled": false,
        "archivePath": paths::archive_file().to_string_lossy(),
        "limitRate": "",
        "filenameTemplate": "%(title).150B [%(id)s].%(ext)s",
        // 「优先选择」的编码，空 = 不指定。值必须是 yt-dlp 报出的编码名前缀
        // （`avc1` / `vp9` / `av01` / `mp4a` / `opus` / `vorbis`），
        // 白名单在 `runner::codec_of` 里把关（DESIGN §3.3）。
        "preferVcodec": "",
        "preferAcodec": "",
        // JS 运行时：留空表示自动检测。**不是可选优化**——
        // 不给的话 YouTube 会返回「需要重载页面」或只给 storyboard。
        // 注意这里**没有** `jsRemoteComponents`：界面上已经不提供那个开关，
        // 但 `runner::js_of` 仍然读它，好让需要的人改 config.json 打开。
        "jsRuntime": "",
        // 对 generic 提取器开启指纹模拟，过 Cloudflare 拦截。
        // **默认关**：yt-dlp 自己默认也不做（issue #11335），它的帮助文本
        // 明确警告强制模拟会拖慢速度、降低稳定性。撞上拦截时界面上会指过来。
        "impersonate": false,
        // ── 章节切分（ROADMAP §F15）──
        // 长视频按章节拆成多个文件。默认关：会显著增加后处理时间。
        "splitChapters": false,
        // ── 下载后动作（ROADMAP §F16）──
        // 完成后执行的命令（--exec）。留空 = 不执行。
        // ⚠️ 会以本机权限执行，界面必须做安全确认，绝不静默启用。
        "execCommand": "",
        // ── 系统通知（ROADMAP §F1）──
        // 任务进入终态时弹系统通知。skipped 是「已存在跳过」，不算坏事，默认不弹。
        "notifyOnComplete": true,
        "notifyOnFailure": true,
        "notifyOnSkip": false,
        // ── 剪贴板监听（ROADMAP §F4）──
        // 默认关：读剪贴板是敏感操作，且容易在用户复制别的文本时误弹。
        "watchClipboard": false,
        // ── 界面主题（ROADMAP §F20）──
        // light / dark / system。system = 跟随操作系统深浅色，前端监听
        // prefers-color-scheme 实时切换。
        "theme": "system",
        // ── 浏览器扩展一键推送（ROADMAP §F19）──
        // 是否启用本机 127.0.0.1:19090 接收端点。默认开：只监听回环地址，
        // 本机外的程序连不进来，风险很低。
        "browserPush": true
    })
}

fn load_settings() -> Value {
    std::fs::read_to_string(paths::settings_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(default_settings)
}

/// 递归补齐缺失的设置键。
///
/// `defaults` 的结构决定结果结构：对象逐键递归，其余以 `stored` 为准
/// （只在 `stored` 是 `null` 时回落到默认值）。
fn merge_defaults(defaults: Value, stored: Value) -> Value {
    match defaults {
        Value::Object(d) => match stored {
            Value::Object(mut u) => {
                for (k, dv) in d {
                    let v = match u.remove(&k) {
                        Some(uv) => merge_defaults(dv, uv),
                        None => dv,
                    };
                    u.insert(k, v);
                }
                Value::Object(u)
            }
            // 类型对不上（null、字符串…）时回到默认值，
            // 否则前端会拿到它没预期的类型
            _ => Value::Object(d),
        },
        other => {
            if stored.is_null() {
                other
            } else {
                stored
            }
        }
    }
}

/// 把早期的 `proxyEnabled` + `proxyUrl` 迁移成结构化的 `proxyMode` 配置。
///
/// 早期版本只有一个「使用代理 + 一个 URL 输入框」。升级后如果不管它，
/// 用户配好的代理会静默失效——那是「明明昨天还能下、今天全失败」的经典来源。
fn migrate_proxy(mut s: Value) -> Value {
    // 已经有新配置就什么都不做
    if s.get("proxyMode").is_some() {
        return s;
    }
    let Some(obj) = s.as_object_mut() else {
        return s;
    };

    let enabled = obj.get("proxyEnabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let url = obj
        .get("proxyUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let parsed = if enabled {
        ytdlp_core::parse_proxy_url(&url)
    } else {
        None
    };

    match parsed {
        Some(c) => {
            obj.insert("proxyMode".into(), Value::String("manual".into()));
            obj.insert("proxyProtocol".into(), Value::String(c.protocol.as_str().into()));
            obj.insert("proxyHost".into(), Value::String(c.host));
            obj.insert("proxyPort".into(), Value::from(c.port));
            if !c.user.is_empty() {
                obj.insert("proxyAuth".into(), Value::Bool(true));
                obj.insert("proxyUser".into(), Value::String(c.user));
                obj.insert("proxyPassword".into(), Value::String(c.password));
            }
        }
        None => {
            obj.insert("proxyMode".into(), Value::String("none".into()));
        }
    }
    s
}

/// 补全缺失或为空的设置项。
///
/// 两个真实踩过的坑：
/// 1. 早期版本把 `outputDir` 默认成空串，用户在设置页看到的是空栏、
///    侧栏也不显示路径——看起来像「没有设置项」。
/// 2. 后来新增 `jsRuntime` 键，**老配置里根本没有这个键**，前端
///    `s.jsRuntime.trim()` 抛 TypeError，整个设置面板白屏。逐个特判挡不住
///    下一个新键，所以这里直接按 `default_settings()` 递归补齐。
fn normalize_settings(s: Value) -> Value {
    // ⚠️ 迁移必须在补全默认值**之前**：补全会把 `proxyMode` 填成默认的 "none"，
    // 那样就再也分不清「老配置里压根没有这个键」和「用户真的选了不用代理」，
    // 升级时会把用户已经配好的代理静默丢掉。
    let s = migrate_proxy(s);
    let mut s = merge_defaults(default_settings(), s);
    // `proxyBypass` 已废弃：直连名单现在是 `proxy::LOCAL_BYPASS` 常量，用户不可配。
    // `merge_defaults` 只补键不删键，所以老配置里的这个键会一直留在 config.json 里，
    // 看起来像个能用的开关。这里显式丢掉，避免误导。
    if let Some(obj) = s.as_object_mut() {
        obj.remove("proxyBypass");
    }
    // 老配置里 `subLangs` 是旧默认值 `all,-live_chat`：它没排除 `danmaku`，而 B站弹幕
    // 既嵌不进 mkv（XML，ffmpeg 读不了），走 aria2c 又必然报错（裸 deflate 解不开），
    // 会把整条任务判失败。只迁移**恰好等于旧默认值**的配置——用户自己填过的一律不动。
    // 放在 `merge_defaults` 之后：此时缺键已被补成新默认值，剩下的就是真正的存量。
    if s.pointer("/embed/subLangs")
        .and_then(|v| v.as_str())
        .map(|v| v.trim() == ytdlp_core::LEGACY_DEFAULT_SUB_LANGS)
        .unwrap_or(false)
    {
        if let Some(embed) = s.get_mut("embed").and_then(Value::as_object_mut) {
            embed.insert(
                "subLangs".into(),
                Value::String(ytdlp_core::DEFAULT_SUB_LANGS.to_string()),
            );
        }
    }
    let blank = s
        .get("outputDir")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().is_empty())
        .unwrap_or(true);
    if blank {
        if let Some(obj) = s.as_object_mut() {
            obj.insert(
                "outputDir".into(),
                Value::String(paths::default_output_dir().to_string_lossy().into_owned()),
            );
        }
    }
    s
}

/// 当前设置下的 temp 根目录（可由 `tempDir` 覆盖，见 `paths::temp_root`）。
fn temp_root_of(state: &AppState) -> std::path::PathBuf {
    let settings = state
        .settings
        .lock()
        .map(|s| s.clone())
        .unwrap_or(Value::Null);
    paths::temp_root(&settings)
}

/// 回收不再对应任何任务的临时条目。
///
/// 删除任务时 `remove_dir_all` 可能因目录被占用而失败，留下空壳目录无限累积。
/// 启动时统一清理。
///
/// **保留条件**：目录名是一个「还有续传价值」的任务 id。
/// - 终态（`completed` / `skipped`）**不保留**：成品已经落到输出目录，
///   yt-dlp 不会再续传任何东西，目录里只剩宿主自己写的 `filepath.txt`
///   （那个路径早已存进 `Task::filepath`，删掉不影响「删除文件」）。
/// - 其余状态（`paused` / `failed` / …）**必须保留**：`.part` 在里面，
///   删了就是静默失去断点续传（DESIGN §5.1）。
///
/// 两个根都要扫：`tempDir` 是可以改的，改过之后旧根里还会留着历史碎片。
/// temp 根下**只应该有任务目录**，所以名字对不上的一律清掉——
/// 包括散落的文件（实测手工跑 yt-dlp 时会在根下丢下成品文件，永远没人回收）。
/// 只在启动时调用，此刻没有任何任务在跑，不存在竞态。
fn sweep_orphan_temp_dirs(state: &AppState) {
    let keep: std::collections::HashSet<String> = state
        .snapshot()
        .into_iter()
        .filter(has_resumable_temp)
        .map(|t| t.id)
        .collect();
    let mut roots = vec![temp_root_of(state), paths::default_temp_root()];
    roots.dedup();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = p.is_dir();
            if is_dir && keep.contains(&name) && !is_empty_dir(&p) {
                continue;
            }
            let _ = if is_dir {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
        }
    }
}

/// 这个任务的 temp 目录里还有没有值得续传的东西。
///
/// 终态即「下载流程已经走完」——`completed` 与 `skipped` 都返回 exit 0，
/// 但两者都不会再碰 temp 目录（DESIGN §12.1）。
fn has_resumable_temp(t: &Task) -> bool {
    !matches!(t.state.as_str(), "completed" | "skipped")
}

/// 目录存在且一个条目都没有。读不到就当「不空」——宁可留着也不误删。
fn is_empty_dir(p: &std::path::Path) -> bool {
    match std::fs::read_dir(p) {
        Ok(mut it) => it.next().is_none(),
        Err(_) => false,
    }
}

// ─────────────────────────── 命令 ───────────────────────────

#[tauri::command]
fn get_settings(state: State<AppState>) -> Value {
    state
        .settings
        .lock()
        .map(|s| s.clone())
        .unwrap_or_else(|_| default_settings())
}

#[tauri::command(async)]
fn save_settings(state: State<AppState>, settings: Value) -> Result<(), String> {
    // ⚠️ 归一化后再落内存和磁盘。`get_settings` 是归一化过的，如果保存路径
    // 不归一化，「读到的」和「存进去的」就会不一致：新增设置键时前端拿到的是
    // `undefined`（历史上就是这样把整个设置面板打成白屏的）。
    // 这里也保证了 `normalize_settings` 里清掉的废弃键不会又被写回去。
    let settings = normalize_settings(settings);

    // ⚠️ 改 `tempDir` 会让**已经存在的 .part 找不到**：新目录里没有它们，
    // yt-dlp 会从头下，旧碎片永远留在旧目录里（那种目录只在启动时按**当前**
    // tempRoot 清理，换了根就再也扫不到）。
    //
    // 界面已经把输入框禁掉了，这里再加一道：真发生了至少要在日志里看得见，
    // 否则「为什么断点续传没了」永远查不出来。
    {
        let old = state
            .settings
            .lock()
            .map(|s| s.get("tempDir").and_then(|v| v.as_str()).unwrap_or("").to_string())
            .unwrap_or_default();
        let new = settings
            .get("tempDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if old != new {
            let busy = temp_dir_in_use(state.clone());
            if busy.get("inUse").and_then(|v| v.as_bool()).unwrap_or(false) {
                logfile::warn(format!(
                    "tempDir 从「{old}」改成「{new}」，但有 {} 个任务的临时目录里还有可续传文件——\
                     这些任务的断点续传会失效",
                    busy.get("count").and_then(|v| v.as_u64()).unwrap_or(0)
                ));
            } else {
                logfile::info(format!("tempDir 改为「{new}」"));
            }
        }
    }

    if let Ok(mut s) = state.settings.lock() {
        *s = settings.clone();
    }

    // 「记住密码」没勾时，密码只留在内存里，**写盘前抹掉**。
    // 明文密码落在 config.json 里是用户明确表示不想要的事。
    let mut on_disk = settings;
    let remember = on_disk
        .get("proxyRemember")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if !remember {
        if let Some(o) = on_disk.as_object_mut() {
            o.insert("proxyPassword".into(), Value::String(String::new()));
        }
    }

    let dir = paths::app_data_root();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&on_disk).map_err(|e| e.to_string())?;
    std::fs::write(paths::settings_file(), text).map_err(|e| e.to_string())
}

/// 读一次 Windows 的系统代理设置，供设置页展示。
///
/// 只是**展示**：`跟随系统代理` 模式在每次探测/下载时会重新读，
/// 免得用户改了系统代理还得回来点一下。
#[tauri::command(async)]
fn system_proxy() -> Value {
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
fn add_url_inner(app: &AppHandle, url: &str) -> Result<Task, String> {
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

/// 从归档文本里删掉含 `ids` 的行，返回（新文本, 删除条数）。
///
/// 抽成纯函数是因为 BOM 这个坑只有单测拦得住：`read_to_string` 不去 BOM，
/// 而 yt-dlp 读归档用的是 `encoding='utf-8'`（**同样不认 BOM**），于是归档
/// 首行会静默失效——实测该视频会被重新下载。写回时一律不写 BOM。
///
/// `ids` 是**媒体 id**（归档行 `<提取器> <媒体id>` 里的后半段），按**整 token**精确匹配：
/// 不做子串匹配，否则 `BV116a364EE1` 会误删 `BV116a364EE11` 之类的前缀行。
fn strip_archive_ids(content: &str, ids: &[String]) -> (String, usize) {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut removed = 0usize;
    let kept: Vec<&str> = content
        .lines()
        .filter(|line| {
            let hit = line
                .split_whitespace()
                .any(|tok| ids.iter().any(|id| id == tok));
            if hit {
                removed += 1;
            }
            !hit
        })
        .collect();
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    (out, removed)
}

/// 任务 id → 媒体 id 的反查（供「从归档移除」用）。
///
/// ⚠️ 归档里存的是 `<提取器> <媒体id>`，**不是任务 id**（`t-…`）。用任务 id 去匹配
/// 永远删不掉——实测这就是「从归档移除后重新下载仍报已在归档中」的根因。抽成纯函数
/// 好单测；老记录没有 `videoId` 时原样退回（至少不会把 `t-…` 硬塞进匹配）。
fn archive_targets(tasks: &[Task], ids: &[String]) -> Vec<String> {
    ids.iter()
        .map(|id| {
            tasks
                .iter()
                .find(|t| &t.id == id)
                .and_then(|t| t.video_id.clone())
                .unwrap_or_else(|| id.clone())
        })
        .collect()
}

/// 从 download-archive 移除条目，使视频可重新下载。
///
/// ⚠️ 仅此一步**不足以**重新下载：成品文件仍在磁盘时 yt-dlp 会跳过并返回 exit=0
/// （DESIGN §13.2）。
#[tauri::command(async)]
fn remove_from_archive(state: State<AppState>, ids: Vec<String>) -> Result<usize, String> {
    let targets = {
        let tasks = state.tasks.lock().map_err(|e| e.to_string())?;
        archive_targets(&tasks, &ids)
    };

    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or(Value::Null);
    let archive = settings
        .get("archivePath")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(paths::archive_file);

    if !archive.exists() {
        return Ok(0);
    }

    // 归档写入在 yt-dlp 侧有 LockFileEx 独占锁；宿主改写必须容忍占用并重试。
    // ⚠️ `removed` 必须在循环**内**统计：写失败重试时如果累加，会把同一条
    // 重复计数，界面上报出「移除了 2 条」而实际只删了 1 条。
    for _attempt in 0..5 {
        let content = match std::fs::read_to_string(&archive) {
            Ok(c) => c,
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(120));
                continue;
            }
        };
        let (out, removed) = strip_archive_ids(&content, &targets);
        match std::fs::write(&archive, out) {
            Ok(_) => return Ok(removed),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(120)),
        }
    }
    Err("归档文件被占用，无法写入".into())
}

/// 删除单条记录的成品文件。**独立动作，永不自动执行**（DESIGN §13.2）。
#[tauri::command(async)]
fn delete_file(state: State<AppState>, id: String) -> Result<(), String> {
    let path = {
        let tasks = state.tasks.lock().map_err(|e| e.to_string())?;
        tasks
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.filepath.clone())
    };
    let Some(p) = path else {
        return Err("该任务没有已落地的文件".into());
    };
    match std::fs::remove_file(&p) {
        Ok(_) => {
            state.with_task(&id, |t| t.filepath = None);
            Ok(())
        }
        // 文件可能正被播放器占用 —— 必须明确报错而不是静默失败。
        Err(e) => Err(format!("删除失败（文件可能正被占用）：{e}")),
    }
}

/// 移动 / 重命名已下载的成品文件（ROADMAP §F5）。
///
/// `new_dir`：移动到目标目录（保留原文件名）；`new_name`：重命名（留在原目录）。
/// 两者可同时给——先移动再改名。**绝不静默覆盖**：目标已存在就报错。
/// 只允许 completed / skipped 状态；下载中的任务文件句柄被占用，拒绝。
#[tauri::command(async)]
fn relocate_file(
    app: AppHandle,
    id: String,
    new_dir: Option<String>,
    new_name: Option<String>,
) -> Result<String, String> {
    let (src, output_dir, state_kind) = {
        let st = app.state::<AppState>();
        let tasks = st.tasks.lock().map_err(|e| e.to_string())?;
        let t = tasks.iter().find(|t| t.id == id).ok_or("任务不存在")?;
        let src = t.filepath.clone().ok_or("该任务没有已落地的文件")?;
        (src, t.output_dir.clone(), t.state.clone())
    };

    if state_kind == "downloading" || state_kind == "postprocessing" || state_kind == "paused" {
        return Err("任务正在下载或后处理，不能移动文件".into());
    }

    let src_path = std::path::PathBuf::from(&src);
    if !src_path.exists() {
        return Err(format!("源文件不存在：{src}"));
    }

    let file_name = new_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            src_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
        });

    let dir = new_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(&output_dir));

    let dst = dir.join(file_name);
    if dst == src_path {
        return Ok(src); // 没变化，不报错
    }
    if dst.exists() {
        return Err(format!("目标已存在，不会覆盖：{}", dst.display()));
    }
    // 目标目录可能不存在（比如用户手填了一个新目录），先建好。
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("无法创建目标目录：{e}"))?;
    }

    std::fs::rename(&src_path, &dst).map_err(|e| {
        format!("移动失败（文件可能正被占用）：{e}")
    })?;

    let new_path = dst.to_string_lossy().into_owned();
    app.state::<AppState>().with_task(&id, |t| {
        t.filepath = Some(new_path.clone());
        t.output_dir = dir.to_string_lossy().into_owned();
    });
    let _ = app.emit("task://update", app.state::<AppState>().snapshot());

    Ok(new_path)
}

/// 用系统默认程序打开已下载的文件（行双击 / 详情里的「打开文件」）。
#[tauri::command(async)]
fn open_file(path: String) -> Result<(), String> {
    shell::open_file(&path)
}
/// 在资源管理器中选中该文件（而不是打开它）。
#[tauri::command(async)]
fn reveal_file(path: String) -> Result<(), String> {
    shell::reveal_file(&path)
}

/// 弹原生「选择文件夹」对话框。返回 `None` 表示用户取消。
///
/// 命令声明成 `async` + `spawn_blocking`：对话框是模态阻塞的，
/// 跑在主线程上会把窗口和消息循环一起卡死。
#[tauri::command]
async fn pick_folder(initial: Option<String>) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || shell::pick_folder(initial.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

/// 弹原生「选择文件」对话框。返回 `None` 表示用户取消。
#[tauri::command]
async fn pick_file(initial: Option<String>) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || shell::pick_file(initial.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

/// 磁盘空间预估（ROADMAP §F6）。
///
/// 把「进行中/排队中」任务的预估大小求和，与输出目录所在盘的剩余空间对比，
/// 返回是否需要提醒。**只警告不硬拦**：预估值可能偏小（合并/嵌入后更大），
/// 也可能整块缺失（拿不到就是 None），所以由前端提示、用户决定继续与否。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DiskSpaceReport {
    /// 输出目录所在卷的剩余可用字节；拿不到为 null。
    free_bytes: Option<u64>,
    /// 所有待下载任务的预估大小之和；任何一条拿不到就为 null（保守，不误判）。
    needed_bytes: Option<u64>,
    /// 是否需要提醒（剩余 < 预估 × 1.2，留 20% 余量）。
    warn: bool,
}

#[tauri::command]
fn disk_space_check(state: State<AppState>) -> DiskSpaceReport {
    use crate::state::ACTIVE_STATES;
    let tasks = state.tasks.lock().map(|t| t.clone()).unwrap_or_default();

    // 待下载 = 还没落地的那些（含进行中/排队/探测/待选择）。
    let pending: Vec<&Task> = tasks
        .iter()
        .filter(|t| ACTIVE_STATES.contains(&t.state.as_str()))
        .collect();

    let output_dir = state
        .settings
        .lock()
        .ok()
        .and_then(|s| {
            s.get("outputDir")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| paths::default_output_dir().to_string_lossy().into_owned());

    let free_bytes = shell::disk_free_bytes(&output_dir);

    // 所有待下载任务都有 sizeEstimate 才求和；任一条 None 就整体 None——
    // 否则会低估所需空间，把「放不下」误判成「放得下」。
    let mut sum: Option<u64> = Some(0);
    for t in &pending {
        match t.size_estimate {
            Some(v) => sum = sum.map(|s| s.saturating_add(v)),
            None => {
                sum = None;
                break;
            }
        }
    }

    let warn = match (free_bytes, sum) {
        (Some(free), Some(need)) => {
            let need_with_margin = need.saturating_mul(12).checked_div(10).unwrap_or(u64::MAX);
            free < need_with_margin
        }
        _ => false,
    };

    DiskSpaceReport {
        free_bytes,
        needed_bytes: sum,
        warn,
    }
}

/// 探测可用格式。
///
/// 返回**解析后的** `MediaInfo`（camelCase，且已滤掉 storyboard 之类的非媒体条目），
/// 而不是原始 JSON——否则前端要再写一份 snake_case → camelCase 的字段映射，
/// 两条解析路径必然分叉（DESIGN §3 的老问题）。
#[tauri::command]
async fn probe_formats(
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
fn list_cookie_profiles() -> Vec<cookies::CookieProfile> {
    cookies::list()
}

/// 导入一份 cookies.txt。**先校验格式再落盘**，格式错误带行号返回。
#[tauri::command(async)]
fn import_cookie_profile(
    name: String,
    content: String,
    origin: String,
) -> Result<cookies::CookieProfile, String> {
    cookies::save(&name, &content, &origin)
}

#[tauri::command(async)]
fn delete_cookie_profile(id: String) -> Result<(), String> {
    cookies::remove(&id)
}

/// 导入前预览：读一份外部 cookies.txt 并校验，返回 cookie 条数。
#[tauri::command(async)]
fn inspect_cookie_file(path: String) -> Result<usize, String> {
    let content = std::fs::read_to_string(&path).map_err(|e| format!("读取失败：{e}"))?;
    ytdlp_core::validate_netscape(&content).map_err(|e| e.message())
}

/// 预检已导入的 profile 是否仍可用（文件可能被外部删掉或改坏）。
#[tauri::command(async)]
fn test_cookie_profile(id: String) -> Value {
    let p = cookies::check_profile(&id);
    json!({ "ok": p.is_ok(), "summary": p.summary() })
}

/// 预检「从浏览器读取」。会真的跑一次 yt-dlp 并分类失败原因。
#[tauri::command]
async fn test_cookie_browser(browser: String) -> Value {
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
fn list_browsers() -> Vec<cookies::BrowserChoice> {
    cookies::list_browsers()
}

// ─────────────────── yt-dlp 自更新（DESIGN §8）───────────────────
//
// 三条更新链路里唯一不需要重新发版宿主的那条。
// ureq 是阻塞的，用 spawn_blocking 包起来，别堵住主线程。

#[tauri::command]
async fn check_ytdlp_update(state: State<'_, AppState>) -> Result<Value, String> {
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or(Value::Null);
    let proxy = runner::proxy_of(&settings);
    let info = tauri::async_runtime::spawn_blocking(move || update::check(proxy.as_deref()))
        .await
        .map_err(|e| e.to_string())??;
    serde_json::to_value(info).map_err(|e| e.to_string())
}

#[tauri::command]
async fn apply_ytdlp_update(state: State<'_, AppState>) -> Result<String, String> {
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
async fn test_proxy(state: State<'_, AppState>) -> Result<Value, String> {
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
fn detect_js_runtimes() -> Value {
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
fn codec_choices() -> Value {
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
fn format_presets(state: State<AppState>) -> Value {
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
fn preview_format_expression(settings: Value) -> String {
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
fn read_clipboard() -> Result<String, String> {
    clipboard::read_text()
}

/// 现在改 `tempDir` 会不会让续传失效？
///
/// ## 判据是**磁盘上有没有可续传的文件**，不是任务状态
///
/// 任务状态只能说明「流程没走完」；真正会丢的是 temp 目录里的 `.part` 碎片。
/// 按状态判会把一堆早就下完、目录里只剩 `filepath.txt` 的任务也算进来，
/// 于是临时目录被永久锁死——那种「明明是空的却不让改」最招人烦。
///
/// `filepath.txt` 是宿主自己写的完成标记，不算可续传内容。
#[tauri::command(async)]
fn temp_dir_in_use(state: State<AppState>) -> Value {
    let settings = state
        .settings
        .lock()
        .map(|s| s.clone())
        .unwrap_or(Value::Null);
    let root = paths::temp_root(&settings);

    let mut busy: Vec<String> = Vec::new();
    for t in state.snapshot() {
        let dir = paths::task_temp_dir_in(&root, &t.id);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let has_resume = entries.flatten().any(|e| e.file_name() != "filepath.txt");
        if has_resume {
            busy.push(t.id);
        }
    }

    json!({
        "inUse": !busy.is_empty(),
        "count": busy.len(),
        "taskIds": busy,
    })
}

/// 当前数据目录（设置、历史、cookie 都在这儿）+ 是否便携模式。
///
/// 用户最常问的就是「我的历史存哪了」，而 `%APPDATA%` 在资源管理器里
/// 默认是隐藏的——所以直接把路径摆到设置页上。
#[tauri::command]
fn data_dir() -> Value {
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
fn aria2c_info() -> Value {
    let (path, version) = paths::aria2c_status();
    json!({
        "found": path.is_some(),
        "path": path,
        "version": version,
    })
}

/// yt-dlp 可执行文件信息，供设置页展示与「检查更新」使用。
#[tauri::command]
async fn ytdlp_info(state: State<'_, AppState>) -> Result<Value, String> {
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
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
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            list_tasks,
            add_url,
            add_urls,
            start_playlist,
            scheduler_stats,
            pause_task,
            resume_task,
            set_task_format,
            remove_record,
            remove_many,
            remove_from_archive,
            delete_file,
            open_file,
            reveal_file,
            relocate_file,
            pick_folder,
            pick_file,
            disk_space_check,
            probe_formats,
            ytdlp_info,
            aria2c_info,
            list_cookie_profiles,
            import_cookie_profile,
            delete_cookie_profile,
            inspect_cookie_file,
            test_cookie_profile,
            test_cookie_browser,
            list_browsers,
            detect_js_runtimes,
            codec_choices,
            data_dir,
            temp_dir_in_use,
            format_presets,
            preview_format_expression,
            read_clipboard,
            check_ytdlp_update,
            apply_ytdlp_update,
            test_proxy,
            system_proxy,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 老配置里没有新键 → 前端 `s.jsRuntime.trim()` 抛 TypeError、设置面板白屏。
    /// 这个测试锁住「缺失键必须被补齐」。
    #[test]
    fn merge_defaults_fills_missing_keys() {
        let defaults = json!({ "a": "", "jsRuntime": "" });
        let stored = json!({ "a": "keep" });
        let merged = merge_defaults(defaults, stored);

        assert_eq!(merged["a"], json!("keep"));
        assert_eq!(merged["jsRuntime"], json!(""));
    }

    /// 界面不再暴露 `jsRemoteComponents`，但它**仍是从 config.json 可开的逃生门**：
    /// 一旦它回到 `default_settings`，设置页就会重新显示一个没人改的勾选框。
    #[test]
    fn remote_components_is_not_a_default_setting() {
        let s = normalize_settings(json!({}));
        assert!(s.get("jsRemoteComponents").is_none(), "实际: {s}");
        // 手工写进去的值必须被保留，否则逃生门形同虚设。
        let s = normalize_settings(json!({ "jsRemoteComponents": true }));
        assert_eq!(s["jsRemoteComponents"], json!(true));
    }

    /// 旧默认 `all,-live_chat` 必须被迁成含 `-danmaku` 的新默认；用户自定义值原样保留。
    #[test]
    fn sub_langs_migrates_legacy_default_only() {
        // 恰好等于旧默认 → 迁移
        let s = normalize_settings(json!({ "embed": { "subs": true, "subLangs": "all,-live_chat" } }));
        assert_eq!(
            s["embed"]["subLangs"], json!(ytdlp_core::DEFAULT_SUB_LANGS),
            "实际: {}", s["embed"]["subLangs"]
        );
        // 缺键 → 补成新默认
        let s = normalize_settings(json!({}));
        assert_eq!(s["embed"]["subLangs"], json!(ytdlp_core::DEFAULT_SUB_LANGS));
        // 用户自定义 → 不动（这是逃生门）
        let s = normalize_settings(json!({ "embed": { "subLangs": "zh-Hans,en" } }));
        assert_eq!(s["embed"]["subLangs"], json!("zh-Hans,en"));
        let s = normalize_settings(json!({ "embed": { "subLangs": "all,-live_chat,danmaku" } }));
        assert_eq!(s["embed"]["subLangs"], json!("all,-live_chat,danmaku"));
    }

    #[test]
    fn merge_defaults_recurses_into_nested_objects() {
        let defaults = json!({ "embed": { "subs": false, "thumbnail": false } });
        let stored = json!({ "embed": { "subs": true } });
        let merged = merge_defaults(defaults, stored);

        assert_eq!(merged["embed"]["subs"], json!(true));
        assert_eq!(merged["embed"]["thumbnail"], json!(false));
    }

    /// 类型对不上时不能把用户的值吞掉，也不能让前端拿到错类型。
    #[test]
    fn merge_defaults_keeps_stored_values() {
        let merged = merge_defaults(json!({ "n": 1, "s": "x" }), json!({ "n": 7, "s": "y" }));
        assert_eq!(merged["n"], json!(7));
        assert_eq!(merged["s"], json!("y"));
    }

    #[test]
    fn merge_defaults_replaces_null_with_object_default() {
        let merged = merge_defaults(json!({ "embed": { "subs": false } }), json!({ "embed": null }));
        assert_eq!(merged["embed"]["subs"], json!(false));
    }

    #[test]
    fn merge_defaults_non_object_stored_falls_back_to_defaults() {
        let merged = merge_defaults(json!({ "a": 1 }), json!("not an object"));
        assert_eq!(merged["a"], json!(1));
    }

    /// 非对象默认值 + `null` 存量 → 用默认值；非 null 存量 → 原样保留。
    #[test]
    fn merge_defaults_scalar_rules() {
        assert_eq!(merge_defaults(json!(5), json!(null)), json!(5));
        assert_eq!(merge_defaults(json!(5), json!(9)), json!(9));
    }

    fn task_in_state(state: &str) -> Task {        let mut t = Task::new(
            "t-1".into(),
            "https://example.com/v".into(),
            ".".into(),
            "bv*+ba/b".into(),
        );
        t.state = state.into();
        t
    }

    /// 终态任务没有可续传的东西，temp 目录该被回收；
    /// 其余状态必须保留 `.part`，否则断点续传静默失效（DESIGN §5.1）。
    #[test]
    fn only_non_terminal_states_keep_their_temp_dir() {
        for state in ["completed", "skipped"] {
            assert!(!has_resumable_temp(&task_in_state(state)), "{state} 应回收");
        }
        for state in [
            "pending",
            "probing",
            "downloading",
            "postprocessing",
            "paused",
            "failed",
            "canceled",
        ] {
            assert!(has_resumable_temp(&task_in_state(state)), "{state} 应保留");
        }
    }

    // ───────── 代理：旧配置迁移 ─────────

    /// **升级不能把用户配好的代理丢掉**——否则表现是「昨天还能下，今天全失败」。
    #[test]
    fn legacy_enabled_proxy_migrates_to_manual() {
        let s = normalize_settings(json!({
            "proxyEnabled": true,
            "proxyUrl": "http://127.0.0.1:7897",
        }));
        assert_eq!(s["proxyMode"], json!("manual"));
        assert_eq!(s["proxyHost"], json!("127.0.0.1"));
        assert_eq!(s["proxyPort"], json!(7897));
        assert_eq!(s["proxyProtocol"], json!("http"));
    }

    #[test]
    fn legacy_disabled_proxy_migrates_to_none() {
        let s = normalize_settings(json!({ "proxyEnabled": false, "proxyUrl": "http://h:1" }));
        assert_eq!(s["proxyMode"], json!("none"));
    }

    /// 勾了「使用代理」但地址是空的/填坏的 → 退回不用代理，
    /// 而不是留一个 manual + 空主机（那会让每次请求都失败）。
    #[test]
    fn legacy_enabled_but_unparseable_falls_back_to_none() {
        for bad in ["", "   ", "http://:8080"] {
            let s = normalize_settings(json!({ "proxyEnabled": true, "proxyUrl": bad }));
            assert_eq!(s["proxyMode"], json!("none"), "输入: {bad:?}");
        }
    }

    #[test]
    fn legacy_proxy_with_credentials_migrates_auth() {
        let s = normalize_settings(json!({
            "proxyEnabled": true,
            "proxyUrl": "socks5://u:p%40w@h:1080",
        }));
        assert_eq!(s["proxyMode"], json!("manual"));
        assert_eq!(s["proxyProtocol"], json!("socks5h"));
        assert_eq!(s["proxyAuth"], json!(true));
        assert_eq!(s["proxyUser"], json!("u"));
        assert_eq!(s["proxyPassword"], json!("p@w"));
    }

    /// 已经是新配置时，迁移必须**原样放过**——不能把用户选的 none 又改回 manual。
    #[test]
    fn migration_does_not_touch_new_config() {
        let s = normalize_settings(json!({
            "proxyMode": "none",
            "proxyEnabled": true,
            "proxyUrl": "http://127.0.0.1:7897",
        }));
        assert_eq!(s["proxyMode"], json!("none"));
    }

    /// 绕过列表已改为**内建常量**，设置里不该再出现这个键——
    /// 留着它意味着界面上又会冒出一个没人改的输入框。
    #[test]
    fn proxy_bypass_is_no_longer_a_setting() {
        let s = normalize_settings(json!({}));
        assert!(s.get("proxyBypass").is_none(), "实际: {s}");

        // 老配置里残留的键要被清掉，否则 config.json 里一直挂着一个
        // 看起来能用的开关（`merge_defaults` 只补键不删键）。
        let s = normalize_settings(json!({ "proxyBypass": "example.com", "proxyMode": "manual" }));
        assert!(s.get("proxyBypass").is_none(), "实际: {s}");
    }

    /// 归档首行的 BOM 会静默废掉那一条记录（见 `strip_archive_ids` 的注释）。
    ///
    /// `ids` 现在传的是**媒体 id**（归档行 `<提取器> <媒体id>` 的后半段），
    /// 按整 token 精确匹配——这正是 §13.3 那个 bug 的修法。
    #[test]
    fn archive_removal_strips_bom_and_never_writes_one() {
        let ids = vec!["BV1dK93BxESx".to_string()];
        let (out, removed) =
            strip_archive_ids("\u{feff}bilibili BV1dK93BxESx\nyoutube tW34TyACBIQ\n", &ids);
        assert_eq!(removed, 1);
        assert_eq!(out, "youtube tW34TyACBIQ\n");
        assert!(!out.starts_with('\u{feff}'));

        // 没有 BOM 时行为不变
        let (out, removed) = strip_archive_ids("bilibili BV1dK93BxESx\n", &ids);
        assert_eq!((out.as_str(), removed), ("", 1));

        // 删空之后不该留一个空行
        let (out, removed) = strip_archive_ids("youtube tW34TyACBIQ\n", &ids);
        assert_eq!((out.as_str(), removed), ("youtube tW34TyACBIQ\n", 0));
    }

    /// §13.3：媒体 id 是**整 token**匹配，不能是子串——否则前缀 id 会误删别的行。
    #[test]
    fn archive_removal_matches_whole_token_not_substring() {
        let ids = vec!["BV116a364EE1".to_string()];
        // 第二行的 id 是前缀关系（BV116a364EE11），不能被子串匹配误删
        let (out, removed) = strip_archive_ids(
            "bilibili BV116a364EE1\nyoutube BV116a364EE11\n",
            &ids,
        );
        assert_eq!(removed, 1, "只删精确匹配的那一条");
        assert_eq!(out, "youtube BV116a364EE11\n");
    }

    /// §13.3：任务 id → 媒体 id 反查。归档里没有任务 id，拿任务 id 去匹配永远删不掉。
    #[test]
    fn archive_targets_resolves_task_id_to_media_id() {
        let mut a = Task::new("t-1".into(), "u".into(), ".".into(), "bv*+ba/b".into());
        a.video_id = Some("BV116a364EE1".into());
        let mut b = Task::new("t-2".into(), "u".into(), ".".into(), "bv*+ba/b".into());
        b.video_id = Some("tW34TyACBIQ".into());
        // 老记录没有 video_id → 原样退回
        let c = Task::new("t-3".into(), "u".into(), ".".into(), "bv*+ba/b".into());

        let tasks = vec![a, b, c];
        assert_eq!(
            archive_targets(&tasks, &["t-1".to_string(), "t-2".to_string()]),
            vec!["BV116a364EE1".to_string(), "tW34TyACBIQ".to_string()]
        );
        // 未知 id 原样退回
        assert_eq!(
            archive_targets(&tasks, &["t-missing".to_string()]),
            vec!["t-missing".to_string()]
        );
    }
}
