//! 应用自身的更新（yt-dlp 的更新在 `update.rs`）。
//!
//! 走 Tauri 官方 updater：发布流程（`release.yml`）给安装包签名并生成 `latest.json`，
//! 这里拉 GitHub 上最新 Release 的 `latest.json`，验签后下载安装。

use crate::state::AppState;
use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

/// 检查到的更新留着给「安装」用，免得再请求一次。
#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<Update>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateInfo {
    pub current: String,
    /// `None` = 已是最新。
    pub latest: Option<String>,
    pub notes: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    downloaded: u64,
    total: Option<u64>,
}

/// 与探测、下载用同一套代理设置：这个网络环境下直连 GitHub 往往不通。
/// updater 底层的 HTTP 客户端只认 http(s) 代理，socks 的就只能直连了。
fn updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let settings = app.state::<AppState>().settings.lock().map(|s| s.clone()).unwrap_or_default();
    let mut b = app.updater_builder();
    if let Some(p) = crate::runner::proxy_for(&settings, "https://github.com/")
        .filter(|p| p.starts_with("http://") || p.starts_with("https://"))
        .and_then(|p| tauri::Url::parse(&p).ok())
    {
        b = b.proxy(p);
    }
    b.build().map_err(|e| e.to_string())
}

fn readable(e: impl std::fmt::Display) -> String {
    let raw = e.to_string();
    crate::logfile::warn(format!("应用更新：{raw}"));
    // 插件把「连不上」和「还没有这个文件」报成同一句话，分不出来，只能两个都说
    if raw.contains("Could not fetch a valid release JSON") {
        "拿不到更新信息：连不上 GitHub（可在「设置 → 网络」配代理），或者还没有发布过支持自动更新的版本"
            .into()
    } else if raw.contains("signature") {
        format!("更新包签名校验失败，已放弃安装：{raw}")
    } else {
        format!("检查更新失败：{raw}")
    }
}

#[tauri::command]
pub async fn check_app_update(app: AppHandle) -> Result<AppUpdateInfo, String> {
    let current = app.package_info().version.to_string();
    let found = updater(&app)?.check().await.map_err(readable)?;
    let info = AppUpdateInfo {
        current,
        latest: found.as_ref().map(|u| u.version.clone()),
        notes: found.as_ref().and_then(|u| u.body.clone()),
    };
    *app.state::<PendingUpdate>().0.lock().map_err(|e| e.to_string())? = found;
    Ok(info)
}

/// 下载并安装。Windows 上安装程序启动后应用会退出，所以**先落库再装**。
#[tauri::command]
pub async fn install_app_update(app: AppHandle) -> Result<(), String> {
    let update = app
        .state::<PendingUpdate>()
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("请先检查更新")?;

    let mut downloaded = 0u64;
    let progress_app = app.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress_app
                    .emit("appupdate://progress", DownloadProgress { downloaded, total });
            },
            || {},
        )
        .await
        .map_err(readable)?;

    crate::logfile::info(format!("应用更新已下载：v{}，开始安装", update.version));
    let _ = app.state::<AppState>().flush();
    update.install(bytes).map_err(|e| format!("安装更新失败：{e}"))
}
