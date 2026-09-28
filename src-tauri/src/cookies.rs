//! Cookie 多 profile 存储与预检（DESIGN §6）。
//!
//! 为什么是「多 profile」而不是单个输入框：用户必然有多套身份
//! （B站账号 A / YouTube 账号 B），任务记录里要存 `cookie_profile_id`。
//! 单一 cookie 输入框在第二个账号出现时就得重构数据模型。
//!
//! 存储布局（凭证是明文，**只放 AppData，不放项目目录**）：
//! ```text
//! <AppData>/ytdlp-desktop/cookies/
//!   index.json        元数据（名称、来源、条数、导入时间）
//!   <id>.txt          各 profile 的 Netscape 格式 cookie
//! ```

use crate::paths;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use ytdlp_core::{classify_browser_probe, validate_netscape, BrowserProbe};

/// profile 元数据。**不含 cookie 内容** —— 列表接口不该把凭证发给前端。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieProfile {
    pub id: String,
    pub name: String,
    /// `file`（导入的 cookies.txt）或 `browser`（从浏览器读取）。
    pub source: String,
    /// 来源说明：文件名，或浏览器名。
    pub origin: String,
    pub cookie_count: usize,
    pub created_at: u64,
}

pub fn profiles_dir() -> PathBuf {
    paths::cookies_dir()
}

pub fn profile_path(id: &str) -> PathBuf {
    profiles_dir().join(format!("{id}.txt"))
}

fn index_path() -> PathBuf {
    profiles_dir().join("index.json")
}

/// 列出所有 profile。索引损坏时返回空表而不是报错——用户不该因为
/// 一个坏掉的索引就完全用不了 cookie 功能。
pub fn list() -> Vec<CookieProfile> {
    std::fs::read_to_string(index_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<CookieProfile>>(&s).ok())
        .unwrap_or_default()
}

fn write_index(items: &[CookieProfile]) -> Result<(), String> {
    let dir = profiles_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建 cookie 目录：{e}"))?;
    let text = serde_json::to_string_pretty(items).map_err(|e| e.to_string())?;
    std::fs::write(index_path(), text).map_err(|e| format!("写入索引失败：{e}"))
}

/// 导入一份 cookies.txt。
///
/// **先校验再落盘**：格式错误在这里就报出来，附上行号与原因，
/// 而不是等任务失败时甩一句 yt-dlp 的原始报错。
pub fn save(name: &str, content: &str, origin: &str) -> Result<CookieProfile, String> {
    let count = validate_netscape(content).map_err(|e| e.message())?;

    let name = if name.trim().is_empty() {
        "未命名".to_string()
    } else {
        name.trim().to_string()
    };

    let id = format!("{:x}", crate::state::now_ms());
    let dir = profiles_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建 cookie 目录：{e}"))?;
    std::fs::write(profile_path(&id), content).map_err(|e| format!("写入 cookie 文件失败：{e}"))?;

    let p = CookieProfile {
        id,
        name,
        source: "file".into(),
        origin: origin.to_string(),
        cookie_count: count,
        created_at: crate::state::now_ms(),
    };

    let mut items = list();
    items.push(p.clone());
    write_index(&items)?;
    Ok(p)
}

pub fn remove(id: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(profile_path(id));
    let items: Vec<CookieProfile> = list().into_iter().filter(|p| p.id != id).collect();
    write_index(&items)
}

pub fn get(id: &str) -> Option<CookieProfile> {
    list().into_iter().find(|p| p.id == id)
}

/// 读某个 profile 的 cookie 文件路径。
pub fn path_of(id: &str) -> Option<PathBuf> {
    let p = profile_path(id);
    p.is_file().then_some(p)
}

// ─────────────────── 浏览器清单与 profile 枚举 ───────────────────
//
// 界面不该只给一个写死的下拉框：本机可能装了别的浏览器，而且**同一个浏览器
// 常有多个 profile**（工作与个人各一份，登录态不同）。
// 这里按 yt-dlp 自己的路径映射去探测，保证「界面上选得到的」就是
// 「yt-dlp 真能读到的」。

/// 界面上可选的一个浏览器。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserChoice {
    pub name: String,
    pub label: String,
    /// 本机是否装了（cookie 库是否存在）。
    pub installed: bool,
    pub supports_profiles: bool,
    /// 是否 Chromium 系——界面据此决定要不要提示「Windows 上多半读不到」。
    pub chromium: bool,
    /// 该浏览器的 profile，**最近用过的在前**。
    pub profiles: Vec<ProfileChoice>,
    /// 建议选哪个 profile（最近使用过的那个）。
    pub recommended_profile: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileChoice {
    pub id: String,
    pub label: String,
    /// cookie 库的最后修改时间（毫秒），用于排序与「多久没用」提示。
    pub last_used: Option<u64>,
}

fn mtime_ms(p: &std::path::Path) -> Option<u64> {
    std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// Chromium 系浏览器的用户数据目录。
/// 路径映射与 yt-dlp 的 `_get_chromium_based_browser_settings` 保持一致。
fn chromium_data_dir(name: &str) -> Option<PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok();
    let roaming = std::env::var("APPDATA").ok();

    if cfg!(windows) {
        let (base, sub) = match name {
            "brave" => (local.as_deref()?, r"BraveSoftware\Brave-Browser\User Data"),
            "chrome" => (local.as_deref()?, r"Google\Chrome\User Data"),
            "chromium" => (local.as_deref()?, r"Chromium\User Data"),
            "edge" => (local.as_deref()?, r"Microsoft\Edge\User Data"),
            "vivaldi" => (local.as_deref()?, r"Vivaldi\User Data"),
            "whale" => (local.as_deref()?, r"Naver\Naver Whale\User Data"),
            "opera" => (roaming.as_deref()?, r"Opera Software\Opera Stable"),
            _ => return None,
        };
        return Some(PathBuf::from(base).join(sub));
    }

    let home = std::env::var("HOME").ok()?;
    let mac = cfg!(target_os = "macos");
    let sub = match (name, mac) {
        ("brave", true) => "Library/Application Support/BraveSoftware/Brave-Browser",
        ("chrome", true) => "Library/Application Support/Google/Chrome",
        ("chromium", true) => "Library/Application Support/Chromium",
        ("edge", true) => "Library/Application Support/Microsoft Edge",
        ("opera", true) => "Library/Application Support/com.operasoftware.Opera",
        ("vivaldi", true) => "Library/Application Support/Vivaldi",
        ("whale", true) => "Library/Application Support/Naver/Whale",
        ("brave", false) => ".config/BraveSoftware/Brave-Browser",
        ("chrome", false) => ".config/google-chrome",
        ("chromium", false) => ".config/chromium",
        ("edge", false) => ".config/microsoft-edge",
        ("opera", false) => ".config/opera",
        ("vivaldi", false) => ".config/vivaldi",
        ("whale", false) => ".config/naver-whale",
        _ => return None,
    };
    Some(PathBuf::from(home).join(sub))
}

/// Firefox 的 `Profiles` 目录。
///
/// yt-dlp 把 `--cookies-from-browser firefox:<profile>` 里的 `<profile>`
/// **直接拼在这个目录后面**，所以它就是子目录名。
fn firefox_profiles_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        let roaming = std::env::var("APPDATA").ok()?;
        return Some(PathBuf::from(roaming).join(r"Mozilla\Firefox\Profiles"));
    }
    let home = std::env::var("HOME").ok()?;
    let p = if cfg!(target_os = "macos") {
        PathBuf::from(&home).join("Library/Application Support/Firefox/Profiles")
    } else {
        PathBuf::from(&home).join(".mozilla/firefox")
    };
    Some(p)
}

/// 列出某个 Chromium 系浏览器下的所有 profile。
fn chromium_profiles(dir: &std::path::Path) -> Vec<ProfileChoice> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        // Cookie 库的位置随版本变过：新版在 Network/Cookies
        let db = [path.join("Network").join("Cookies"), path.join("Cookies")]
            .into_iter()
            .find(|p| p.is_file());
        let Some(db) = db else { continue };

        let id = e.file_name().to_string_lossy().into_owned();
        let label = if id == "Default" {
            "默认 (Default)".to_string()
        } else {
            id.clone()
        };
        out.push(ProfileChoice {
            id,
            label,
            last_used: mtime_ms(&db),
        });
    }
    out
}

fn firefox_profiles(dir: &std::path::Path) -> Vec<ProfileChoice> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let db = e.path().join("cookies.sqlite");
        if !db.is_file() {
            continue;
        }
        let id = e.file_name().to_string_lossy().into_owned();
        out.push(ProfileChoice {
            id: id.clone(),
            label: id,
            last_used: mtime_ms(&db),
        });
    }
    out
}

/// 枚举本机可用的浏览器与各自的 profile。
///
/// 排序：**装了的在前**，每个浏览器内 profile 按最近使用倒序——
/// 用户要的几乎总是「我正在用的那个」。
pub fn list_browsers() -> Vec<BrowserChoice> {
    let mut out: Vec<BrowserChoice> = ytdlp_core::cookies::SUPPORTED_BROWSERS
        .iter()
        .map(|info| {
            let available = ytdlp_core::cookies::browser_available_here(info.name);

            let mut profiles: Vec<ProfileChoice> = if !available || info.name == "safari" {
                // Safari 的 cookie 库不是「多 profile」结构，交给 yt-dlp 自己找
                Vec::new()
            } else if info.name == "firefox" {
                firefox_profiles_dir()
                    .map(|d| firefox_profiles(&d))
                    .unwrap_or_default()
            } else {
                chromium_data_dir(info.name)
                    .map(|d| chromium_profiles(&d))
                    .unwrap_or_default()
            };
            profiles.sort_by(|a, b| b.last_used.cmp(&a.last_used));

            // Opera 不支持 profile（与 yt-dlp 的 browsers_without_profiles 一致）
            let supports_profiles = info.supports_profiles && info.name != "safari";
            if !supports_profiles {
                profiles.clear();
            }

            let installed = if info.name == "firefox" {
                firefox_profiles_dir().map(|d| d.is_dir()).unwrap_or(false)
            } else if info.name == "safari" {
                available
            } else {
                chromium_data_dir(info.name)
                    .map(|d| d.is_dir())
                    .unwrap_or(false)
            };

            BrowserChoice {
                name: info.name.to_string(),
                label: info.label.to_string(),
                installed,
                supports_profiles,
                chromium: info.chromium,
                recommended_profile: profiles.first().map(|p| p.id.clone()),
                profiles,
            }
        })
        .collect();

    // 装了的排前面；稳定排序保证其余保持 SUPPORTED_BROWSERS 的顺序（Firefox 优先）
    out.sort_by_key(|b| !b.installed);
    out
}

/// 预检：导入的 cookies.txt 是否可用。
///
/// 只做本地校验——文件是我们自己存下来的，格式在 `save` 时已校验过，
/// 这里重新读一遍是为了发现「被外部删掉/改坏」的情况。
pub fn check_profile(id: &str) -> BrowserProbe {
    let Some(path) = path_of(id) else {
        return BrowserProbe::NotFound;
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => match validate_netscape(&content) {
            Ok(n) => BrowserProbe::Ok {
                count: n,
                browser: get(id).map(|p| p.name).unwrap_or_else(|| id.to_string()),
            },
            Err(e) => BrowserProbe::Other(e.message()),
        },
        Err(e) => BrowserProbe::Other(format!("读取失败：{e}")),
    }
}

/// 预检：从浏览器读取 cookie 是否可用（DESIGN §6.2）。
///
/// 实测：Windows 上 Chrome 与 Edge **都失败**，但原因不同
/// （数据库被占用 vs DPAPI 解密失败），给用户的指引必须不同。
///
/// 用 `--cookies-from-browser` 加一个必定失败的 URL：
/// cookie 提取发生在任何网络请求**之前**，所以拿得到真实的提取结果。
pub async fn check_browser(browser: &str) -> BrowserProbe {
    let Some(exe) = paths::resolve_ytdlp() else {
        return BrowserProbe::Other("未找到可用的 yt-dlp".into());
    };

    let out = tokio::process::Command::new(exe)
        .args([
            "--cookies-from-browser",
            browser,
            "--simulate",
            "--no-warnings",
            "--no-colors",
            "--socket-timeout",
            "5",
            // 这个域名不会被解析，用来让 yt-dlp 快速走完 cookie 阶段后退出
            "https://cookie-probe.invalid/",
        ])
        .stdin(Stdio::null())
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .output()
        .await;

    match out {
        Ok(o) => classify_browser_probe(
            &crate::text::decode_console(&o.stdout),
            &crate::text::decode_console(&o.stderr),
        ),
        Err(e) => BrowserProbe::Other(format!("无法启动 yt-dlp：{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// `std::env::set_var` 是**进程全局**的，而 cargo 默认并行跑测试——
    /// 几个测试各设各的 `APPDATA` 会互相踩，表现为「系统找不到指定的路径」。
    /// 用一个锁把它们串起来。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 在隔离的 APPDATA 下跑一段代码。
    fn with_temp_appdata<T>(f: impl FnOnce() -> T) -> T {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "ytdlp-cookie-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("APPDATA", &tmp);

        let result = f();

        let _ = std::fs::remove_dir_all(&tmp);
        drop(guard);
        result
    }

    const CONTENT: &str = "# Netscape HTTP Cookie File\n\
                           .example.com\tTRUE\t/\tFALSE\t1790000000\ttoken\tv\n";

    /// 索引读写 + 删除。
    #[test]
    fn save_list_remove_roundtrip() {
        with_temp_appdata(|| {
            let p = save("测试账号", CONTENT, "cookies.txt").expect("导入应成功");
            assert_eq!(p.cookie_count, 1);
            assert_eq!(p.name, "测试账号");

            let all = list();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].id, p.id);

            assert!(path_of(&p.id).is_some());
            assert!(check_profile(&p.id).is_ok());

            remove(&p.id).unwrap();
            assert!(list().is_empty());
            assert!(path_of(&p.id).is_none());
        });
    }

    /// 格式错误的文件**不该落盘**——否则用户会得到一个永远失败的 profile。
    #[test]
    fn rejects_bad_format_without_writing() {
        with_temp_appdata(|| {
            let err = save("坏的", "not a cookie file at all", "x.txt").unwrap_err();
            assert!(err.contains("Netscape"), "实际: {err}");
            assert!(list().is_empty(), "校验失败时不应写入索引");
        });
    }

    #[test]
    fn empty_name_gets_default() {
        with_temp_appdata(|| {
            let p = save("   ", CONTENT, "f.txt").unwrap();
            assert_eq!(p.name, "未命名");
        });
    }

    /// 多个 profile 要能共存——这正是「多账号」的核心诉求。
    #[test]
    fn keeps_multiple_profiles() {
        with_temp_appdata(|| {
            let a = save("B站账号", CONTENT, "bili.txt").unwrap();
            let b = save("YouTube账号", CONTENT, "yt.txt").unwrap();
            assert_ne!(a.id, b.id, "两个 profile 不能拿到同一个 id");

            let all = list();
            assert_eq!(all.len(), 2);
            assert!(all.iter().any(|p| p.name == "B站账号"));
            assert!(all.iter().any(|p| p.name == "YouTube账号"));
            // 各自的文件都在
            assert!(path_of(&a.id).is_some() && path_of(&b.id).is_some());
        });
    }

    /// profile 文件被外部删掉后，预检要报出来而不是假装可用。
    #[test]
    fn missing_file_is_reported_by_check() {
        with_temp_appdata(|| {
            let p = save("会消失", CONTENT, "x.txt").unwrap();
            std::fs::remove_file(profile_path(&p.id)).unwrap();
            // 文件没了 → 不该是 Ok
            assert!(!check_profile(&p.id).is_ok());
        });
    }

    /// 删除不存在的 id 不应 panic。
    #[test]
    fn remove_unknown_id_is_ok() {
        with_temp_appdata(|| {
            assert!(remove("does-not-exist").is_ok());
        });
    }
}
