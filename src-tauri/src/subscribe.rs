//! 频道 / 播放列表订阅：定期检查，有新视频就自动加入下载。
//!
//! 单独存在 `subscriptions.json` 里而不是设置里：设置页保存时是整份覆盖，
//! 后台刚写进去的「上次检查时间 / 已见过的视频」会被一个旧副本冲掉。
//!
//! 订阅时只记下当前已有的视频，**不下载存量**——订阅一个几千集的频道，
//! 想要的是以后的更新，不是一口气排进几千个任务。

use crate::paths;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use ytdlp_core::PlaylistEntry;

/// 每次检查只看最新这么多条。两次检查之间更新超过这个数的频道很少见。
const FEED_LIMIT: usize = 30;
/// 「见过」的视频最多记这么多条，按时间先后淘汰最早的。
const SEEN_CAP: usize = 1000;
/// 后台多久看一次有没有到期的订阅。
const POLL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    pub id: String,
    pub url: String,
    pub title: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Unix 秒。
    #[serde(default)]
    pub last_checked: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// 最近一次检查新加入下载的条数。
    #[serde(default)]
    pub last_new: usize,
    #[serde(default)]
    pub seen: Vec<String>,
}

fn yes() -> bool {
    true
}

/// 发给前端的形状：不带几百条 `seen`。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionView {
    pub id: String,
    pub url: String,
    pub title: String,
    pub enabled: bool,
    pub last_checked: Option<u64>,
    pub last_error: Option<String>,
    pub last_new: usize,
}

impl From<&Subscription> for SubscriptionView {
    fn from(s: &Subscription) -> Self {
        Self {
            id: s.id.clone(),
            url: s.url.clone(),
            title: s.title.clone(),
            enabled: s.enabled,
            last_checked: s.last_checked,
            last_error: s.last_error.clone(),
            last_new: s.last_new,
        }
    }
}

#[derive(Default)]
pub struct Subscriptions {
    list: Mutex<Vec<Subscription>>,
    checking: AtomicBool,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn file() -> std::path::PathBuf {
    paths::app_data_root().join("subscriptions.json")
}

/// 用来判断「见没见过」的键。
///
/// 没有 id 的条目（RSS 之类）在解析时拿到的是 `#序号`，那是**位置**不是身份：
/// 顶上多出一条新视频，所有序号都会后移一位。这种只能用链接认。
fn entry_key(e: &PlaylistEntry) -> Option<String> {
    if e.id.is_empty() || e.id.starts_with('#') {
        e.url.clone()
    } else {
        Some(e.id.clone())
    }
}

/// 这一次检查里要加入下载的条目：`(键, 链接)`，没见过且有可下载的链接。
/// 按频道惯例最新的在前，返回时倒过来，让旧的先下。
fn fresh_entries(entries: &[PlaylistEntry], seen: &[String]) -> Vec<(String, String)> {
    let seen: HashSet<&str> = seen.iter().map(String::as_str).collect();
    let mut out: Vec<_> = entries
        .iter()
        .filter_map(|e| Some((entry_key(e)?, e.url.clone()?)))
        .filter(|(k, _)| !seen.contains(k.as_str()))
        .collect();
    out.reverse();
    out
}

fn remember(seen: &mut Vec<String>, ids: impl IntoIterator<Item = String>) {
    for id in ids {
        if !seen.contains(&id) {
            seen.push(id);
        }
    }
    if seen.len() > SEEN_CAP {
        let drop = seen.len() - SEEN_CAP;
        seen.drain(..drop);
    }
}

/// `interval_hours == 0` 表示关闭自动检查。
fn is_due(last_checked: Option<u64>, now: u64, interval_hours: u64) -> bool {
    if interval_hours == 0 {
        return false;
    }
    match last_checked {
        None => true,
        Some(t) => now.saturating_sub(t) >= interval_hours * 3600,
    }
}

impl Subscriptions {
    pub fn load() -> Self {
        let list = std::fs::read_to_string(file())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self { list: Mutex::new(list), checking: AtomicBool::new(false) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Subscription>> {
        self.list.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn views(&self) -> Vec<SubscriptionView> {
        self.lock().iter().map(SubscriptionView::from).collect()
    }

    fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&*self.lock()).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(paths::app_data_root()).map_err(|e| e.to_string())?;
        std::fs::write(file(), text).map_err(|e| format!("保存订阅失败：{e}"))
    }
}

fn settings_of(app: &AppHandle) -> serde_json::Value {
    app.state::<AppState>().settings.lock().map(|s| s.clone()).unwrap_or_default()
}

fn interval_hours(settings: &serde_json::Value) -> u64 {
    settings.get("subscriptionIntervalHours").and_then(|v| v.as_u64()).unwrap_or(6)
}

fn broadcast(app: &AppHandle) {
    let _ = app.emit("subscriptions://update", app.state::<Subscriptions>().views());
}

/// 检查一个订阅，把新视频加入下载。返回新加入的条数。
async fn check_one(app: &AppHandle, id: &str) -> Result<usize, String> {
    let Some(url) = app.state::<Subscriptions>().lock().iter().find(|s| s.id == id).map(|s| s.url.clone())
    else {
        return Ok(0);
    };
    let result = crate::runner::probe_feed(&url, &settings_of(app), FEED_LIMIT).await;

    let subs = app.state::<Subscriptions>();
    let (added, title) = {
        let mut list = subs.lock();
        let Some(sub) = list.iter_mut().find(|s| s.id == id) else {
            return Ok(0);
        };
        sub.last_checked = Some(now());
        match &result {
            Err(e) => {
                sub.last_error = Some(e.clone());
                (Vec::new(), sub.title.clone())
            }
            Ok(info) => {
                sub.last_error = None;
                let fresh = fresh_entries(&info.entries, &sub.seen);
                remember(&mut sub.seen, fresh.iter().map(|(id, _)| id.clone()));
                sub.last_new = fresh.len();
                (fresh, sub.title.clone())
            }
        }
    };
    let _ = subs.save();

    for (_, url) in &added {
        // RSS 条目的链接带着 yt-dlp 内部的 `#__youtubedl_smuggle=…` 尾巴，
        // 原样入队的话任务列表里就显示这一长串
        let url = url.split("#__youtubedl_smuggle=").next().unwrap_or(url);
        if let Err(e) = crate::add_url_inner(app, url) {
            crate::logfile::warn(format!("订阅「{title}」的新视频入队失败：{url}：{e}"));
        }
    }
    if !added.is_empty() {
        crate::logfile::info(format!("订阅「{title}」有 {} 个新视频，已加入下载", added.len()));
        let _ = app
            .notification()
            .builder()
            .title("订阅有更新")
            .body(format!("「{title}」有 {} 个新视频，已加入下载", added.len()))
            .show();
    }
    broadcast(app);
    result.map(|_| added.len())
}

/// 依次检查；同一时刻只允许一轮，免得手动「立即检查」和后台定时撞在一起重复入队。
async fn check_many(app: &AppHandle, ids: Vec<String>) -> Result<usize, String> {
    let subs = app.state::<Subscriptions>();
    if subs.checking.swap(true, Ordering::AcqRel) {
        return Err("正在检查中，请稍候".into());
    }
    let mut total = 0;
    for id in ids {
        total += check_one(app, &id).await.unwrap_or(0);
    }
    subs.checking.store(false, Ordering::Release);
    Ok(total)
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // 启动时先让开：首屏、探测、恢复下载都在这个时候抢资源
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            let hours = interval_hours(&settings_of(&app));
            let t = now();
            let due: Vec<String> = app
                .state::<Subscriptions>()
                .lock()
                .iter()
                .filter(|s| s.enabled && is_due(s.last_checked, t, hours))
                .map(|s| s.id.clone())
                .collect();
            if !due.is_empty() {
                let _ = check_many(&app, due).await;
            }
            tokio::time::sleep(POLL).await;
        }
    });
}

#[tauri::command]
pub fn list_subscriptions(app: AppHandle) -> Vec<SubscriptionView> {
    app.state::<Subscriptions>().views()
}

#[tauri::command]
pub async fn add_subscription(app: AppHandle, url: String) -> Result<SubscriptionView, String> {
    let url = url.trim().to_string();
    if !ytdlp_core::is_valid_url(&url) {
        return Err("不是有效链接".into());
    }
    if app.state::<Subscriptions>().lock().iter().any(|s| s.url == url) {
        return Err("已经订阅过这个链接".into());
    }
    let info = crate::runner::probe_feed(&url, &settings_of(&app), FEED_LIMIT).await?;
    if !info.is_playlist {
        return Err("这是单个视频，请填频道或播放列表的链接".into());
    }
    let mut seen = Vec::new();
    remember(&mut seen, info.entries.iter().filter_map(entry_key));
    let sub = Subscription {
        id: crate::state::unique_id("s-"),
        url,
        title: info.title,
        enabled: true,
        last_checked: Some(now()),
        last_error: None,
        last_new: 0,
        seen,
    };
    let view = SubscriptionView::from(&sub);
    let subs = app.state::<Subscriptions>();
    subs.lock().push(sub);
    subs.save()?;
    broadcast(&app);
    Ok(view)
}

#[tauri::command]
pub fn remove_subscription(app: AppHandle, id: String) -> Result<(), String> {
    let subs = app.state::<Subscriptions>();
    subs.lock().retain(|s| s.id != id);
    subs.save()?;
    broadcast(&app);
    Ok(())
}

#[tauri::command]
pub fn set_subscription_enabled(app: AppHandle, id: String, enabled: bool) -> Result<(), String> {
    let subs = app.state::<Subscriptions>();
    if let Some(s) = subs.lock().iter_mut().find(|s| s.id == id) {
        s.enabled = enabled;
    }
    subs.save()?;
    broadcast(&app);
    Ok(())
}

/// 立即检查。`id` 为空 = 检查全部已启用的订阅。返回新加入下载的条数。
#[tauri::command]
pub async fn check_subscriptions(app: AppHandle, id: Option<String>) -> Result<usize, String> {
    let ids: Vec<String> = app
        .state::<Subscriptions>()
        .lock()
        .iter()
        .filter(|s| match &id {
            Some(one) => &s.id == one,
            None => s.enabled,
        })
        .map(|s| s.id.clone())
        .collect();
    check_many(&app, ids).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, url: Option<&str>) -> PlaylistEntry {
        PlaylistEntry {
            id: id.into(),
            title: id.into(),
            url: url.map(str::to_string),
            duration: None,
            thumbnail: None,
        }
    }

    #[test]
    fn fresh_skips_seen_and_unlinked_and_puts_oldest_first() {
        let entries = vec![
            entry("c", Some("u/c")),
            entry("b", None),
            entry("a", Some("u/a")),
            entry("old", Some("u/old")),
        ];
        let seen = vec!["old".to_string()];
        let keys: Vec<_> = fresh_entries(&entries, &seen).into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["a", "c"]);
    }

    #[test]
    fn positional_ids_fall_back_to_url() {
        // 订阅时只有一条，之后顶上多出一条新的：序号整体后移，但链接没变
        let before = [entry("#1", Some("u/old"))];
        let mut seen = Vec::new();
        remember(&mut seen, before.iter().filter_map(entry_key));
        let after = vec![entry("#1", Some("u/new")), entry("#2", Some("u/old"))];
        let urls: Vec<_> = fresh_entries(&after, &seen).into_iter().map(|(_, u)| u).collect();
        assert_eq!(urls, ["u/new"]);
    }

    #[test]
    fn remember_dedups_and_caps() {
        let mut seen = vec!["x".to_string()];
        remember(&mut seen, ["x".to_string(), "y".to_string()]);
        assert_eq!(seen, ["x", "y"]);
        remember(&mut seen, (0..SEEN_CAP).map(|i| i.to_string()));
        assert_eq!(seen.len(), SEEN_CAP);
        assert_eq!(seen.last().unwrap(), &(SEEN_CAP - 1).to_string());
        assert!(!seen.contains(&"x".to_string()));
    }

    #[test]
    fn due_respects_interval_and_off() {
        assert!(is_due(None, 100, 6));
        assert!(!is_due(Some(0), 3600 * 6 - 1, 6));
        assert!(is_due(Some(0), 3600 * 6, 6));
        assert!(!is_due(None, 100, 0));
    }

    #[test]
    fn old_files_without_new_fields_still_load() {
        let s: Subscription =
            serde_json::from_str(r#"{"id":"s-1","url":"https://x/y","title":"T"}"#).unwrap();
        assert!(s.enabled);
        assert!(s.seen.is_empty());
    }
}
