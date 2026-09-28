//! yt-dlp 输出解析。纯函数、零依赖、可单测。
//!
//! 解析三条独立的信息流：
//! 1. **进度行** `download|status|downloaded|total|estimate|speed|eta`
//! 2. **后处理标记** `[Key] message`
//! 3. **静默跳过**：归档命中与文件已存在 —— 两者**都返回 exit=0**
//!
//! 第 3 条是设计里最容易埋雷的地方：若不单独识别，
//! UI 会把「什么都没下」显示成「下载完成」（DESIGN §13.1）。

use crate::args::{PROGRESS_FIELDS, PROGRESS_PREFIX};

/// 解析 `NA` / 空串 / 数字。
fn num<T: std::str::FromStr>(s: &str) -> Option<T> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("NA") || s == "None" {
        return None;
    }
    s.parse::<T>().ok()
}

// ─────────────────────────── 进度 ───────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressStatus {
    Downloading,
    Finished,
    Error,
}

impl ProgressStatus {
    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "downloading" => Some(Self::Downloading),
            "finished" => Some(Self::Finished),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// 一条进度事件。
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub status: ProgressStatus,
    pub downloaded: Option<u64>,
    /// 已按 `total_bytes → total_bytes_estimate` 回落。
    pub total: Option<u64>,
    pub speed: Option<f64>,
    pub eta: Option<u64>,
}

impl Progress {
    /// 完成比例。**总大小未知时返回 `None`**，UI 应切「不确定态」而不是显示 0%。
    pub fn fraction(&self) -> Option<f64> {
        let total = self.total?;
        if total == 0 {
            return None;
        }
        let done = self.downloaded? as f64;
        Some((done / total as f64).clamp(0.0, 1.0))
    }

    /// 总大小未知 → 进度条应为不确定态。
    pub fn is_indeterminate(&self) -> bool {
        self.total.is_none()
    }

    /// `finished` **不等于**下载完成，后面还有合并/嵌入（HANDOFF §3.2）。
    pub fn is_finished(&self) -> bool {
        self.status == ProgressStatus::Finished
    }
}

// ─────────────────────────── 后处理 ───────────────────────────

/// 已知的后处理阶段。用于把状态机从 `downloading` 推进到 `postprocessing`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostProcessKind {
    Merger,
    ExtractAudio,
    EmbedSubtitle,
    EmbedThumbnail,
    Metadata,
    VideoRemuxer,
    SubtitlesConvertor,
    ThumbnailsConvertor,
    VideoConvertor,
    SplitChapters,
    Fixup,
    Exec,
    /// 为未来版本预留。**解析器不会产出它**（见 [`PostProcessKind::from_key`] 的白名单策略），
    /// 仅供调用方在需要时手工构造。
    Other(String),
}

impl PostProcessKind {
    /// 已知的后处理 key。返回 `None` 表示该行**不是**后处理标记。
    ///
    /// 这里刻意用**白名单**，而不是「key 是大驼峰就当成后处理」的猜测：
    /// yt-dlp 的提取器日志用的是完全一样的 `[Key] msg` 形态
    /// （实测 `[generic] Extracting URL:`、`[youtube] ...`）。
    /// 一旦误判，任务会在**下载尚未结束**时就跳进 `postprocessing` 状态。
    /// 未知 key 宁可忽略（只是少一个进度文案），也不冒误判风险。
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "Merger" => Self::Merger,
            "ExtractAudio" => Self::ExtractAudio,
            "EmbedSubtitle" => Self::EmbedSubtitle,
            "EmbedThumbnail" => Self::EmbedThumbnail,
            "Metadata" => Self::Metadata,
            "VideoRemuxer" => Self::VideoRemuxer,
            "VideoConvertor" => Self::VideoConvertor,
            "SubtitlesConvertor" => Self::SubtitlesConvertor,
            "ThumbnailsConvertor" => Self::ThumbnailsConvertor,
            "SplitChapters" => Self::SplitChapters,
            "Exec" => Self::Exec,
            k if k.starts_with("Fixup") => Self::Fixup,
            _ => return None,
        })
    }

    /// 面向用户的中文阶段名，用于「正在合并 / 正在嵌入字幕」等提示。
    pub fn label(&self) -> &str {
        match self {
            Self::Merger => "正在合并音视频",
            Self::ExtractAudio => "正在提取音频",
            Self::EmbedSubtitle => "正在嵌入字幕",
            Self::EmbedThumbnail => "正在嵌入缩略图",
            Self::Metadata => "正在写入元数据",
            Self::VideoRemuxer => "正在转封装",
            Self::SubtitlesConvertor => "正在转换字幕",
            Self::ThumbnailsConvertor => "正在转换缩略图",
            Self::VideoConvertor => "正在转换视频",
            Self::SplitChapters => "正在切分章节",
            Self::Fixup => "正在修复容器",
            Self::Exec => "正在执行后处理命令",
            Self::Other(_) => "正在后处理",
        }
    }
}

/// 一条后处理事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostProcess {
    /// 原始 key，如 `EmbedThumbnail`。
    pub key: String,
    pub kind: PostProcessKind,
    /// `[EmbedThumbnail]` 的中间 token 可变（`mutagen` / `ffmpeg` / `atomicparsley`）。
    pub method: Option<String>,
    pub message: String,
}

// ─────────────────────────── 跳过 ───────────────────────────

/// 静默跳过的原因。**两者都让 yt-dlp 返回 exit=0**，与真正下载完成无法从退出码区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `has already been recorded in the archive`
    Archive,
    /// `has already been downloaded`（成品文件已存在）
    FileExists,
}

impl SkipReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Archive => "已在下载归档中，已跳过",
            Self::FileExists => "文件已存在，已跳过",
        }
    }
}

// ─────────────────────────── 事件 ───────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Progress(Progress),
    /// aria2c **自身上报**的进度（DESIGN §11.2）。
    ///
    /// 用 aria2c 时 yt-dlp 一个进度事件都不发（实测 0 条），进度只能从这里来。
    /// 字段与 [`Progress`] 完全一致，这样界面侧不需要任何改动；
    /// 单独一个变体是为了让调用方能标注「此任务用了 aria2c，进度可能不精确」。
    Aria2cProgress(Progress),
    PostProcess(PostProcess),
    /// `[download] Destination: <path>` —— 注意这是 **temp** 路径。
    Destination(String),
    Skipped(SkipReason),
    Error(String),
    Warning(String),
}

/// 解析 aria2c 的大小写法：`B` / `KiB` / `MiB` / `GiB` / `TiB`。
///
/// 实测 aria2c 输出用的是二进制单位（`20MiB`）。
/// 十进制写法（`MB`）也一并接受——不同版本/配置下见过。
pub fn parse_size(s: &str) -> Option<u64> {
    parse_size_f64(s).map(|v| v as u64)
}

/// 同上但保留小数。**速度必须走这个**：`1.4MiB` 经 `u64` 会变成 `1468006`，
/// 丢掉的那 0.4 字节在速度显示上是无意义的精度损失。
fn parse_size_f64(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("NA") {
        return None;
    }
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let v: f64 = num.parse().ok()?;
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kib" | "kb" | "k" => 1024.0,
        "mib" | "mb" | "m" => 1024.0 * 1024.0,
        "gib" | "gb" | "g" => 1024.0_f64.powi(3),
        "tib" | "tb" | "t" => 1024.0_f64.powi(4),
        _ => return None,
    };
    Some(v * mult)
}

/// 解析 aria2c 的 ETA 写法：`28s` / `1m30s` / `1h2m3s` / `01:23` / `1:02:03`。
pub fn parse_eta(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // HH:MM:SS 或 MM:SS
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        let nums: Option<Vec<u64>> = parts.iter().map(|p| p.trim().parse::<u64>().ok()).collect();
        let nums = nums?;
        return match nums.len() {
            3 => Some(nums[0] * 3600 + nums[1] * 60 + nums[2]),
            2 => Some(nums[0] * 60 + nums[1]),
            _ => None,
        };
    }

    // 1h2m3s 形式；末尾的裸数字当秒
    let mut total = 0u64;
    let mut num = String::new();
    let mut any_unit = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
            continue;
        }
        let n: u64 = num.parse().ok()?;
        num.clear();
        any_unit = true;
        total += match ch.to_ascii_lowercase() {
            'h' => n * 3600,
            'm' => n * 60,
            's' => n,
            _ => return None,
        };
    }
    if !num.is_empty() {
        total += num.parse::<u64>().ok()?;
        any_unit = true;
    }
    any_unit.then_some(total)
}

/// 解析 aria2c 的进度行（DESIGN §11.2）。
///
/// 实测格式（`--summary-interval=1` 输出）：
/// ```text
/// [#5a8aba 1.0MiB/20MiB(5%) CN:16 DL:691KiB ETA:28s]
/// ```
///
/// 单位后缀与 `ETA:` 都可能缺失（例如连接数为 0 时），因此每个字段都是可选的。
pub fn parse_aria2c_summary(line: &str) -> Option<Progress> {
    let t = line.trim();
    let inner = t.strip_prefix("[#")?.strip_suffix(']')?;

    // 第一段是 gid（十六进制），后面才是内容
    let (gid, rest) = inner.split_once(char::is_whitespace)?;
    if gid.is_empty() {
        return None;
    }
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }

    // 已下/总量——必须是第一个 token，否则多半不是进度行。
    // 注意总量后面**紧跟着**括号百分比（`20MiB(5%)`），必须先切掉，
    // 否则单位会变成 `MiB(5%)` 而认不出来。
    let first = rest.split_whitespace().next()?;
    let (done_s, total_raw) = first.split_once('/')?;
    let total_s = total_raw.split('(').next().unwrap_or(total_raw);
    let downloaded = parse_size(done_s);
    // aria2c 在总大小未知时写 `0B`。映射成 `None`，否则
    // `is_indeterminate()`（看 total 是否为 None）会返回 false，
    // 而 `fraction()`（total 为 0 时返回 None）又给不出比例——两者自相矛盾。
    let total = parse_size(total_s).filter(|v| *v > 0);

    // 括号里的百分比是冗余的（可由两个大小推出），不单独保存。
    let field = |key: &str| rest.split_whitespace().find_map(|tok| tok.strip_prefix(key));

    Some(Progress {
        status: ProgressStatus::Downloading,
        downloaded,
        total,
        speed: field("DL:").and_then(parse_size_f64),
        eta: field("ETA:").and_then(parse_eta),
    })
}

/// 解析单行输出。无法识别时返回 `None`（调用方可当作普通日志）。
pub fn parse_line(line: &str) -> Option<Event> {
    let line = line.trim_end_matches(['\r', '\n']);
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // ── 1) 进度行：必须以 `download|` 开头 ──
    // 不能用 `starts_with("download")`，否则会误吃 `[download] Destination:`。
    if let Some(rest) = trimmed.strip_prefix(&format!("{PROGRESS_PREFIX}|")) {
        if let Some(p) = parse_progress(rest) {
            return Some(Event::Progress(p));
        }
        return None;
    }

    // ── 2) aria2c 自身上报的进度（DESIGN §11.2）──
    // 用 aria2c 时 yt-dlp 一条进度都不发，只能靠这个。
    // 它混在同一条 stdout 流里，所以必须先于 `[Key]` 后处理标记判断。
    if trimmed.starts_with("[#") {
        return parse_aria2c_summary(trimmed).map(Event::Aria2cProgress);
    }

    // ── 3) 错误 / 警告 ──
    if let Some(msg) = trimmed.strip_prefix("ERROR:") {
        return Some(Event::Error(msg.trim().to_string()));
    }
    if let Some(msg) = trimmed.strip_prefix("WARNING:") {
        return Some(Event::Warning(msg.trim().to_string()));
    }

    // ── 4) 后处理标记 `[Key] msg` ──
    if let Some(ev) = parse_postprocess(trimmed) {
        return Some(ev);
    }

    None
}

fn parse_progress(rest: &str) -> Option<Progress> {
    let parts: Vec<&str> = rest.split('|').collect();
    let status = ProgressStatus::parse(parts.first().copied().unwrap_or(""))?;

    // 按位置取值，容忍缺少尾部字段（实测不同版本段数可能不同）。
    // 注意：`rest` 的第 0 段就是 status（`download|` 已被剥掉），
    // 因此 `PROGRESS_FIELDS[i]` 对应 `parts[i]`，不是 `parts[i + 1]`。
    let field = |name: &str| -> Option<&str> {
        PROGRESS_FIELDS
            .iter()
            .position(|f| *f == name)
            .and_then(|i| parts.get(i).copied())
    };

    let downloaded: Option<u64> = field("downloaded_bytes").and_then(num);
    let total_bytes: Option<u64> = field("total_bytes").and_then(num);
    // HANDOFF §3.2：total_bytes 可能是 NA，必须回落到 total_bytes_estimate。
    let estimate: Option<u64> = field("total_bytes_estimate").and_then(num);
    let speed: Option<f64> = field("speed").and_then(num);
    let eta: Option<u64> = field("eta").and_then(num);

    Some(Progress {
        status,
        downloaded,
        total: total_bytes.or(estimate),
        speed,
        eta,
    })
}

fn parse_postprocess(line: &str) -> Option<Event> {
    let inner = line.strip_prefix('[')?;
    let close = inner.find(']')?;
    let key = &inner[..close];
    // key 必须是纯标识符，排除 `[download] ...` 之外的噪声。
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let message = inner[close + 1..].trim().to_string();

    // ── `[download]` 需要细分，它有四种含义 ──
    if key == "download" {
        if message.contains("has already been recorded in the archive") {
            return Some(Event::Skipped(SkipReason::Archive));
        }
        if message.contains("has already been downloaded") {
            return Some(Event::Skipped(SkipReason::FileExists));
        }
        if let Some(path) = message.strip_prefix("Destination:") {
            // 这是 temp 路径，不是 after_move 后的最终路径。
            return Some(Event::Destination(path.trim().to_string()));
        }
        return None;
    }

    // 不在白名单里 → 不是后处理标记（可能是提取器日志）。
    let kind = PostProcessKind::from_key(key)?;
    // 只有 EmbedThumbnail 的 message 带可变的 method 前缀。
    let method = if kind == PostProcessKind::EmbedThumbnail {
        message
            .split_once(':')
            .filter(|(m, _)| m.chars().all(|c| c.is_ascii_alphanumeric()) && !m.is_empty())
            .map(|(m, _)| m.to_string())
    } else {
        None
    };

    Some(Event::PostProcess(PostProcess {
        key: key.to_string(),
        kind,
        method,
        message,
    }))
}

/// 从 `--print-to-file` 的落地文件内容中取最终路径。
///
/// 该文件是 **append** 模式（`YoutubeDL.py:3255`），播放列表/多格式会累积多行，
/// 因此取**最后一行**；调用方应在每次派发前截断它。
pub fn parse_filepath_file(content: &str) -> Option<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .next_back()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::progress_template;

    fn prog(line: &str) -> Progress {
        match parse_line(line) {
            Some(Event::Progress(p)) => p,
            other => panic!("期望进度事件，得到 {other:?}"),
        }
    }

    /// 实测样例（HANDOFF §3.2 复现）。
    #[test]
    fn parses_real_progress_line() {
        let p = prog("download|downloading|1024|9437184|NA|939201.2455718347|10");
        assert_eq!(p.status, ProgressStatus::Downloading);
        assert_eq!(p.downloaded, Some(1024));
        assert_eq!(p.total, Some(9437184));
        assert_eq!(p.speed, Some(939201.2455718347));
        assert_eq!(p.eta, Some(10));
        assert!(!p.is_indeterminate());
    }

    /// HANDOFF §3.2 的实测样例：estimate 回落。
    #[test]
    fn falls_back_to_estimate() {
        let p = prog("download|downloading|1024|NA|3145728|NA|NA");
        assert_eq!(p.total, Some(3145728), "total_bytes 为 NA 时必须回落到 estimate");

        let p = prog("download|downloading|1024|NA|NA|1000|NA");
        assert_eq!(p.total, None);
        assert!(p.is_indeterminate());
        assert_eq!(p.fraction(), None, "总大小未知时不能给出比例（不要显示 0%）");
    }

    #[test]
    fn finished_status() {
        let p = prog("download|finished|3145728|3145728|NA|131417041.33|NA");
        assert!(p.is_finished());
        assert_eq!(p.fraction(), Some(1.0));
    }

    #[test]
    fn fraction_clamps() {
        let p = prog("download|downloading|9999999|1000|NA|NA|NA");
        assert_eq!(p.fraction(), Some(1.0));
    }

    #[test]
    fn total_zero_is_indeterminate() {
        let p = prog("download|downloading|0|0|NA|NA|NA");
        assert_eq!(p.fraction(), None);
    }

    /// 容忍尾部字段缺失。
    #[test]
    fn tolerates_short_lines() {
        let p = prog("download|downloading|512");
        assert_eq!(p.downloaded, Some(512));
        assert_eq!(p.total, None);
        assert_eq!(p.speed, None);
    }

    /// 进度模板与解析器必须对得上：这是跨模块的硬契约。
    #[test]
    fn template_and_parser_agree() {
        let tmpl = progress_template();
        let rendered = tmpl
            .replace("%(progress.status)s", "downloading")
            .replace("%(progress.downloaded_bytes)s", "2048")
            .replace("%(progress.total_bytes)s", "4096")
            .replace("%(progress.total_bytes_estimate)s", "NA")
            .replace("%(progress.speed)s", "512.5")
            .replace("%(progress.eta)s", "4");
        let p = prog(&rendered);
        assert_eq!(p.downloaded, Some(2048));
        assert_eq!(p.total, Some(4096));
        assert_eq!(p.speed, Some(512.5));
        assert_eq!(p.eta, Some(4));
        assert_eq!(p.fraction(), Some(0.5));
    }

    /// `[download]` 有四种含义，必须逐一分清。
    #[test]
    fn download_bracket_is_disambiguated() {
        // 1) destination（temp 路径）
        match parse_line("[download] Destination: D:\\t\\x.mp4") {
            Some(Event::Destination(p)) => assert_eq!(p, "D:\\t\\x.mp4"),
            other => panic!("期望 Destination，得到 {other:?}"),
        }
        // 2) 归档跳过
        assert_eq!(
            parse_line("[download] test: test has already been recorded in the archive"),
            Some(Event::Skipped(SkipReason::Archive))
        );
        // 3) 文件已存在跳过
        assert_eq!(
            parse_line("[download] D:\\o\\s.mp4 has already been downloaded"),
            Some(Event::Skipped(SkipReason::FileExists))
        );
        // 4) 普通下载日志不产生事件
        assert_eq!(parse_line("[download] 100% of 6.00MiB in 00:00:00"), None);
    }

    /// 静默跳过是「UI 会撒谎」的根源：必须能识别出来。
    #[test]
    fn skip_reasons_are_distinct_and_labelled() {
        assert_ne!(SkipReason::Archive, SkipReason::FileExists);
        assert!(SkipReason::Archive.label().contains("归档"));
        assert!(SkipReason::FileExists.label().contains("已存在"));
    }

    /// 实测的四种后处理标记。
    #[test]
    fn parses_real_postprocess_markers() {
        let cases = [
            (
                "[EmbedSubtitle] Embedding subtitles in \"C:\\o\\t.mp4\"",
                PostProcessKind::EmbedSubtitle,
                None,
            ),
            (
                "[Metadata] Adding metadata to \"C:\\o\\t.mp4\"",
                PostProcessKind::Metadata,
                None,
            ),
            (
                "[EmbedThumbnail] mutagen: Adding thumbnail to \"C:\\o\\t.mp4\"",
                PostProcessKind::EmbedThumbnail,
                Some("mutagen"),
            ),
            (
                "[EmbedThumbnail] ffmpeg: Adding thumbnail to \"C:\\o\\t.mkv\"",
                PostProcessKind::EmbedThumbnail,
                Some("ffmpeg"),
            ),
            (
                "[VideoRemuxer] Remuxing video from mp4 to mkv; Destination: C:\\o\\t.mkv",
                PostProcessKind::VideoRemuxer,
                None,
            ),
            ("[Merger] Merging formats into \"C:\\o\\t.mp4\"", PostProcessKind::Merger, None),
        ];
        for (line, kind, method) in cases {
            match parse_line(line) {
                Some(Event::PostProcess(pp)) => {
                    assert_eq!(pp.kind, kind, "行: {line}");
                    assert_eq!(pp.method.as_deref(), method, "行: {line}");
                }
                other => panic!("行 {line} 期望后处理事件，得到 {other:?}"),
            }
        }
    }

    /// EmbedThumbnail 的 method 不能误伤 `Destination:` 这类消息。
    #[test]
    fn method_extraction_is_scoped_to_thumbnail() {
        match parse_line("[EmbedSubtitle] Embedding subtitles in \"x\"") {
            Some(Event::PostProcess(pp)) => assert_eq!(pp.method, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fixup_family_is_grouped() {
        for k in ["FixupM3u8", "FixupM4a", "FixupStretched", "FixupTimestamp"] {
            assert_eq!(PostProcessKind::from_key(k), Some(PostProcessKind::Fixup));
        }
        // 未知 key 必须被拒绝，否则提取器日志会被误判成后处理事件。
        assert_eq!(PostProcessKind::from_key("SomethingNew"), None);
        assert_eq!(PostProcessKind::from_key("generic"), None);
        assert_eq!(PostProcessKind::from_key("youtube"), None);
        assert_eq!(PostProcessKind::from_key("info"), None);
    }

    #[test]
    fn every_kind_has_a_label() {
        for k in [
            PostProcessKind::Merger,
            PostProcessKind::EmbedSubtitle,
            PostProcessKind::EmbedThumbnail,
            PostProcessKind::Metadata,
            PostProcessKind::VideoRemuxer,
            PostProcessKind::Other("X".into()),
        ] {
            assert!(!k.label().is_empty());
        }
    }

    #[test]
    fn errors_and_warnings() {
        assert_eq!(
            parse_line("ERROR: Unsupported URL: https://x"),
            Some(Event::Error("Unsupported URL: https://x".into()))
        );
        assert_eq!(
            parse_line("WARNING: Falling back on generic information extractor"),
            Some(Event::Warning("Falling back on generic information extractor".into()))
        );
    }

    #[test]
    fn plain_logs_are_ignored() {
        for l in [
            "[generic] Extracting URL: https://x",
            "[info] test: Downloading 1 format(s): mp4",
            "[youtube] Extracting URL",
            "",
            "   ",
        ] {
            assert_eq!(parse_line(l), None, "行: {l:?}");
        }
    }

    /// 进度行不能被误当成后处理标记（两者都含 "download"）。
    #[test]
    fn progress_not_confused_with_postprocess() {
        match parse_line("download|downloading|1|2|NA|NA|NA") {
            Some(Event::Progress(_)) => {}
            other => panic!("期望 Progress，得到 {other:?}"),
        }
    }

    /// `--print-to-file` 是 append 模式，必须取最后一行。
    #[test]
    fn filepath_takes_last_line() {
        let content = "C:\\o\\a.mp4\nC:\\o\\b.mp4\n\n";
        assert_eq!(parse_filepath_file(content), Some("C:\\o\\b.mp4".to_string()));
        assert_eq!(parse_filepath_file(""), None);
        assert_eq!(parse_filepath_file("\n  \n"), None);
        assert_eq!(
            parse_filepath_file("  C:\\o\\only.mp4  "),
            Some("C:\\o\\only.mp4".to_string())
        );
    }

    // ─────────── aria2c 进度（DESIGN §11.2，用实测原文）───────────

    /// 实测原文：`--summary-interval=1` 输出的进度行。
    const ARIA2C_LINE: &str = "[#5a8aba 1.0MiB/20MiB(5%) CN:16 DL:691KiB ETA:28s]";

    #[test]
    fn parses_real_aria2c_line() {
        match parse_line(ARIA2C_LINE) {
            Some(Event::Aria2cProgress(p)) => {
                assert_eq!(p.downloaded, Some(1_048_576)); // 1.0 MiB
                assert_eq!(p.total, Some(20_971_520)); // 20 MiB
                assert_eq!(p.speed, Some(707_584.0)); // 691 KiB
                assert_eq!(p.eta, Some(28));
                assert_eq!(p.status, ProgressStatus::Downloading);
            }
            other => panic!("期望 Aria2cProgress，得到 {other:?}"),
        }
    }

    /// **关键**：aria2c 的进度必须能算出比例，否则界面拿不到可用信息。
    #[test]
    fn aria2c_progress_is_usable_by_ui() {
        let p = parse_aria2c_summary("[#abc 5.0MiB/20MiB(25%) CN:16 DL:1.4MiB ETA:10s]").unwrap();
        assert_eq!(p.fraction(), Some(0.25));
        assert!(!p.is_indeterminate());
        assert_eq!(p.speed, Some(1_468_006.4)); // 1.4 MiB
    }

    /// summary 块里的装饰行不能被误判成进度。
    #[test]
    fn aria2c_block_noise_is_ignored() {
        for l in [
            " *** Download Progress Summary as of Mon Sep 28 21:49:17 2026 *** ",
            "===============================================================================",
            "FILE: D:/tmp//./x.mp4.part",
            "-------------------------------------------------------------------------------",
            "[generic] Extracting URL: http://x",
        ] {
            assert_eq!(parse_line(l), None, "行: {l:?}");
        }
    }

    #[test]
    fn size_units() {
        assert_eq!(parse_size("512B"), Some(512));
        assert_eq!(parse_size("1KiB"), Some(1024));
        assert_eq!(parse_size("1.5MiB"), Some(1_572_864));
        assert_eq!(parse_size("2GiB"), Some(2_147_483_648));
        // 十进制写法兜底（不同版本/配置下见过）
        assert_eq!(parse_size("1MB"), Some(1_048_576));
        assert_eq!(parse_size("NA"), None);
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("abc"), None);
    }

    #[test]
    fn eta_formats() {
        assert_eq!(parse_eta("28s"), Some(28));
        assert_eq!(parse_eta("1m30s"), Some(90));
        assert_eq!(parse_eta("1h2m3s"), Some(3723));
        assert_eq!(parse_eta("01:23"), Some(83));
        assert_eq!(parse_eta("1:02:03"), Some(3723));
        assert_eq!(parse_eta("45"), Some(45)); // 裸数字当秒
        assert_eq!(parse_eta(""), None);
        assert_eq!(parse_eta("abc"), None);
    }

    /// 字段缺失不能整体失败——没有活跃连接时 aria2c 会省略 CN/DL/ETA。
    #[test]
    fn aria2c_tolerates_missing_fields() {
        let p = parse_aria2c_summary("[#abc 1.0MiB/20MiB(5%)]").unwrap();
        assert_eq!(p.downloaded, Some(1_048_576));
        assert_eq!(p.total, Some(20_971_520));
        assert_eq!(p.speed, None);
        assert_eq!(p.eta, None);
    }

    /// 总大小未知时应进入不确定态，而不是显示 0%。
    #[test]
    fn aria2c_unknown_total_is_indeterminate() {
        let p = parse_aria2c_summary("[#abc 1.0MiB/0B(0%) CN:1 DL:10KiB ETA:1h]").unwrap();
        assert!(p.is_indeterminate());
        assert_eq!(p.fraction(), None);
    }

    /// `[#` 开头但内容不对 → 不产生事件，也不能被当成后处理标记。
    #[test]
    fn aria2c_not_confused_with_postprocess() {
        assert!(matches!(
            parse_line("[Merger] Merging formats into \"x\""),
            Some(Event::PostProcess(_))
        ));
        assert_eq!(parse_line("[#abc not-a-progress-line]"), None);
        assert_eq!(parse_line("[#]"), None);
    }
}
