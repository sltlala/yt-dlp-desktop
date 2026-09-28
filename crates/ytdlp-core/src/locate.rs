//! yt-dlp 可执行文件的查找顺序。
//!
//! 顺序：`<AppData>/<App>/bin/` → `<InstallDir>/bin/` → `PATH`
//!
//! **为什么 AppData 优先**：安装目录通常在 `Program Files`，实测非管理员进程
//! 写入会抛 `UnauthorizedAccessException`（DESIGN §8）。把升级副本放 AppData，
//! 升级 yt-dlp 就**不需要管理员权限**，也不会被安装器的「修复」功能还原。
//!
//! 注意 `InstallDir` 不是直接给的：我们要的是**当前 exe 所在目录**，
//! 这正是 Tauri 的 `externalBin` 落点。这里显式传入以便单测。

use std::path::{Path, PathBuf};

/// 随包分发的可执行文件。
///
/// 两者**走完全一样的查找顺序**（bundled → AppData → PATH），
/// 所以这里抽象成枚举而不是各写一份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    YtDlp,
    /// 外部下载器。GPLv2，随包分发——见 `binaries/README-third-party.md`。
    Aria2c,
}

impl Tool {
    pub fn exe_name(self) -> &'static str {
        match (self, cfg!(windows)) {
            (Tool::YtDlp, true) => "yt-dlp.exe",
            (Tool::YtDlp, false) => "yt-dlp",
            (Tool::Aria2c, true) => "aria2c.exe",
            (Tool::Aria2c, false) => "aria2c",
        }
    }

    /// 传给 `--version` 之外，界面里也用这个名字。
    pub fn display_name(self) -> &'static str {
        match self {
            Tool::YtDlp => "yt-dlp",
            Tool::Aria2c => "aria2c",
        }
    }
}

/// 平台对应的可执行文件名（yt-dlp）。
///
/// 保留这个便捷函数是因为绝大多数调用点只关心 yt-dlp。
pub fn exe_name() -> &'static str {
    Tool::YtDlp.exe_name()
}

/// 升级副本的相对位置（相对 AppData 根）。
pub fn appdata_bin_dir(app_data_root: &Path) -> PathBuf {
    app_data_root.join("bin")
}

/// 出厂副本的相对位置（相对安装目录）。
pub fn install_bin_dir(install_dir: &Path) -> PathBuf {
    install_dir.join("bin")
}

/// 查找顺序中的候选路径（按优先级）。
///
/// `install_dir` 应传**当前 exe 所在目录**——Tauri 的 `externalBin`
/// 会把它放在那里。为兼容开发期布局，`<install_dir>/bin/` 也会被尝试。
pub fn candidates_for(
    tool: Tool,
    app_data_root: Option<&Path>,
    install_dir: Option<&Path>,
    path_env: Option<&str>,
) -> Vec<PathBuf> {
    let name = tool.exe_name();
    let mut out = Vec::new();

    // 1) 升级副本：可写、无需提权。
    if let Some(root) = app_data_root {
        out.push(appdata_bin_dir(root).join(name));
    }
    // 2) 出厂副本：随安装包签名，只读。
    if let Some(dir) = install_dir {
        out.push(dir.join(name));
        out.push(install_bin_dir(dir).join(name));
    }
    // 3) PATH 兜底。
    if let Some(path) = path_env {
        for entry in std::env::split_paths(path) {
            if entry.as_os_str().is_empty() {
                continue;
            }
            out.push(entry.join(name));
        }
    }
    out
}

/// `candidates_for` 的 yt-dlp 版本（保持既有调用点不变）。
pub fn candidates(
    app_data_root: Option<&Path>,
    install_dir: Option<&Path>,
    path_env: Option<&str>,
) -> Vec<PathBuf> {
    candidates_for(Tool::YtDlp, app_data_root, install_dir, path_env)
}

/// 按优先级返回第一个存在的候选。
///
/// `exists` 以闭包注入，便于在无文件系统的环境里单测。
pub fn locate_exe_for(
    tool: Tool,
    app_data_root: Option<&Path>,
    install_dir: Option<&Path>,
    path_env: Option<&str>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    candidates_for(tool, app_data_root, install_dir, path_env)
        .into_iter()
        .find(|p| exists(p))
}

/// `locate_exe_for` 的 yt-dlp 版本。
pub fn locate_exe(
    app_data_root: Option<&Path>,
    install_dir: Option<&Path>,
    path_env: Option<&str>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    locate_exe_for(Tool::YtDlp, app_data_root, install_dir, path_env, exists)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn order_is_appdata_then_install_then_path() {
        let ad_root = p("C:/Users/u/AppData/Roaming/App");
        let inst = p("C:/Program Files/App");
        let c = candidates(Some(&ad_root), Some(&inst), Some("C:/tools;D:/bin"));

        // 用 PathBuf 相等比较，避免依赖路径分隔符方向（Windows 是 `\`）。
        let pos_ad = c
            .iter()
            .position(|x| *x == appdata_bin_dir(&ad_root).join(exe_name()))
            .expect("应含 AppData 候选");
        let pos_inst = c
            .iter()
            .position(|x| *x == inst.join(exe_name()))
            .expect("应含安装目录候选");

        // AppData 必须排第一，否则升级副本永远不被使用。
        assert_eq!(pos_ad, 0);
        assert!(pos_ad < pos_inst);
        assert!(c.contains(&p("C:/tools").join(exe_name())));
        assert!(c.contains(&p("D:/bin").join(exe_name())));
    }

    #[test]
    fn picks_first_existing() {
        let ad = p("C:/ad");
        let inst = p("C:/inst");
        let found = locate_exe(Some(&ad), Some(&inst), None, |c| {
            c.starts_with("C:/inst")
        });
        assert!(found.unwrap().starts_with("C:/inst"));
    }

    #[test]
    fn prefers_appdata_when_both_exist() {
        let found = locate_exe(
            Some(&p("C:/ad")),
            Some(&p("C:/inst")),
            None,
            |_| true, // 两者都存在
        );
        assert!(
            found.unwrap().starts_with("C:/ad"),
            "升级副本必须优先于出厂副本"
        );
    }

    #[test]
    fn falls_back_to_path() {
        let found = locate_exe(None, None, Some("C:/tools"), |_| true);
        assert_eq!(found.unwrap(), p("C:/tools").join(exe_name()));
    }

    #[test]
    fn returns_none_when_nothing_exists() {
        assert_eq!(locate_exe(Some(&p("C:/ad")), Some(&p("C:/i")), None, |_| false), None);
    }

    #[test]
    fn empty_path_entries_skipped() {
        let c = candidates(None, None, Some(";;C:/tools;"));
        let s: Vec<String> = c.iter().map(|x| x.display().to_string()).collect();
        assert_eq!(s.len(), 1, "空 PATH 项应被跳过: {s:?}");
    }

    #[test]
    fn exe_name_matches_platform() {
        if cfg!(windows) {
            assert_eq!(exe_name(), "yt-dlp.exe");
        } else {
            assert_eq!(exe_name(), "yt-dlp");
        }
    }

    // ───────── aria2c：走同一套查找顺序 ─────────

    #[test]
    fn aria2c_uses_the_same_lookup_order() {
        let ad = p("C:/ad");
        let inst = p("C:/inst");
        let c = candidates_for(Tool::Aria2c, Some(&ad), Some(&inst), Some("C:/tools"));
        let name = Tool::Aria2c.exe_name();

        assert_eq!(c[0], appdata_bin_dir(&ad).join(name), "AppData 必须排第一");
        assert!(c.contains(&inst.join(name)));
        assert!(c.contains(&inst.join("bin").join(name)));
        assert!(c.contains(&p("C:/tools").join(name)));
        // 不能把 yt-dlp 的文件名混进来
        assert!(!c.contains(&p("C:/tools").join(exe_name())));
    }

    #[test]
    fn aria2c_locate_picks_first_existing() {
        let found = locate_exe_for(
            Tool::Aria2c,
            Some(&p("C:/ad")),
            Some(&p("C:/inst")),
            None,
            |c| c.starts_with("C:/inst"),
        );
        assert!(found.unwrap().starts_with("C:/inst"));
    }

    #[test]
    fn tool_names_are_distinct() {
        assert_ne!(Tool::YtDlp.exe_name(), Tool::Aria2c.exe_name());
        assert_eq!(Tool::Aria2c.display_name(), "aria2c");
        if cfg!(windows) {
            assert_eq!(Tool::Aria2c.exe_name(), "aria2c.exe");
        } else {
            assert_eq!(Tool::Aria2c.exe_name(), "aria2c");
        }
    }
}
