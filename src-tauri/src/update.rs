//! yt-dlp 自更新（DESIGN §8）。
//!
//! 三条独立更新链路之一，也是**唯一不需要重新发版宿主**的那条：
//! 换掉一个 `yt-dlp.exe` 就能修好全部站点问题。
//!
//! ## 为什么不用 `yt-dlp -U`
//!
//! 写只读目录会失败、不支持 nightly 切换。走 GitHub Releases API 更可控。
//!
//! ## Windows 文件锁（实测）
//!
//! | 操作 | 运行中的 exe |
//! |---|---|
//! | 覆盖写入 / 删除 | ❌ 失败 |
//! | **重命名** | ✅ 成功 |
//!
//! 所以替换流程是「先重命名再写入」，**不必等所有任务空闲**；
//! 旧文件留到下次启动时清理（那时进程已退出）。
//!
//! ## 替换前必须验证
//!
//! 一个损坏的 exe 会让**所有**任务同时失败，比不更新糟得多。
//! 因此新文件必须先跑通 `--version` 才允许换上去。

use crate::net::{agent, explain_net_err};
use crate::paths;
use serde::Deserialize;
use std::path::{Path, PathBuf};

const RELEASES_API: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";

/// 「最新版本」的**稳定资产地址**。
///
/// `apply` 刻意不用 GitHub API：未认证的 API 每小时只有 60 次配额，
/// 而共享代理的出口 IP 很容易被其他用户耗尽——实测就撞上过 403，
/// 于是「检查得到有新版本、更新却失败」。
/// 这个 URL 由 GitHub 自己重定向到最新资产，不受 API 配额影响。
fn latest_asset_url() -> String {
    format!(
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/{}",
        asset_name_for_platform()
    )
}

/// 更新检查结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub newer: bool,
    /// 当前实际在用的 yt-dlp 路径。
    pub current_path: Option<String>,
    /// 可下载的资产名与大小，供界面展示。
    pub asset_name: Option<String>,
    pub asset_size: Option<u64>,
    pub notes_url: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    size: u64,
}

/// 本平台该下载哪个资产。
///
/// 官方发布的 `yt-dlp.exe` 是 **onefile**（单文件自包含），
/// 这正是 `externalBin` 能用的形态。包管理器给的 onedir 形态不行（DESIGN §2.5）。
fn asset_name_for_platform() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else {
        "yt-dlp_linux"
    }
}

/// 当前实际在用的 yt-dlp 版本。
fn local_version() -> Option<String> {
    version_of(&paths::resolve_ytdlp()?)
}

/// 检查是否有新版本。**不做任何写入**。
pub fn check(proxy: Option<&str>) -> Result<UpdateInfo, String> {
    let agent = agent(proxy)?;
    let resp = agent
        .get(RELEASES_API)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| match e {
            // 403 基本都是速率限制：未认证的 API 每小时只有 60 次配额，
            // 共享代理的出口 IP 很容易被别人耗尽。
            ureq::Error::Status(code, ref r) if code == 403 || code == 429 => format!(
                "GitHub API 拒绝了请求（HTTP {code}）。\
                 未认证的 API 每小时只有 60 次配额，用共享代理时容易被其他用户耗尽。\n\
                 请稍后再试——更新本身不依赖 API，可以在“更新”按钮里直接触发。"
            ),
            other => explain_net_err(&other.to_string(), proxy),
        })?;

    let release: Release = resp
        .into_json()
        .map_err(|e| format!("解析 GitHub 响应失败：{e}"))?;

    let latest = release.tag_name.trim_start_matches('v').to_string();
    let current = local_version().unwrap_or_default();
    // 本地版本读不到时保守处理：不认为「有新版本」，避免误替换
    let newer = !current.is_empty() && ytdlp_core::is_newer(&latest, &current);

    let want = asset_name_for_platform();
    let asset = release.assets.iter().find(|a| a.name == want);

    Ok(UpdateInfo {
        newer,
        current,
        latest,
        current_path: paths::resolve_ytdlp().map(|p| p.display().to_string()),
        asset_name: asset.map(|a| a.name.clone()),
        asset_size: asset.map(|a| a.size),
        notes_url: release.html_url,
    })
}

/// 下载新版本并替换。
///
/// 目标**永远写到 `<AppData>/<App>/bin/`**，不碰安装目录里的出厂副本：
/// 安装目录通常在 `Program Files`，非管理员写不进去，而且安装器的
/// 「修复」功能可能把它还原（DESIGN §8 约束 1）。
///
/// ## 为什么这里不走 GitHub API
///
/// API 有速率配额（实测撞过 403）。改成从稳定地址下载、**用下载到的文件
/// 自己报的版本号**判断是否需要替换——比先问 API 再下载更健壮，
/// 而且「能跑起来并报出版本」本身就是比大小校验更强的验证。
pub fn apply(proxy: Option<&str>) -> Result<String, String> {
    let agent = agent(proxy)?;

    let current = local_version().ok_or("当前 yt-dlp 无法运行，请先修复它再更新")?;

    // 目标始终是 AppData 下的升级副本
    let dir = paths::app_data_root().join("bin");
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建目录：{e}"))?;
    let target = dir.join(ytdlp_core::exe_name());
    let new_file = target.with_extension(if cfg!(windows) { "exe.new" } else { "new" });

    // ── 下载 ──
    let url = latest_asset_url();
    let resp = agent
        .get(&url)
        .call()
        .map_err(|e| explain_net_err(&e.to_string(), proxy))?;
    {
        let mut reader = resp.into_reader();
        let mut f = std::fs::File::create(&new_file)
            .map_err(|e| format!("无法写入 {}：{e}", new_file.display()))?;
        std::io::copy(&mut reader, &mut f).map_err(|e| format!("下载中断：{e}"))?;
        f.sync_all().ok();
    }

    // ── 验证 + 判版本：跑得起来且报出版本号，才算有效文件 ──
    let Some(new_version) = version_of(&new_file) else {
        let _ = std::fs::remove_file(&new_file);
        return Err("下载到的文件无法运行，已放弃更新（旧版本保持不变）".into());
    };
    if !ytdlp_core::is_newer(&new_version, &current) {
        let _ = std::fs::remove_file(&new_file);
        return Ok(format!("已是最新版本（{current}）"));
    }

    // ── 原子替换：先重命名旧文件，再放入新的 ──
    //
    // Windows 上运行中的 exe 不能覆盖也不能删除，**但可以重命名**，
    // 所以这里不必等任务空闲；旧文件留到下次启动清理（DESIGN §8 约束 2）。
    let old = target.with_extension(if cfg!(windows) { "exe.old" } else { "old" });
    let _ = std::fs::remove_file(&old);
    if target.exists() {
        std::fs::rename(&target, &old)
            .map_err(|e| format!("无法移开旧文件（可能正被占用，请稍后重试）：{e}"))?;
    }
    std::fs::rename(&new_file, &target).map_err(|e| {
        // 替换失败要把旧的放回去，不能留下一个「没有 yt-dlp」的状态
        let _ = std::fs::rename(&old, &target);
        format!("替换失败，已回滚：{e}")
    })?;

    // 缓存里可能还留着旧路径/旧验证结果
    paths::invalidate_ytdlp_cache();
    Ok(format!("已更新 {current} → {new_version}"))
}

/// 跑一次 `--version`，返回有效版本号。
fn version_of(exe: &Path) -> Option<String> {
    let out = std::process::Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = crate::text::decode_console(&out.stdout).trim().to_string();
    ytdlp_core::looks_like_version(&v).then_some(v)
}

/// 清理上次更新留下的 `.old`（进程退出后就能删了）。
///
/// 必须在**启动时**调用：更新发生时旧文件可能正被占用。
pub fn sweep_old_binaries() {
    let dir = paths::app_data_root().join("bin");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".old") || name.ends_with(".new") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// 供诊断：列出 AppData 下 bin 目录的内容。
pub fn installed_files() -> Vec<PathBuf> {
    let dir = paths::app_data_root().join("bin");
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_onefile_asset_per_platform() {
        let a = asset_name_for_platform();
        if cfg!(windows) {
            // 必须是官方 onefile 版：onedir 形态无法被 externalBin 单独复制
            assert_eq!(a, "yt-dlp.exe");
        }
        assert!(!a.is_empty());
    }

    /// 代理相关的 agent 构造与错误措辞已移到 `net` 模块，并在那里测试；
    /// 这里只保留更新流程自己的约束。

    /// 清理只该动 `.old` / `.new`，绝不能碰到正在用的那个文件。
    #[test]
    fn sweep_targets_only_temp_names() {
        for n in ["yt-dlp.exe.old", "yt-dlp.exe.new"] {
            assert!(n.ends_with(".old") || n.ends_with(".new"), "{n}");
        }
        let live = ytdlp_core::exe_name();
        assert!(!live.ends_with(".old") && !live.ends_with(".new"));
    }

    /// `apply` 用的稳定地址必须与平台资产名一致，且**不能**指向 API。
    #[test]
    fn apply_url_is_stable_and_api_free() {
        let u = latest_asset_url();
        assert!(u.contains("/releases/latest/download/"), "{u}");
        assert!(!u.contains("api.github.com"), "apply 不该依赖有配额的 API：{u}");
        assert!(u.ends_with(asset_name_for_platform()), "{u}");
    }
}
