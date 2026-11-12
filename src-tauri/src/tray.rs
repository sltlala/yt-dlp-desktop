//! 系统托盘：关窗后留在托盘里继续下载，悬停看进度，右键退出。

use crate::state::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Window, WindowEvent};
use tauri_plugin_notification::NotificationExt;

const TRAY_ID: &str = "main";

/// 本次运行里是否已经提示过「已最小化到托盘」。只提示一次，免得每次关窗都弹。
static HINTED: AtomicBool = AtomicBool::new(false);

pub fn init(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("yt-dlp 下载器")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "show" => show_main(app),
            "quit" => quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    spawn_tooltip(app.clone());
    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn quit_app(app: &AppHandle) {
    // 落库节流是 3 秒一次，退出前补写一次，免得最后几秒的状态丢掉
    let _ = app.state::<AppState>().flush();
    app.exit(0);
}

fn close_to_tray_enabled(app: &AppHandle) -> bool {
    app.state::<AppState>()
        .settings
        .lock()
        .ok()
        .and_then(|s| s.get("closeToTray").and_then(|v| v.as_bool()))
        .unwrap_or(true)
}

/// 主窗口的关闭按钮：开了「关闭时最小化到托盘」就只隐藏窗口，下载照常进行。
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    let app = window.app_handle();
    if !close_to_tray_enabled(app) {
        let _ = app.state::<AppState>().flush();
        return;
    }
    api.prevent_close();
    let _ = window.hide();
    if !HINTED.swap(true, Ordering::Relaxed) {
        let _ = app
            .notification()
            .builder()
            .title("yt-dlp 下载器仍在运行")
            .body("下载会在后台继续。点托盘图标打开窗口，右键托盘图标退出。")
            .show();
    }
}

/// 托盘悬停文字：正在下载几个、总速度多少。
fn tooltip_text(app: &AppHandle) -> String {
    let (n, speed) = app
        .state::<AppState>()
        .tasks
        .lock()
        .map(|tasks| {
            tasks.iter().filter(|t| t.state == "downloading").fold((0, 0.0), |(n, s), t| {
                (n + 1, s + t.progress.speed.unwrap_or(0.0))
            })
        })
        .unwrap_or((0, 0.0));
    if n == 0 {
        "yt-dlp 下载器 · 空闲".to_string()
    } else {
        format!("yt-dlp 下载器 · 正在下载 {n} 个 · {}/s", fmt_bytes(speed))
    }
}

fn fmt_bytes(n: f64) -> String {
    let units = ["B", "KB", "MB", "GB"];
    let mut v = n;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i > 0 && v < 10.0 {
        format!("{v:.1} {}", units[i])
    } else {
        format!("{v:.0} {}", units[i])
    }
}

fn spawn_tooltip(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last = String::new();
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let text = tooltip_text(&app);
            if text != last {
                if let Some(tray) = app.tray_by_id(TRAY_ID) {
                    let _ = tray.set_tooltip(Some(&text));
                }
                last = text;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_speed_like_the_frontend() {
        assert_eq!(fmt_bytes(512.0), "512 B");
        assert_eq!(fmt_bytes(1536.0), "1.5 KB");
        assert_eq!(fmt_bytes(200.0 * 1024.0 * 1024.0), "200 MB");
    }
}
