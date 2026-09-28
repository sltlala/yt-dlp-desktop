//! 应用目录布局。
//!
//! 关键决策：**升级副本放 AppData，出厂副本留在安装目录**（DESIGN §8）。
//! 安装目录通常在 `Program Files`，实测非管理员进程写入会抛
//! `UnauthorizedAccessException`，且安装器的「修复」功能可能把它还原。

use std::path::PathBuf;
use ytdlp_core::locate;

/// `%APPDATA%\<identifier>`（Windows）/ `~/.config/<identifier>`（Unix）
pub fn app_data_root() -> PathBuf {
    if let Ok(dir) = std::env::var("APPDATA") {
        return PathBuf::from(dir).join("ytdlp-desktop");
    }
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("ytdlp-desktop");
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config/ytdlp-desktop")
}

/// 当前 exe 所在目录 —— Tauri 的 `externalBin` 落点。
pub fn install_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
}

pub fn settings_file() -> PathBuf {
    app_data_root().join("config.json")
}

/// 任务历史数据库（SQLite）。
pub fn db_file() -> PathBuf {
    app_data_root().join("tasks.db")
}

/// 默认输出目录。
///
/// 之前这里给的是空字符串，结果是设置页那一栏空着、侧栏也不显示路径，
/// 看起来像「没有设置项」。给一个用户能认出来的真实目录。
pub fn default_output_dir() -> PathBuf {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let dl = PathBuf::from(&profile).join("Downloads");
        if dl.is_dir() {
            return dl;
        }
        return PathBuf::from(profile);
    }
    if let Ok(home) = std::env::var("HOME") {
        let dl = PathBuf::from(&home).join("Downloads");
        if dl.is_dir() {
            return dl;
        }
        return PathBuf::from(home);
    }
    app_data_root().join("downloads")
}

pub fn archive_file() -> PathBuf {
    app_data_root().join("archive.txt")
}

/// 多 profile 的 cookies.txt 存放处（DESIGN §6）。
///
/// 暂未接线到界面——cookie 子系统要做「多 profile + 预检」，
/// 不能只是设置页里的一个输入框。
#[allow(dead_code)]
pub fn cookies_dir() -> PathBuf {
    app_data_root().join("cookies")
}

/// temp 根目录的默认值：`<AppData>/<App>/tmp`（DESIGN §5.1）。
pub fn default_temp_root() -> PathBuf {
    app_data_root().join("tmp")
}

/// 本次生效的 temp 根目录，可由设置里的 `tempDir` 覆盖。
///
/// **为什么允许改**：`--paths temp:` 与输出目录在**同一个卷**上时，合并/嵌入后的
/// 成品只要改名就能落到输出目录；跨卷则要把整个文件复制一遍（4K 视频动辄几个 GB）。
/// 默认落在 AppData（系统盘），对「输出到别的盘」的用户正好是最坏情况。
///
/// ⚠️ 换目录会让**已经在进行的任务**在新目录里找不到旧 `.part`，
/// 续传静默失效、旧碎片永久残留。界面上那句提示就是这个意思。
pub fn temp_root(settings: &serde_json::Value) -> PathBuf {
    settings
        .get("tempDir")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_temp_root)
}

/// 在指定根目录下推导任务 temp 目录。
///
/// **必须可跨重启稳定重建**，否则断点续传失效（DESIGN §5.1）——所以这里
/// 只做 `root.join(task_id)`，不允许掺任何随机成分。
pub fn task_temp_dir_in(root: &std::path::Path, task_id: &str) -> PathBuf {
    root.join(task_id)
}

/// 已解析并**验证可用**的路径缓存（两个工具各一份）。
static RESOLVED_YTDLP: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
static RESOLVED_ARIA2C: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// 真正跑一次 `--version` 验证可执行文件能用。
///
/// **只检查文件是否存在是不够的**。实测：yt-dlp 有两种打包形态——
/// - **onefile**：官方发布的 `yt-dlp.exe`，单文件自包含（17.8 MB）
/// - **onedir**：`yt-dlp.exe` + 同级 `_internal\` 目录（本机这份就是）
///
/// 只把 onedir 的 `.exe` 复制到别处，会得到一个跑不起来的残缺副本，
/// 报错是 `Failed to load Python DLL ..._internal\python310.dll`。
/// 而 Tauri 的 `externalBin` **只复制那一个文件**——所以这种副本一旦排在
/// 查找顺序前面，就会**遮蔽掉 PATH 上完好的安装**。
fn probe_works(exe: &std::path::Path) -> bool {
    std::process::Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| {
            o.status.success()
                && ytdlp_core::looks_like_version(&crate::text::decode_console(&o.stdout))
        })
        .unwrap_or(false)
}

/// aria2c 的可用性验证。
///
/// 它和 yt-dlp 一样是「文件在 ≠ 能跑」：缺 DLL 的副本 `--version` 会失败。
/// 输出形如 `aria2 version 1.37.0`，用它把冒牌货挡掉。
fn probe_aria2c(exe: &std::path::Path) -> bool {
    std::process::Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| {
            o.status.success()
                && crate::text::decode_console(&o.stdout)
                    .to_ascii_lowercase()
                    .contains("aria2 version")
        })
        .unwrap_or(false)
}

/// 通用的「按优先级解析 + 验证」流程。
fn resolve_tool(
    tool: locate::Tool,
    cache: &std::sync::Mutex<Option<PathBuf>>,
    verify: fn(&std::path::Path) -> bool,
) -> Option<PathBuf> {
    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = guard.as_ref() {
            if p.is_file() {
                return Some(p.clone());
            }
        }
    }

    let cands = locate::candidates_for(
        tool,
        Some(&app_data_root()),
        install_dir().as_deref(),
        std::env::var("PATH").ok().as_deref(),
    );

    for c in cands {
        if !c.is_file() {
            continue;
        }
        if verify(&c) {
            let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(c.clone());
            return Some(c);
        }
        // 明确记下来，否则「明明有却报找不到」会非常难查
        eprintln!(
            "跳过不可用的 {}（可能是被单独复制的残缺副本）: {}",
            tool.display_name(),
            c.display()
        );
    }

    // PATH 上可能有 locate 没覆盖到的写法，最后再用 shell 找一次
    if let Ok(out) = std::process::Command::new(if cfg!(windows) { "where" } else { "which" })
        .arg(tool.exe_name())
        .output()
    {
        for line in crate::text::decode_console(&out.stdout).lines() {
            let p = PathBuf::from(line.trim());
            if p.is_file() && verify(&p) {
                let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
                *guard = Some(p.clone());
                return Some(p);
            }
        }
    }
    None
}

/// 按优先级解析 yt-dlp 可执行文件，**并验证它确实能运行**。
///
/// 结果会缓存；缓存的路径若消失则重新解析。
pub fn resolve_ytdlp() -> Option<PathBuf> {
    resolve_tool(locate::Tool::YtDlp, &RESOLVED_YTDLP, probe_works)
}

/// 按优先级解析 aria2c，**并验证它确实能运行**。
///
/// 顺序与 yt-dlp 一致：**随包的出厂副本优先于 PATH**。
/// 这样用户机器上那份旧的（或根本没有）不会影响我们，
/// 也解释了为什么「随包分发」是有意义的而不是多此一举。
pub fn resolve_aria2c() -> Option<PathBuf> {
    resolve_tool(locate::Tool::Aria2c, &RESOLVED_ARIA2C, probe_aria2c)
}

/// 清空已解析路径的缓存。
///
/// 自更新替换掉 exe 之后必须调用，否则会继续用缓存里的旧路径/旧验证结果。
pub fn invalidate_ytdlp_cache() {
    let mut guard = RESOLVED_YTDLP.lock().unwrap_or_else(|e| e.into_inner());
    *guard = None;
}

/// 探测结果：给设置页与诊断用。
pub fn aria2c_status() -> (Option<String>, Option<String>) {
    let Some(p) = resolve_aria2c() else {
        return (None, None);
    };
    let version = std::process::Command::new(&p)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .map(|o| crate::text::decode_console(&o.stdout))
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()));
    (Some(p.display().to_string()), version)
}

/// 在 PATH 里找可执行文件。
fn which(exe: &str, path: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        // Windows 上 node 可能是 node.exe，也可能是 node.cmd（某些包管理器）
        vec![
            format!("{exe}.exe"),
            format!("{exe}.cmd"),
            format!("{exe}.bat"),
            exe.to_string(),
        ]
    } else {
        vec![exe.to_string()]
    };
    for dir in std::env::split_paths(path) {
        for n in &names {
            let p = dir.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// 检测系统上可用的 JS 运行时。
///
/// **这不是可选优化**。实测：同一份 yt-dlp、同一个链接、同样的 cookie 与代理，
/// 只差 `--js-runtimes node` 就是「需要重载页面」与「成功」的区别。
/// yt-dlp **不会**自动启用已安装的运行时，而 YouTube 的 n-sig 挑战需要它。
pub fn detect_js_runtimes() -> Vec<String> {
    let path = std::env::var("PATH").unwrap_or_default();
    ytdlp_core::JS_RUNTIMES
        .iter()
        .filter(|name| which(name, &path).is_some())
        .map(|s| s.to_string())
        .collect()
}

/// 检测结果附带各自的可执行文件路径，供设置页展示。
pub fn detect_js_runtimes_detailed() -> Vec<(String, Option<String>)> {
    let path = std::env::var("PATH").unwrap_or_default();
    ytdlp_core::JS_RUNTIMES
        .iter()
        .map(|name| {
            (
                name.to_string(),
                which(name, &path).map(|p| p.display().to_string()),
            )
        })
        .collect()
}

/// 供诊断使用：列出所有候选及各自是否可用。
pub fn diagnose_ytdlp() -> Vec<(String, bool, bool)> {
    let cands = locate::candidates(
        Some(&app_data_root()),
        install_dir().as_deref(),
        std::env::var("PATH").ok().as_deref(),
    );
    cands
        .into_iter()
        .map(|c| {
            let exists = c.is_file();
            let usable = exists && probe_works(&c);
            (c.display().to_string(), exists, usable)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 纯函数，不碰环境变量：只验证「设置里的 tempDir 真的被用上」。
    #[test]
    fn temp_root_honours_setting() {
        let root = temp_root(&json!({ "tempDir": "E:\\下载\\视频\\tmp\\" }));
        assert_eq!(
            task_temp_dir_in(&root, "t-42"),
            PathBuf::from("E:\\下载\\视频\\tmp\\").join("t-42")
        );
    }

    /// 空串 / 缺失 / 非字符串都回落到默认根，不能让 temp 变成相对路径。
    #[test]
    fn temp_root_falls_back_when_blank() {
        let default = default_temp_root();
        assert_eq!(temp_root(&json!({})), default);
        assert_eq!(temp_root(&json!({ "tempDir": "" })), default);
        assert_eq!(temp_root(&json!({ "tempDir": "   " })), default);
        assert_eq!(temp_root(&json!({ "tempDir": null })), default);
        assert_eq!(temp_root(&json!({ "tempDir": 7 })), default);
    }

    /// 目录名只能是 task_id，掺随机成分会让续传静默失效（DESIGN §5.1）。
    #[test]
    fn task_dir_is_pure_function_of_id() {
        let root = PathBuf::from("/tmp/root");
        assert_eq!(task_temp_dir_in(&root, "a"), task_temp_dir_in(&root, "a"));
        assert!(task_temp_dir_in(&root, "a").ends_with("a"));
    }
}
