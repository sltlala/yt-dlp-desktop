//! 临时目录回收、下载归档，以及对成品文件的操作（打开、移动、删除）。

use crate::state::{AppState, Task};
use crate::{paths, runner, shell};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};


/// 当前设置下的 temp 根目录（可由 `tempDir` 覆盖，见 `paths::temp_root`）。
pub fn temp_root_of(state: &AppState) -> std::path::PathBuf {
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
pub fn sweep_orphan_temp_dirs(state: &AppState) {
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
pub fn has_resumable_temp(t: &Task) -> bool {
    !matches!(t.state.as_str(), "completed" | "skipped")
}

/// 目录存在且一个条目都没有。读不到就当「不空」——宁可留着也不误删。
pub fn is_empty_dir(p: &std::path::Path) -> bool {
    match std::fs::read_dir(p) {
        Ok(mut it) => it.next().is_none(),
        Err(_) => false,
    }
}

/// 从归档文本里删掉含 `ids` 的行，返回（新文本, 删除条数）。
///
/// 抽成纯函数是因为 BOM 这个坑只有单测拦得住：`read_to_string` 不去 BOM，
/// 而 yt-dlp 读归档用的是 `encoding='utf-8'`（**同样不认 BOM**），于是归档
/// 首行会静默失效——实测该视频会被重新下载。写回时一律不写 BOM。
///
/// `ids` 是**媒体 id**（归档行 `<提取器> <媒体id>` 里的后半段），按**整 token**精确匹配：
/// 不做子串匹配，否则 `BV116a364EE1` 会误删 `BV116a364EE11` 之类的前缀行。
pub fn strip_archive_ids(content: &str, ids: &[String]) -> (String, usize) {
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
pub fn archive_targets(tasks: &[Task], ids: &[String]) -> Vec<String> {
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
pub fn remove_from_archive(state: State<AppState>, ids: Vec<String>) -> Result<usize, String> {
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
pub fn delete_file(app: AppHandle, id: String) -> Result<(), String> {
    let path = {
        let state = app.state::<AppState>();
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
            runner::update(&app, &id, |t| t.filepath = None);
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
pub fn relocate_file(
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
    runner::update(&app, &id, |t| {
        t.filepath = Some(new_path.clone());
        t.output_dir = dir.to_string_lossy().into_owned();
    });

    Ok(new_path)
}

/// 用系统默认程序打开已下载的文件（行双击 / 详情里的「打开文件」）。
#[tauri::command(async)]
pub fn open_file(path: String) -> Result<(), String> {
    shell::open_file(&path)
}
/// 在资源管理器中选中该文件（而不是打开它）。
#[tauri::command(async)]
pub fn reveal_file(path: String) -> Result<(), String> {
    shell::reveal_file(&path)
}

/// 弹原生「选择文件夹」对话框。返回 `None` 表示用户取消。
///
/// 命令声明成 `async` + `spawn_blocking`：对话框是模态阻塞的，
/// 跑在主线程上会把窗口和消息循环一起卡死。
#[tauri::command]
pub async fn pick_folder(initial: Option<String>) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || shell::pick_folder(initial.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

/// 弹原生「选择文件」对话框。返回 `None` 表示用户取消。
#[tauri::command]
pub async fn pick_file(initial: Option<String>) -> Result<Option<String>, String> {
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
pub struct DiskSpaceReport {
    /// 输出目录所在卷的剩余可用字节；拿不到为 null。
    free_bytes: Option<u64>,
    /// 所有待下载任务的预估大小之和；任何一条拿不到就为 null（保守，不误判）。
    needed_bytes: Option<u64>,
    /// 是否需要提醒（剩余 < 预估 × 1.2，留 20% 余量）。
    warn: bool,
}

#[tauri::command]
pub fn disk_space_check(state: State<AppState>) -> DiskSpaceReport {
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
pub fn temp_dir_in_use(state: State<AppState>) -> Value {
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

#[cfg(test)]
mod tests {
    use super::*;

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
