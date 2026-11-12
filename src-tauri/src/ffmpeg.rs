//! ffmpeg 一键下载。
//!
//! ## 为什么不随包分发
//!
//! 体积约 140 MB（安装包会因此大十倍），而且是 GPL——随包就得同时承担源码
//! 提供义务。DESIGN §8 的结论是「首次需要时引导下载」。
//!
//! ## 来源
//!
//! [yt-dlp/FFmpeg-Builds](https://github.com/yt-dlp/FFmpeg-Builds)：yt-dlp 官方维护，
//! 带有专门为 yt-dlp 打的补丁。`releases/download/latest/` 是固定地址，
//! 与 yt-dlp 自更新一样**不走 GitHub API**，不受未认证配额的限制。
//!
//! ## 落点
//!
//! `<AppData>/bin/`，与 yt-dlp 升级副本同目录：无需管理员权限，
//! 而且查找顺序里 AppData 本来就排第一（见 `locate.rs`）。

use crate::net::{agent, explain_net_err};
use crate::paths;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const ASSET_URL: &str =
    "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip";

/// 压缩包里要取出的文件。`ffplay` 用不到，不装。
const WANTED: &[&str] = &["ffmpeg.exe", "ffprobe.exe"];

/// 下载并安装到 `<AppData>/bin/`。`on_progress(已下载, 总大小)` 用于界面进度条。
pub fn install(
    proxy: Option<&str>,
    on_progress: impl Fn(u64, Option<u64>),
) -> Result<String, String> {
    if !cfg!(windows) {
        return Err("一键下载目前只支持 Windows，其他系统请用包管理器安装 ffmpeg".into());
    }

    let bin = paths::app_data_root().join("bin");
    let work = paths::app_data_root().join("ffmpeg-download");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("无法创建临时目录：{e}"))?;
    std::fs::create_dir_all(&bin).map_err(|e| format!("无法创建目录：{e}"))?;

    let result = (|| {
        let zip = work.join("ffmpeg.zip");
        download(proxy, &zip, &on_progress)?;
        extract(&zip, &work)?;
        for name in WANTED {
            let src = find_file(&work, name)
                .ok_or_else(|| format!("压缩包里没有 {name}，下载的文件可能不完整"))?;
            replace_into(&src, &bin.join(name))?;
        }
        Ok::<(), String>(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result?;

    paths::invalidate_ffmpeg_cache();
    match paths::ffmpeg_status() {
        (Some(path), version) => Ok(format!(
            "已安装 {}：{path}",
            version.unwrap_or_else(|| "ffmpeg".into())
        )),
        _ => Err("已解压但 ffmpeg 无法运行（可能被杀毒软件拦截）".into()),
    }
}

fn download(
    proxy: Option<&str>,
    dest: &Path,
    on_progress: &impl Fn(u64, Option<u64>),
) -> Result<(), String> {
    let resp = agent(proxy)?
        .get(ASSET_URL)
        .call()
        .map_err(|e| explain_net_err(&e.to_string(), proxy))?;
    let total = resp
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok());

    let mut reader = resp.into_reader();
    let mut f = std::fs::File::create(dest).map_err(|e| format!("无法写入 {}：{e}", dest.display()))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut done: u64 = 0;
    let mut last_report: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("下载中断：{e}"))?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).map_err(|e| format!("写入失败：{e}"))?;
        done += n as u64;
        // 每 2 MB 报一次，免得事件刷屏
        if done - last_report >= 2 * 1024 * 1024 {
            last_report = done;
            on_progress(done, total);
        }
    }
    f.sync_all().ok();
    on_progress(done, total);

    if let Some(t) = total {
        if done != t {
            return Err(format!("下载不完整（{done}/{t} 字节），请重试"));
        }
    }
    Ok(())
}

/// 用系统自带的 `tar`（bsdtar，Windows 10 1803 起内置）解 zip，
/// 免得为一次解压引入 zip crate。
fn extract(zip: &Path, into: &Path) -> Result<(), String> {
    let out = std::process::Command::new("tar")
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(into)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("无法调用系统 tar 解压：{e}"))?;
    if !out.status.success() {
        return Err(format!(
            "解压失败：{}",
            crate::text::decode_console(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// 压缩包里多套了一层 `ffmpeg-master-latest-win64-gpl/bin/`，名字随版本变，
/// 所以按文件名递归找，不写死路径。
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_file(&p, name) {
                return Some(found);
            }
        } else if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            return Some(p);
        }
    }
    None
}

/// 与 yt-dlp 自更新同样的套路：运行中的 exe 不能覆盖但能重命名（DESIGN §8），
/// 旧文件改名为 `.old`，下次启动由 `update::sweep_old_binaries` 清理。
fn replace_into(src: &Path, target: &Path) -> Result<(), String> {
    if target.exists() {
        let old = target.with_extension("exe.old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(target, &old)
            .map_err(|e| format!("无法替换 {}（可能正被占用）：{e}", target.display()))?;
    }
    std::fs::rename(src, target)
        .or_else(|_| std::fs::copy(src, target).map(|_| ()))
        .map_err(|e| format!("无法写入 {}：{e}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nested_file_case_insensitively() {
        let root = std::env::temp_dir().join(format!("ffmpeg-find-{}", std::process::id()));
        let nested = root.join("ffmpeg-master-latest-win64-gpl").join("bin");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("FFmpeg.exe"), b"x").unwrap();

        let found = find_file(&root, "ffmpeg.exe");
        let _ = std::fs::remove_dir_all(&root);
        assert!(found.is_some_and(|p| p.ends_with("FFmpeg.exe")));
    }

    #[test]
    fn missing_file_is_none() {
        let root = std::env::temp_dir().join(format!("ffmpeg-none-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let found = find_file(&root, "ffprobe.exe");
        let _ = std::fs::remove_dir_all(&root);
        assert!(found.is_none());
    }
}
