//! yt-dlp 桌面工具的宿主无关核心层。
//!
//! 这一层刻意**不依赖 Tauri、不依赖任何第三方 crate**：
//! 「参数构造 + 输出解析」是全部逻辑里最容易出错、也最需要单测的部分
//! （HANDOFF §7）。把它独立出来，既能离线 `cargo test`，也让 UI 选型不成为瓶颈。
//!
//! 模块划分：
//! - [`args`]    —— 命令行参数构造（纯函数）
//! - [`parse`]   —— 输出解析（纯函数，含三条静默跳过路径）
//! - [`locate`]  —— exe 查找顺序（AppData → InstallDir → PATH）
//! - [`version`] —— 版本比较，供 yt-dlp 自更新使用

pub mod args;
pub mod cookies;
pub mod locate;
pub mod parse;
pub mod probe;
pub mod proxy;
pub mod url;
pub mod version;

pub use args::{
    build_download_args, build_probe_args, format_expression_for, playlist_items_spec,
    preset_expression, progress_template, resolve_container, AudioFormat, Container, CookieSource,
    DownloadSpec, EmbedOptions, JsRuntimeOptions, Preset, FILEPATH_FILE, JS_RUNTIMES,
};
pub use cookies::{classify_browser_probe, validate_netscape, BrowserProbe, CookieError};
pub use locate::{
    appdata_bin_dir, candidates, candidates_for, exe_name, install_bin_dir, locate_exe,
    locate_exe_for, Tool,
};
pub use parse::{
    parse_filepath_file, parse_line, Event, PostProcess, PostProcessKind, Progress, ProgressStatus,
    SkipReason,
};
pub use probe::{
    format_kind, has_codec, parse_info_json, FormatInfo, FormatKind, MediaInfo, PlaylistEntry,
};
pub use proxy::{matches_pattern, parse_bypass, parse_proxy_url, ProxyConfig, ProxyProtocol};
pub use url::host_of;
pub use version::{is_newer, looks_like_version};

/// 任务状态机（DESIGN §5.4）。
///
/// `downloading → postprocessing` 的切换时机：收到 `finished` 进度
/// **且** 出现 `[Merger]` / `[EmbedSubtitle]` 之类的后处理标记。
/// 这个中间态是**必需的** —— 用户看到 100% 却还在转圈时必须显示
/// 「正在合并」，否则会被当成卡死。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Probing,
    Downloading,
    PostProcessing(PostProcessKind),
    Completed,
    /// 归档命中或文件已存在 —— **yt-dlp 返回 exit=0**，必须与 `Completed` 区分。
    Skipped(SkipReason),
    Failed(String),
    Paused,
    Canceled,
}

impl TaskState {
    /// 进程被杀或崩溃后，启动时应把这些状态重置为 `Paused`（DESIGN §5.3）。
    pub fn is_resumable_after_crash(&self) -> bool {
        matches!(
            self,
            TaskState::Probing | TaskState::Downloading | TaskState::PostProcessing(_)
        )
    }

    /// 终态：不会被崩溃恢复改写。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Skipped(_) | TaskState::Failed(_) | TaskState::Canceled
        )
    }

    pub fn label(&self) -> String {
        match self {
            TaskState::Pending => "等待中".into(),
            TaskState::Probing => "正在解析".into(),
            TaskState::Downloading => "下载中".into(),
            TaskState::PostProcessing(k) => k.label().to_string(),
            TaskState::Completed => "已完成".into(),
            TaskState::Skipped(r) => r.label().to_string(),
            TaskState::Failed(e) => format!("失败：{e}"),
            TaskState::Paused => "已暂停".into(),
            TaskState::Canceled => "已取消".into(),
        }
    }

    /// 进度条是否应该显示为「不确定态」。
    pub fn is_indeterminate(&self) -> bool {
        matches!(self, TaskState::Probing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_recovery_resets_only_in_flight_states() {
        for s in [
            TaskState::Probing,
            TaskState::Downloading,
            TaskState::PostProcessing(PostProcessKind::Merger),
        ] {
            assert!(s.is_resumable_after_crash(), "{s:?} 应被重置为 Paused");
            assert!(!s.is_terminal());
        }
        for s in [
            TaskState::Completed,
            TaskState::Skipped(SkipReason::Archive),
            TaskState::Skipped(SkipReason::FileExists),
            TaskState::Failed("x".into()),
            TaskState::Canceled,
        ] {
            assert!(!s.is_resumable_after_crash(), "{s:?} 不应被改写");
            assert!(s.is_terminal(), "{s:?} 应为终态");
        }
    }

    /// 静默跳过必须是独立终态，不能归入 Completed（DESIGN §13.1）。
    #[test]
    fn skipped_is_not_completed() {
        assert_ne!(
            TaskState::Skipped(SkipReason::FileExists),
            TaskState::Completed
        );
        assert!(TaskState::Skipped(SkipReason::Archive).label().contains("跳过"));
    }

    #[test]
    fn postprocessing_has_visible_label() {
        let s = TaskState::PostProcessing(PostProcessKind::Merger);
        assert_eq!(s.label(), "正在合并音视频");
        let s = TaskState::PostProcessing(PostProcessKind::EmbedSubtitle);
        assert_eq!(s.label(), "正在嵌入字幕");
    }
}
