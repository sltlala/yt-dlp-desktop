//! 运行时状态。
//!
//! 设置项刻意以 `serde_json::Value` 透传：**前端是设置结构的唯一真源**，
//! 后端只按需抽取它要用的字段（见 `runner::spec_from_settings`）。
//! 这样避免在 Rust 侧重复声明二十多个字段、两边必然漂移。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// 进度。`total` 为 `None` 表示总大小未知 —— 界面应切「不确定态」而非显示 0%。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub downloaded: Option<u64>,
    pub total: Option<u64>,
    pub speed: Option<f64>,
    pub eta: Option<u64>,
}

/// 任务。字段名与 `src/types.ts` 的 `Task` 一一对应。
///
/// 新增字段一律加 `#[serde(default)]`，这样旧版本的数据库行
/// 仍然能被读进来，不会因为缺字段而丢掉整个历史。
///
/// `PartialEq` 是给存储层测试用的：验证「全字段往返」时逐字段比对太啰嗦。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub url: String,
    pub title: String,
    pub extractor: String,
    pub thumbnail: Option<String>,
    pub duration_sec: Option<f64>,
    /// `pending` / `probing` / `selecting` / `queued` / `downloading`
    /// / `postprocessing` / `completed` / `skipped` / `failed` / `paused` / `canceled`
    pub state: String,
    pub post_process: Option<String>,
    pub skip_reason: Option<String>,
    pub progress: Progress,
    pub filepath: Option<String>,
    /// **预估**下载大小（字节），探测后就有。
    ///
    /// 来自 yt-dlp 的 `requested_downloads`——即按当前 `-f` 表达式**实际会选中**
    /// 的那几条格式，体积由 yt-dlp 加好。拿不到时为 `None`（例如扁平播放列表），
    /// 界面必须显示「未知」而不是 0。
    #[serde(default)]
    pub size_estimate: Option<u64>,
    /// **实际**成品大小（字节），下载结束后从磁盘读。
    ///
    /// 不复用进度里的 `total`：那是预估，而且合并/嵌入字幕后成品会变大。
    #[serde(default)]
    pub size_actual: Option<u64>,
    /// 界面展示用的 `-f` 表达式（等价于「当前生效的」那个）。
    pub format_expression: String,
    /// 用户在「可用格式」里**显式选定**的表达式。
    ///
    /// `None` = 跟随设置里的预设（新建任务的默认状态）。
    /// `Some` 才覆盖预设——区分这两者，是为了让「改设置里的预设」仍能影响
    /// 尚未下载的任务，而不是被建任务那一刻的快照永久钉死。
    #[serde(default)]
    pub format_override: Option<String>,
    pub container: String,
    pub output_dir: String,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub added_at: u64,
    pub finished_at: Option<u64>,

    // ── 探测结果 ──
    /// 探测到的可用格式，供「高级模式」直接展示（避免再探一次）。
    #[serde(default)]
    pub formats: Vec<ytdlp_core::FormatInfo>,
    /// 可用字幕语言，让用户知道这个视频有什么字幕可选。
    #[serde(default)]
    pub subtitle_langs: Vec<String>,
    /// 播放列表条目。非空表示这是一个待勾选的播放列表（DESIGN §9）。
    #[serde(default)]
    pub playlist_entries: Vec<ytdlp_core::PlaylistEntry>,
    /// 勾选结果对应的 `--playlist-items` 值。
    #[serde(default)]
    pub playlist_items: Option<String>,
    /// 排队位置说明，仅用于界面提示（如「等待探测」「排队中」）。
    #[serde(default)]
    pub queue_hint: Option<String>,
    /// 本任务的进度是否来自 aria2c 自身上报（DESIGN §11.2）。
    /// 为真时界面应提示「进度可能不精确」。
    #[serde(default)]
    pub used_aria2c: bool,
    /// 这个任务**已经因为 Cloudflare 拦截自动开过指纹模拟**。
    ///
    /// 两个作用：
    /// 1. **防止无限重试**——重试过一次就不再重试，失败就是真失败；
    /// 2. 让下载**继承探测的发现**——探测阶段撞上拦截时把这个置上，
    ///    待会儿下载就不用再撞一次才发现要开模拟。
    ///
    /// 存在任务上而不是全局设置里：用户没要求开模拟，是这一个站点的这一次
    /// 需要，不该顺手改掉他所有的下载。
    #[serde(default)]
    pub auto_impersonate: bool,
}

impl Task {
    pub fn new(id: String, url: String, output_dir: String, format_expression: String) -> Self {
        Self {
            id,
            url,
            title: String::new(),
            extractor: String::new(),
            thumbnail: None,
            duration_sec: None,
            state: "pending".into(),
            post_process: None,
            skip_reason: None,
            progress: Progress::default(),
            filepath: None,
            size_estimate: None,
            size_actual: None,
            format_expression,
            format_override: None,
            container: "mp4".into(),
            output_dir,
            error: None,
            warnings: Vec::new(),
            added_at: now_ms(),
            finished_at: None,
            formats: Vec::new(),
            subtitle_langs: Vec::new(),
            playlist_entries: Vec::new(),
            playlist_items: None,
            queue_hint: None,
            used_aria2c: false,
            auto_impersonate: false,
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 生成一个**进程内唯一**的短 id：毫秒时间戳 + 自增序号。
///
/// ⚠️ 只用 `now_ms()` 是不够的：同一毫秒里连续两次调用会拿到**同一个 id**
/// （CI 上 `cookies::tests::keeps_multiple_profiles` 就是因此偶发失败）。
/// 撞 id 的后果不是「重复」而是**后者覆盖前者**——
/// 任务撞 id 会顶掉前一条任务，cookie profile 撞 id 会覆盖掉前一份 cookie 文件，
/// 而用户粘贴多行链接 / 连续导入几个 cookies.txt 正是最容易撞上的用法。
///
/// 序号取低 16 位：够用且不会让 id 变得很长；跨进程由时间戳区分。
pub fn unique_id(prefix: &str) -> String {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{:x}{:04x}", now_ms(), n & 0xffff)
}

/// 正在运行的任务句柄。
pub struct RunningProc {
    pub pid: u32,
    /// 用于取消：置位后 runner 会连带杀掉子进程树。
    pub cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct AppState {
    pub tasks: Mutex<Vec<Task>>,
    pub settings: Mutex<serde_json::Value>,
    pub running: Mutex<HashMap<String, RunningProc>>,
    /// 待写入数据库的任务 id。
    ///
    /// **按任务记录而不是一个全局 dirty 布尔**：进度更新很频繁
    /// （实测每秒数条），全量重写会把几百条任务都编码一遍；
    /// 只写真正变过的那几条（DESIGN §5.2「仅在状态转换时落库」）。
    pending_dirty: Mutex<std::collections::HashSet<String>>,
    /// 待从数据库删除的任务 id。
    pending_removed: Mutex<std::collections::HashSet<String>>,
    /// SQLite 连接。`None` 表示尚未初始化（单元测试里就是这种情况）。
    db: Mutex<Option<rusqlite::Connection>>,
}

impl AppState {
    /// 从数据库恢复任务历史，并把崩溃时残留的进行中状态重置（DESIGN §5.3）。
    ///
    /// 重置过的状态要立刻写回，否则下次启动还会看到同样的「进行中」。
    pub fn init_db(&self, path: &std::path::Path) -> Result<(), String> {
        let conn = crate::store::open(path)?;
        let loaded = crate::store::load_all(&conn)?;
        let n = loaded.len();

        if let Ok(mut t) = self.tasks.lock() {
            *t = loaded;
        }
        self.reset_crashed_tasks();
        *self.db.lock().map_err(|e| e.to_string())? = Some(conn);

        if n > 0 {
            // 崩溃恢复可能改过状态：全部标记一次再落库
            for id in self.snapshot().into_iter().map(|t| t.id) {
                self.mark_dirty(&id);
            }
            self.flush()?;
        }
        Ok(())
    }

    /// 把「变过的」和「删掉的」任务写进数据库。
    ///
    /// 由后台任务每 3 秒调一次（DESIGN §5.2）。**只写变化的那几条**，
    /// 不是全量重写——进度事件每秒数条，全量写会把几百条任务反复编码。
    pub fn flush(&self) -> Result<(), String> {
        let (dirty, removed) = self.take_pending();
        if dirty.is_empty() && removed.is_empty() {
            return Ok(());
        }

        let mut guard = self.db.lock().map_err(|e| e.to_string())?;
        let Some(conn) = guard.as_mut() else {
            // 数据库尚未打开（例如单元测试）：静默跳过，不影响功能
            return Ok(());
        };

        let to_write: Vec<Task> = {
            let tasks = self.tasks.lock().map_err(|e| e.to_string())?;
            tasks
                .iter()
                .filter(|t| dirty.contains(&t.id))
                .cloned()
                .collect()
        };

        crate::store::upsert_many(conn, &to_write)?;
        crate::store::delete_many(conn, &removed)?;
        Ok(())
    }

    /// 启动时把残留的「进行中」状态重置为 `paused`（DESIGN §5.3）。
    ///
    /// `postprocessing` 崩在合并/转码中途最麻烦：下载其实已完成，
    /// 但临时文件状态未知。恢复策略是重跑整条命令，靠 `--continue`
    /// 跳过已下载部分。
    pub fn reset_crashed_tasks(&self) {
        if let Ok(mut tasks) = self.tasks.lock() {
            for t in tasks.iter_mut() {
                if matches!(
                    t.state.as_str(),
                    "probing" | "downloading" | "postprocessing"
                ) {
                    t.state = "paused".into();
                    t.post_process = None;
                    t.progress.speed = None;
                    t.progress.eta = None;
                }
            }
        }
    }

    pub fn snapshot(&self) -> Vec<Task> {
        self.tasks.lock().map(|t| t.clone()).unwrap_or_default()
    }

    /// 在锁内修改**单个**任务，并标记待写库。
    ///
    /// 用回调而不是把 `MutexGuard` 暴露给调用方：否则在 `app.state::<AppState>()`
    /// 这类**临时值**上写 `if let Ok(g) = st.tasks.lock()` 会触发 E0597 ——
    /// guard 的临时值生命周期长于 `State` 引用。
    ///
    /// 只暴露这三个会**自动标记**的写入口（`with_task` / `insert_task` /
    /// `remove_tasks`），是为了不可能漏标——漏一个就会出现「重启后这个任务
    /// 的状态退回去了」这种难查的问题。
    pub fn with_task<R>(&self, id: &str, f: impl FnOnce(&mut Task) -> R) -> Option<R> {
        let r = self
            .tasks
            .lock()
            .ok()
            .and_then(|mut g| g.iter_mut().find(|t| t.id == id).map(f));
        if r.is_some() {
            self.mark_dirty(id);
        }
        r
    }

    /// 插入一个任务并标记待写库。
    pub fn insert_task(&self, task: Task) {
        let id = task.id.clone();
        if let Ok(mut g) = self.tasks.lock() {
            g.insert(0, task);
        }
        self.mark_dirty(&id);
    }

    /// 删除若干任务，并标记待从库中移除。
    pub fn remove_tasks(&self, ids: &[String]) {
        if let Ok(mut g) = self.tasks.lock() {
            g.retain(|t| !ids.contains(&t.id));
        }
        if let Ok(mut removed) = self.pending_removed.lock() {
            for id in ids {
                removed.insert(id.clone());
            }
        }
        // 同时从「待写入」里剔除，避免「先删了又把它写回去」
        if let Ok(mut dirty) = self.pending_dirty.lock() {
            for id in ids {
                dirty.remove(id);
            }
        }
    }

    pub fn mark_dirty(&self, id: &str) {
        if let Ok(mut d) = self.pending_dirty.lock() {
            d.insert(id.to_string());
        }
    }

    /// 取出并清空待写/待删集合。写库前调用。
    pub fn take_pending(&self) -> (Vec<String>, Vec<String>) {
        let dirty = self
            .pending_dirty
            .lock()
            .map(|mut d| d.drain().collect::<Vec<_>>())
            .unwrap_or_default();
        let removed = self
            .pending_removed
            .lock()
            .map(|mut d| d.drain().collect::<Vec<_>>())
            .unwrap_or_default();
        (dirty, removed)
    }

    /// 数据库里的任务条数。与内存列表对照可以看出有没有漏写库。
    pub fn stored_count(&self) -> Result<u64, String> {
        let guard = self.db.lock().map_err(|e| e.to_string())?;
        match guard.as_ref() {
            Some(conn) => crate::store::count(conn),
            None => Ok(0),
        }
    }

    /// 是否有待写内容（供后台任务判断要不要动数据库）。
    pub fn has_pending(&self) -> bool {
        let d = self.pending_dirty.lock().map(|d| !d.is_empty()).unwrap_or(false);
        let r = self
            .pending_removed
            .lock()
            .map(|d| !d.is_empty())
            .unwrap_or(false);
        d || r
    }

    /// 登记正在运行的进程。同样用 `&self` 而非返回 guard。
    pub fn set_running(&self, id: String, proc: RunningProc) {
        if let Ok(mut g) = self.running.lock() {
            g.insert(id, proc);
        }
    }

    pub fn clear_running(&self, id: &str) {
        if let Ok(mut g) = self.running.lock() {
            g.remove(id);
        }
    }

    /// 取运行句柄（返回**拥有所有权**的副本，避免借用溢出到调用方）。
    pub fn running_handle(&self, id: &str) -> Option<(u32, Arc<AtomicBool>)> {
        let g = self.running.lock().ok()?;
        g.get(id).map(|p| (p.pid, p.cancel.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `unique_id` 必须在**同一毫秒内连续调用**也不重复。
    ///
    /// 只用时间戳的版本在这里必挂——而它对应的真实场景是「粘贴多行链接」：
    /// 撞 id 会让后一条任务顶掉前一条（不是重复，是覆盖）。
    #[test]
    fn unique_id_never_collides_in_a_burst() {
        let ids: std::collections::HashSet<String> =
            (0..2000).map(|_| unique_id("t-")).collect();
        assert_eq!(ids.len(), 2000, "同一批里出现了重复 id");

        // 前缀要保留，且 id 里不能有文件系统不友好的字符（它会被当作目录名）
        let one = unique_id("t-");
        assert!(one.starts_with("t-"));
        assert!(one.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn unique_id_shares_prefix_shape_with_cookies() {
        // cookies 用空前缀（id 直接当文件名）
        let c = unique_id("");
        assert!(!c.starts_with('-'));
        assert!(c.chars().all(|ch| ch.is_ascii_alphanumeric()));
    }
}
