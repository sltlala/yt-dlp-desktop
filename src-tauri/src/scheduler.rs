//! 任务调度：探测池与下载池**分离**，并做每域名限流（DESIGN §4）。
//!
//! 为什么必须分成两个池：
//! - **探测**只拉元数据，请求很轻，冷启动 0.3–1s 才是主要成本 → 并发可以高
//! - **下载**重、占带宽、触发站点限流 → 并发必须低
//!
//! 探测并发高**不会**导致封禁，下载并发高**会**。共用一个上限必然二选一地错。
//!
//! 每域名限流比「全局降并发」更有效：同一站点同时只跑 1 个下载，
//! 不同站点可以并行——20 个 B 站链接排队、同时跑 1 个 YouTube。

use crate::state::AppState;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// 排队中的下载项。**主机名随队一起存**，避免调度时再去锁 AppState。
struct QueuedDownload {
    id: String,
    host: String,
}

#[derive(Default)]
pub struct Scheduler {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    probe_queue: VecDeque<String>,
    download_queue: VecDeque<QueuedDownload>,
    probing: HashSet<String>,
    /// task_id → host
    downloading: HashMap<String, String>,
}

impl Scheduler {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // 锁中毒说明别的线程 panic 了；这里恢复而非继续 panic，
        // 否则一个探测失败会让整个调度器永久瘫痪。
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 入队探测。已在池中或排队中则不重复入队。
    pub fn enqueue_probe(&self, id: String) {
        let mut g = self.lock();
        if g.probing.contains(&id) || g.probe_queue.contains(&id) {
            return;
        }
        g.probe_queue.push_back(id);
    }

    pub fn enqueue_download(&self, id: String, host: String) {
        let mut g = self.lock();
        if g.downloading.contains_key(&id) || g.download_queue.iter().any(|q| q.id == id) {
            return;
        }
        g.download_queue.push_back(QueuedDownload { id, host });
    }

    fn probe_done(&self, id: &str) {
        self.lock().probing.remove(id);
    }

    fn download_done(&self, id: &str) {
        self.lock().downloading.remove(id);
    }

    /// 任务被取消/移除时清理，避免槽位永久占用。
    pub fn forget(&self, id: &str) {
        let mut g = self.lock();
        g.probing.remove(id);
        g.downloading.remove(id);
        g.probe_queue.retain(|x| x != id);
        g.download_queue.retain(|x| x.id != id);
    }

    /// 供 UI 展示的队列快照。
    pub fn stats(&self) -> (usize, usize, usize, usize) {
        let g = self.lock();
        (
            g.probing.len(),
            g.probe_queue.len(),
            g.downloading.len(),
            g.download_queue.len(),
        )
    }
}

/// 读取并发上限。任务运行中改设置也能生效。
pub fn limits(app: &AppHandle) -> (usize, usize, usize) {
    let st = app.state::<AppState>();
    let guard = st.settings.lock().ok();
    let get = |key: &str, default: usize| -> usize {
        guard
            .as_ref()
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(default)
    };
    (
        get("probeConcurrency", 6).clamp(1, 32),
        get("downloadConcurrency", 2).clamp(1, 16),
        get("perHostConcurrency", 1).clamp(1, 8),
    )
}

/// 启动调度循环。
///
/// 用 250ms 轮询而不是条件变量：队列状态还依赖「设置里改并发数」这种
/// 外部变化，轮询天然覆盖，且每秒 4 次的成本可以忽略。
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            pump(&app);
        }
    });
}

/// 尝试填满两个池的空闲槽位。
pub fn pump(app: &AppHandle) {
    let (probe_limit, dl_limit, host_limit) = limits(app);

    // ── 探测池 ──
    let mut to_probe = Vec::new();
    {
        let sch = app.state::<Scheduler>();
        let mut g = sch.lock();
        while g.probing.len() < probe_limit {
            let Some(id) = g.probe_queue.pop_front() else { break };
            if g.probing.contains(&id) {
                continue;
            }
            g.probing.insert(id.clone());
            to_probe.push(id);
        }
    }
    for id in to_probe {
        let app2 = app.clone();
        let id2 = id.clone();
        tauri::async_runtime::spawn(async move {
            crate::runner::run_probe(app2.clone(), id2.clone()).await;
            app2.state::<Scheduler>().probe_done(&id2);
        });
    }

    // ── 下载池（含每域名限制）──
    let mut to_download = Vec::new();
    {
        let sch = app.state::<Scheduler>();
        let mut g = sch.lock();

        let mut host_count: HashMap<String, usize> = HashMap::new();
        for h in g.downloading.values() {
            *host_count.entry(h.clone()).or_default() += 1;
        }

        // 只扫一遍原队列长度，被域名限流挡下的条目放回队尾，不会死循环。
        let scan = g.download_queue.len();
        for _ in 0..scan {
            if g.downloading.len() + to_download.len() >= dl_limit {
                break;
            }
            let Some(item) = g.download_queue.pop_front() else { break };

            // 已在下载的任务（重复入队保护）
            if g.downloading.contains_key(&item.id) {
                continue;
            }
            let used = host_count.get(&item.host).copied().unwrap_or(0);
            if used >= host_limit {
                g.download_queue.push_back(item);
                continue;
            }
            *host_count.entry(item.host.clone()).or_default() += 1;
            g.downloading.insert(item.id.clone(), item.host.clone());
            to_download.push(item.id);
        }
    }
    for id in to_download {
        let app2 = app.clone();
        let id2 = id.clone();
        tauri::async_runtime::spawn(async move {
            crate::runner::run_download(app2.clone(), id2.clone()).await;
            app2.state::<Scheduler>().download_done(&id2);
        });
    }
}

#[cfg(test)]
mod tests {
    //! 队列逻辑本身依赖 Tauri 的 `AppHandle`，这里只测不依赖句柄的部分：
    //! 入队去重与清理。

    use super::*;

    fn sched() -> Scheduler {
        Scheduler::default()
    }

    #[test]
    fn probe_enqueue_is_idempotent() {
        let s = sched();
        s.enqueue_probe("a".into());
        s.enqueue_probe("a".into());
        assert_eq!(s.lock().probe_queue.len(), 1);
    }

    #[test]
    fn download_enqueue_is_idempotent() {
        let s = sched();
        s.enqueue_download("a".into(), "x.com".into());
        s.enqueue_download("a".into(), "x.com".into());
        assert_eq!(s.lock().download_queue.len(), 1);
    }

    #[test]
    fn forget_clears_every_where() {
        let s = sched();
        s.enqueue_probe("a".into());
        s.enqueue_download("a".into(), "x.com".into());
        s.lock().probing.insert("a".into());
        s.lock().downloading.insert("a".into(), "x.com".into());

        s.forget("a");

        let g = s.lock();
        assert!(g.probe_queue.is_empty());
        assert!(g.download_queue.is_empty());
        assert!(g.probing.is_empty());
        assert!(g.downloading.is_empty());
    }

    #[test]
    fn stats_counts_each_pool() {
        let s = sched();
        s.enqueue_probe("a".into());
        s.enqueue_probe("b".into());
        s.enqueue_download("c".into(), "x.com".into());
        assert_eq!(s.stats(), (0, 2, 0, 1));
    }
}
