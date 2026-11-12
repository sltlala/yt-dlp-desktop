//! 全部下载完成后睡眠 / 关机。
//!
//! 刻意**不存进设置**：这是「今晚这一批下完就关机」的一次性决定，
//! 存下来的话第二天开机下一个视频，下完电脑就自己关了。触发一次后自动复位。
//!
//! 触发前先倒计时 60 秒（界面横幅 + 系统通知），期间可以取消。

use crate::state::AppState;
use serde::Serialize;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

/// 倒计时长度。
const COUNTDOWN_SECS: u32 = 60;

/// 还在干活的状态。`paused` / `selecting` 不算：它们在等用户，不会自己结束，
/// 算进去的话「全部完成」永远不会到来。
const WORKING_STATES: &[&str] = &["pending", "probing", "queued", "downloading", "postprocessing"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    None,
    Sleep,
    Shutdown,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "sleep" => Some(Self::Sleep),
            "shutdown" => Some(Self::Shutdown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub mode: Mode,
    /// 倒计时剩余秒数；`None` = 没在倒计时。
    pub countdown: Option<u32>,
}

struct Inner {
    mode: Mode,
    /// 设定之后是否见过有任务在跑。没见过就不触发——
    /// 否则在空列表上选「完成后关机」会立刻开始倒计时。
    saw_working: bool,
    countdown: Option<u32>,
}

pub struct AfterAll(Mutex<Inner>);

impl Default for AfterAll {
    fn default() -> Self {
        Self(Mutex::new(Inner { mode: Mode::None, saw_working: false, countdown: None }))
    }
}

/// 每秒一次的状态推进。返回值是这一秒要不要真正执行。
fn tick(inner: &mut Inner, working: usize) -> bool {
    if inner.mode == Mode::None {
        return false;
    }
    if working > 0 {
        // 倒计时期间又有新任务（比如重试）：取消倒计时，等它们跑完
        inner.saw_working = true;
        inner.countdown = None;
        return false;
    }
    if !inner.saw_working {
        return false;
    }
    match inner.countdown {
        None => {
            inner.countdown = Some(COUNTDOWN_SECS);
            false
        }
        Some(0) | Some(1) => {
            inner.countdown = None;
            inner.saw_working = false;
            true
        }
        Some(n) => {
            inner.countdown = Some(n - 1);
            false
        }
    }
}

impl AfterAll {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> Status {
        let g = self.lock();
        Status { mode: g.mode, countdown: g.countdown }
    }

    pub fn set(&self, mode: Mode, working_now: usize) {
        let mut g = self.lock();
        g.mode = mode;
        g.countdown = None;
        g.saw_working = working_now > 0;
    }
}

fn working_count(app: &AppHandle) -> usize {
    app.state::<AppState>()
        .tasks
        .lock()
        .map(|t| t.iter().filter(|x| WORKING_STATES.contains(&x.state.as_str())).count())
        .unwrap_or(0)
}

#[tauri::command]
pub fn get_after_all(app: AppHandle) -> Status {
    app.state::<AfterAll>().status()
}

#[tauri::command]
pub fn set_after_all(app: AppHandle, mode: String) -> Result<Status, String> {
    let mode = Mode::parse(&mode).ok_or("未知的完成后动作")?;
    let working = working_count(&app);
    let st = app.state::<AfterAll>();
    st.set(mode, working);
    let status = st.status();
    let _ = app.emit("afterall://status", &status);
    Ok(status)
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let working = working_count(&app);
            let st = app.state::<AfterAll>();
            let (before, fire, after) = {
                let mut g = st.lock();
                let before = g.countdown;
                let fire = tick(&mut g, working);
                (before, fire, g.countdown)
            };
            if before != after || fire {
                let _ = app.emit("afterall://status", st.status());
            }
            let mode = st.status().mode;
            if before.is_none() && after.is_some() {
                let what = if mode == Mode::Shutdown { "关机" } else { "睡眠" };
                let _ = app
                    .notification()
                    .builder()
                    .title("全部下载完成")
                    .body(format!("{COUNTDOWN_SECS} 秒后{what}，可在主窗口取消"))
                    .show();
            }
            if fire {
                execute(&app, mode);
                st.set(Mode::None, 0);
                let _ = app.emit("afterall://status", st.status());
            }
        }
    });
}

fn execute(app: &AppHandle, mode: Mode) {
    // 关机前把还没落库的任务状态写下去
    let _ = app.state::<AppState>().flush();
    let (exe, args): (&str, &[&str]) = match mode {
        Mode::None => return,
        Mode::Shutdown => ("shutdown", &["/s", "/t", "0"]),
        // SetSuspendState 的第一个参数 0 = 睡眠；但若系统开了休眠，rundll32 这条路会变成休眠。
        // 走 .NET 的 Application.SetSuspendState('Suspend') 才是确定的睡眠。
        Mode::Sleep => (
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; \
                 [System.Windows.Forms.Application]::SetSuspendState('Suspend', $false, $false)",
            ],
        ),
    };
    crate::logfile::info(format!("全部下载完成，执行：{exe} {}", args.join(" ")));
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if let Err(e) = cmd.spawn() {
        crate::logfile::error(format!("执行{exe}失败：{e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inner(mode: Mode) -> Inner {
        Inner { mode, saw_working: false, countdown: None }
    }

    #[test]
    fn does_nothing_when_off() {
        let mut g = inner(Mode::None);
        assert!(!tick(&mut g, 0));
        assert!(!tick(&mut g, 3));
        assert_eq!(g.countdown, None);
    }

    #[test]
    fn empty_list_never_fires() {
        let mut g = inner(Mode::Shutdown);
        for _ in 0..200 {
            assert!(!tick(&mut g, 0));
        }
        assert_eq!(g.countdown, None);
    }

    #[test]
    fn counts_down_after_work_finishes_then_fires_once() {
        let mut g = inner(Mode::Sleep);
        assert!(!tick(&mut g, 2));
        assert!(!tick(&mut g, 0));
        assert_eq!(g.countdown, Some(COUNTDOWN_SECS));
        let mut fired = 0;
        for _ in 0..COUNTDOWN_SECS + 5 {
            if tick(&mut g, 0) {
                fired += 1;
            }
        }
        assert_eq!(fired, 1);
    }

    #[test]
    fn new_work_cancels_countdown() {
        let mut g = inner(Mode::Shutdown);
        tick(&mut g, 1);
        tick(&mut g, 0);
        tick(&mut g, 0);
        assert!(g.countdown.is_some());
        assert!(!tick(&mut g, 1));
        assert_eq!(g.countdown, None);
    }

    #[test]
    fn parse_rejects_unknown_modes() {
        assert_eq!(Mode::parse("sleep"), Some(Mode::Sleep));
        assert_eq!(Mode::parse("reboot"), None);
    }
}
