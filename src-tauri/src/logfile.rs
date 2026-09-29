//! 文件日志。
//!
//! ## 为什么需要它
//!
//! 打包后的 GUI 程序**没有控制台**，`eprintln!` 等于丢进黑洞。结果是出问题时
//! 用户手上一点线索都没有——「下载失败」这四个字之外什么都拿不到，而我们
//! 远程排查时最想看的东西（yt-dlp 到底被怎么调用的、它自己说了什么）全都没了。
//!
//! ## 为什么不引日志 crate
//!
//! 需要的只是「带时间戳地把一行追加进文件」。为这一个功能拉进
//! `log` + `tracing` + `env_logger` 一整棵依赖树不划算——与 `shell.rs`
//! 宁可用 `ShellExecuteW` 也不引 crate 是同一个取舍。
//!
//! ## 三条硬约束
//!
//! 1. **日志绝不能把应用搞挂**：所有 IO 失败一律静默吞掉（没有日志总比崩了强）。
//! 2. **不能无限长**：超过 [`MAX_BYTES`] 轮转成 `app.log.1`，只留一份旧的。
//! 3. **不能泄露密码**：调用方负责打码（见 `runner::redact_proxy`），
//!    这里不做兜底——但 `runner` 记命令行时一定会先打码。
//!
//! 日志落在**数据目录**下的 `logs\`，所以便携模式下它就在程序目录里，
//! 跟设置和历史待在一起。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 单份日志的上限。超了就轮转，避免长年累月涨成几百 MB。
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// 当前日志目录。`init` 之前所有写入都是空操作。
static DIR: OnceLock<PathBuf> = OnceLock::new();

/// 日志文件名。界面要显示它。
pub const FILE_NAME: &str = "app.log";

/// 指定日志目录（`<数据目录>\logs`）。
///
/// 启动时调用一次即可；重复调用只有第一次生效。
pub fn init(dir: PathBuf) {
    let _ = DIR.set(dir);
}

/// 当前日志文件路径；未初始化时为 `None`。
pub fn path() -> Option<PathBuf> {
    DIR.get().map(|d| d.join(FILE_NAME))
}

/// 追一行。失败就什么都不做——日志不能反过来影响主流程。
pub fn write(level: &str, msg: impl AsRef<str>) {
    let Some(p) = path() else { return };
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    rotate_if_needed(&p);
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) else {
        return;
    };
    let _ = writeln!(f, "{} [{}] {}", timestamp(), level, msg.as_ref());
}

pub fn info(msg: impl AsRef<str>) {
    write("INFO", msg);
}

pub fn warn(msg: impl AsRef<str>) {
    write("WARN", msg);
}

pub fn error(msg: impl AsRef<str>) {
    write("ERROR", msg);
}

/// 超限就 `app.log` → `app.log.1`（旧的直接覆盖，只留一份）。
fn rotate_if_needed(p: &Path) {
    let too_big = std::fs::metadata(p).map(|m| m.len() >= MAX_BYTES).unwrap_or(false);
    if !too_big {
        return;
    }
    let old = p.with_extension("log.1");
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::rename(p, &old);
}

/// `2026-09-30 01:23:45.678`。
///
/// Windows 上用 `GetLocalTime`：要的是**用户看得懂的本地时间**，
/// 而手算时区需要引 crate（与本模块「不引依赖」的前提冲突）。
/// 非 Windows 退回 Unix 秒——日志主要是给 Windows 用户看的。
#[cfg(windows)]
fn timestamp() -> String {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(t: *mut SystemTime);
    }
    let mut t = SystemTime::default();
    unsafe { GetLocalTime(&mut t) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.year, t.month, t.day, t.hour, t.minute, t.second, t.milliseconds
    )
}

#[cfg(not(windows))]
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没 `init` 之前写入必须是**安全空操作**，不能 panic——
    /// 有些命令（比如单测里直接调 runner）根本没走过启动流程。
    #[test]
    fn write_before_init_is_a_noop() {
        // 注意：不能断言 DIR 为空（别的测试可能已经 init 过），
        // 只要求它不 panic。
        write("INFO", "before init");
    }

    #[test]
    fn timestamp_has_the_expected_shape() {
        let t = timestamp();
        if cfg!(windows) {
            // 2026-09-30 01:23:45.678
            assert_eq!(t.len(), 23, "实际: {t}");
            assert_eq!(&t[4..5], "-");
            assert_eq!(&t[10..11], " ");
            assert_eq!(&t[13..14], ":");
            assert_eq!(&t[19..20], ".");
        }
    }
}
