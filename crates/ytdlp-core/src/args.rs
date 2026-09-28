//! yt-dlp 命令行参数构造。
//!
//! 纯函数、零依赖、不碰文件系统 —— 全部行为可单测。
//! 每条参数的由来都标注了 `DESIGN.md` / `HANDOFF.md` 的出处。

use std::path::PathBuf;

// ─────────────────────────── 进度协议 ───────────────────────────

/// 进度模板的前缀。`parse.rs` 用它识别进度行。
pub const PROGRESS_PREFIX: &str = "download";

/// 进度字段，顺序即输出顺序（见 HANDOFF §3.2）。
///
/// 实测：`download|downloading|1024|9437184|NA|NA|NA`（共 7 段）。
/// `total_bytes` 与 `total_bytes_estimate` 都可能为 `NA`，解析时需回落。
pub const PROGRESS_FIELDS: [&str; 6] = [
    "status",
    "downloaded_bytes",
    "total_bytes",
    "total_bytes_estimate",
    "speed",
    "eta",
];

/// 构造 `--progress-template` 的值。
pub fn progress_template() -> String {
    let mut s = String::from(PROGRESS_PREFIX);
    for f in PROGRESS_FIELDS {
        s.push_str("|%(progress.");
        s.push_str(f);
        s.push_str(")s");
    }
    s
}

/// 最终路径落地文件名（放在任务的 temp 目录里）。
pub const FILEPATH_FILE: &str = "filepath.txt";

// ─────────────────────────── 类型 ───────────────────────────

/// 容器选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// 由宿主按「是否嵌入」自动决定（见 [`resolve_container`]）。
    Auto,
    Mp4,
    Mkv,
    Webm,
}

impl Container {
    pub fn as_str(self) -> &'static str {
        match self {
            Container::Auto => unreachable!("Auto 应在 resolve_container 中被解析"),
            Container::Mp4 => "mp4",
            Container::Mkv => "mkv",
            Container::Webm => "webm",
        }
    }
}

/// 音频提取的目标格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    M4a,
    Opus,
    Flac,
    Wav,
}

impl AudioFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::M4a => "m4a",
            AudioFormat::Opus => "opus",
            AudioFormat::Flac => "flac",
            AudioFormat::Wav => "wav",
        }
    }
}

/// 格式预设。UI 只暴露语义选项，宿主翻译成 `-f` 表达式（DESIGN §3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preset {
    /// 最佳视频+音频。
    Best,
    /// 分辨率上限。
    MaxHeight(u32),
    /// 仅音频。
    AudioOnly(AudioFormat),
}

/// 翻译成 `-f` 表达式。
///
/// **绝不存 `format_id`**（DESIGN §3）：表达式可离线复现、任务可重放，
/// 且不需要持久化 info.json。
pub fn preset_expression(preset: &Preset) -> String {
    match preset {
        Preset::Best => "bv*+ba/b".to_string(),
        Preset::MaxHeight(h) => format!("bv*[height<={h}]+ba/b[height<={h}]/b"),
        Preset::AudioOnly(_) => "ba/b".to_string(),
    }
}

/// 嵌入选项（DESIGN §14）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EmbedOptions {
    pub subs: bool,
    /// `--sub-langs` 的值。支持逗号分隔、`all`、`-` 排除、正则。
    pub sub_langs: String,
    /// 自动生成字幕需要 `--write-auto-subs`（DESIGN §14.6）。
    pub auto_subs: bool,
    /// 显式 `--write-subs`：嵌入后**保留** .vtt 文件（DESIGN §14.5）。
    pub keep_sub_files: bool,
    pub thumbnail: bool,
    /// 显式 `--write-thumbnail`：嵌入后**保留**缩略图文件。
    pub keep_thumbnail_file: bool,
    pub metadata: bool,
    /// `--embed-metadata` 会**连带**嵌入章节与 infojson，
    /// 除非显式传 `--no-embed-chapters` / `--no-embed-info-json`（DESIGN §14.1）。
    pub chapters: bool,
    pub info_json: bool,
}

impl EmbedOptions {
    pub fn any(&self) -> bool {
        self.subs || self.thumbnail || self.metadata
    }
}

/// 容器解析：启用嵌入时默认切到 mkv（DESIGN §14.2）。
///
/// 原因：`--embed-thumbnail` 在 webm 上**直接抛错**；mp4 的缩略图走
/// `mutagen → AtomicParsley → ffmpeg` 三级回落且字幕只能用 `mov_text`；
/// 而 **infojson 只能挂到 mkv/mka**。mkv 是唯一全绿的容器。
pub fn resolve_container(requested: Container, embed: &EmbedOptions) -> Container {
    match requested {
        Container::Auto if embed.any() => Container::Mkv,
        Container::Auto => Container::Mp4,
        explicit => explicit,
    }
}

/// Cookie 来源（DESIGN §6）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookieSource {
    /// 主路径：Netscape 格式的 cookies.txt。
    File(PathBuf),
    /// 辅助路径。实测 Chrome/Edge 在 Windows 上均失败，仅 Firefox 可用。
    Browser(String),
}

/// JS 运行时配置。
///
/// ## 为什么这个不能省
///
/// 实测：**同一份 yt-dlp、同一个链接、同样的 cookie 与代理**，只差
/// `--js-runtimes node`，结果就是「需要重载页面」与「成功」的区别。
///
/// yt-dlp **不会**自动启用系统上已安装的 JS 运行时（尽管 `--no-js-runtimes`
/// 的说明里提到有 "defaults"）。而 YouTube 的 n-sig / player 挑战需要 JS
/// 运行时来解 —— 解不了站点就降级返回（只给 storyboard，或要求重新登录）。
///
/// `--remote-components` 一般**不需要**：官方可执行文件的说明写着
/// "currently not needed if you are using an official executable"。
/// 实测加上它会把一次探测从 29s 拉到 69s，所以默认关闭。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsRuntimeOptions {
    /// 要启用的运行时名（如 `["node"]`）。空表示不传该参数。
    pub runtimes: Vec<String>,
    /// 是否附加 `--remote-components ejs:npm`。
    pub remote_components: bool,
}

impl JsRuntimeOptions {
    pub fn new(runtimes: Vec<String>, remote_components: bool) -> Self {
        Self {
            runtimes,
            remote_components,
        }
    }

    pub fn is_enabled(&self) -> bool {
        !self.runtimes.is_empty()
    }
}

/// yt-dlp 认识的 JS 运行时名，按偏好排序（node 生态最成熟，排最前）。
pub const JS_RUNTIMES: [&str; 4] = ["node", "deno", "bun", "quickjs"];

/// 把 JS 运行时相关的参数追加进去。
fn push_js_args(a: &mut Vec<String>, js: &JsRuntimeOptions) {
    if js.is_enabled() {
        // 一次传一个：`--js-runtimes node --js-runtimes bun`
        for r in &js.runtimes {
            let r = r.trim();
            if !r.is_empty() {
                a.push("--js-runtimes".into());
                a.push(r.to_string());
            }
        }
    }
    if js.remote_components {
        a.push("--remote-components".into());
        a.push("ejs:npm".into());
    }
}

/// 一次下载任务的全部输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadSpec {
    pub url: String,
    /// 任务的稳定 ID。temp 目录由它推导（DESIGN §5.1），**不可每次随机**。
    pub task_id: String,
    pub output_dir: PathBuf,
    /// 任务专属 temp 目录，必须可跨重启稳定重建，否则断点续传失效。
    pub temp_dir: PathBuf,
    pub preset: Preset,
    /// 用户在高级格式表里**显式选定**的 `-f` 表达式，`Some` 时覆盖 `preset`。
    ///
    /// 存表达式而不是 format_id 清单（DESIGN §3）：任务永远可重放，
    /// 也不需要在数据库里保存 info.json。
    ///
    /// `None` 表示「跟随设置里的预设」——这是新建任务的默认状态，
    /// 此时改设置里的预设能立刻影响尚未下载的任务。
    pub format_override: Option<String>,
    pub container: Container,
    pub embed: EmbedOptions,
    pub proxy: Option<String>,
    pub cookies: Option<CookieSource>,
    pub archive: Option<PathBuf>,
    /// 外部下载器。**默认关闭**（DESIGN §11.6）。
    pub aria2c: bool,
    pub limit_rate: Option<String>,
    /// 输出文件名模板。默认带 `%(title).150B` 截断，防止超长标题撑爆 Windows 路径。
    pub filename_template: String,
    /// 播放列表选集，如 `"1,3,5-7"`。`None` 表示全部。
    ///
    /// 由用户在勾选后生成，交给 yt-dlp 的 `--playlist-items` 处理——
    /// 比在宿主侧为每个条目建一条任务简单得多，也不会打乱 `-f` 语义。
    pub playlist_items: Option<String>,
    /// JS 运行时。**YouTube 的 n-sig 挑战靠它**，不给就会出现
    /// 「需要重载页面」或只返回 storyboard。
    pub js: JsRuntimeOptions,
}

/// 把勾选的下标（0-based）转成 `--playlist-items` 的值。
///
/// 去重、升序，并合并连续区间（`1-3` 比 `1,2,3` 更短）。
/// 传入空列表返回 `None`（表示「不限制」而非「什么都不下」）。
pub fn playlist_items_spec(selected: &[usize]) -> Option<String> {
    if selected.is_empty() {
        return None;
    }
    let mut v: Vec<usize> = selected.iter().map(|i| i + 1).collect();
    v.sort_unstable();
    v.dedup();

    let mut parts: Vec<String> = Vec::new();
    let mut start = v[0];
    let mut prev = v[0];
    for &n in &v[1..] {
        if n == prev + 1 {
            prev = n;
            continue;
        }
        parts.push(if start == prev {
            start.to_string()
        } else {
            format!("{start}-{prev}")
        });
        start = n;
        prev = n;
    }
    parts.push(if start == prev {
        start.to_string()
    } else {
        format!("{start}-{prev}")
    });
    Some(parts.join(","))
}

impl DownloadSpec {
    /// 用合理的默认值构造。
    pub fn new(
        url: impl Into<String>,
        task_id: impl Into<String>,
        output_dir: impl Into<PathBuf>,
        temp_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            url: url.into(),
            task_id: task_id.into(),
            output_dir: output_dir.into(),
            temp_dir: temp_dir.into(),
            preset: Preset::Best,
            format_override: None,
            container: Container::Auto,
            embed: EmbedOptions::default(),
            proxy: None,
            cookies: None,
            archive: None,
            aria2c: false,
            limit_rate: None,
            filename_template: "%(title).150B [%(id)s].%(ext)s".to_string(),
            playlist_items: None,
            js: JsRuntimeOptions::default(),
        }
    }

    /// 最终路径的落地文件（`--print-to-file` 的目标）。
    pub fn filepath_file(&self) -> PathBuf {
        self.temp_dir.join(FILEPATH_FILE)
    }

    /// 真正传给 `-f` 的表达式：用户选的优先，否则用预设。
    ///
    /// 空白字符串视为「没选」——界面上清空输入框不该产出一个非法的 `-f`。
    pub fn effective_format_expression(&self) -> String {
        self.format_override
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| preset_expression(&self.preset))
    }
}

// ─────────────────────────── 高级格式表 → 表达式 ───────────────────────────

/// 把「用户在格式表里选中的一行」翻译成 `-f` 表达式（DESIGN §3）。
///
/// **必须按音视频是否分开来翻译**：
/// - 仅视频轨 → `<id>+ba/<id>`：自动配一条最佳音频轨，回退是只要视频
/// - 其余（已封装 / 仅音频） → 只用 `<id>`
///
/// 早期版本对所有「有视频编码」的行都追加 `+ba`，于是选了 `18`（本来就带音频）
/// 会变成 `18+ba/18`——多此一举地再合一条音轨。
pub fn format_expression_for(
    format_id: &str,
    vcodec: Option<&str>,
    acodec: Option<&str>,
) -> String {
    match crate::probe::format_kind(vcodec, acodec) {
        crate::probe::FormatKind::VideoOnly => format!("{format_id}+ba/{format_id}"),
        _ => format_id.to_string(),
    }
}

// ─────────────────────────── 探测 ───────────────────────────

/// `--dump-single-json` 探测元数据。
///
/// Cookie 必须在此阶段就生效：很多站点不登录连元数据都拿不到（DESIGN §6）。
///
/// `flat_playlist` 建议传 `true`：实测 `--flat-playlist` 用在**单个视频**上
/// 同样会返回完整的 `formats`，所以一次探测就能同时覆盖两种情况——
/// 播放列表得到扁平条目供勾选，单视频得到格式表（DESIGN §9）。
pub fn build_probe_args(
    url: &str,
    proxy: Option<&str>,
    cookies: Option<&CookieSource>,
    flat_playlist: bool,
    js: &JsRuntimeOptions,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "--dump-single-json".into(),
        "--skip-download".into(),
        "--no-warnings".into(),
        "--no-colors".into(),
        "--socket-timeout".into(),
        "15".into(),
        "--extractor-retries".into(),
        "3".into(),
    ];

    // 播放列表：先拿扁平条目列表，让用户勾选（DESIGN §9）。
    if flat_playlist {
        a.push("--flat-playlist".into());
    } else {
        a.push("--no-playlist".into());
    }

    // 探测阶段同样要 JS 运行时——否则拿到的就是降级结果
    push_js_args(&mut a, js);
    push_proxy(&mut a, proxy);
    push_cookies(&mut a, cookies);
    a.push("--".into());
    a.push(url.to_string());
    a
}

// ─────────────────────────── 下载 ───────────────────────────

/// 构造完整下载命令的参数（不含 exe 自身路径）。
///
/// ## 关于 `--print` 的重要说明
///
/// 这里**刻意不用 `--print after_move:filepath`**。实测：
///
/// | 配置 | 进度行 |
/// |---|---|
/// | 仅 `--progress-template` | 7 |
/// | 加上 `--print` | **0** |
///
/// `--print` 隐含 `--quiet`，会把进度输出整个压掉。改用
/// `--print-to-file`（它不隐含 `--quiet`），实测两者兼得。
/// 详见 HANDOFF §3.3。
pub fn build_download_args(spec: &DownloadSpec) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "--newline".into(),
        // 必须加：否则 stdout 混入 ANSI 转义序列，解析器会莫名失败。
        "--no-colors".into(),
        "--no-warnings".into(),
        "--progress-delta".into(),
        "0.2".into(),
        "--progress-template".into(),
        progress_template(),
        // 最终路径落地到文件，避免 --print 压制进度。
        "--print-to-file".into(),
        "after_move:filepath".into(),
        spec.filepath_file().to_string_lossy().into_owned(),
        "--continue".into(),
        // 不要传 --no-part：保留 .part 才有断点续传（HANDOFF §3.6）。
        "--retries".into(),
        "10".into(),
        "--fragment-retries".into(),
        "10".into(),
        "--no-mtime".into(),
        "--paths".into(),
        format!("home:{}", spec.output_dir.display()),
        // temp 目录按 task_id 稳定推导，否则恢复时找不到 .part（DESIGN §5.1）。
        "--paths".into(),
        format!("temp:{}", spec.temp_dir.display()),
        "--output".into(),
        spec.filename_template.clone(),
    ];

    // JS 运行时放在前面：它是「能不能拿到真实格式」的前提，
    // 不是可选优化（见 JsRuntimeOptions 的说明）。
    push_js_args(&mut a, &spec.js);

    // ── 格式 ──
    a.push("--format".into());
    a.push(spec.effective_format_expression());

    match &spec.preset {
        Preset::AudioOnly(fmt) => {
            a.push("--extract-audio".into());
            a.push("--audio-format".into());
            a.push(fmt.as_str().into());
            a.push("--audio-quality".into());
            a.push("0".into());
        }
        _ => {
            let container = resolve_container(spec.container, &spec.embed);
            a.push("--merge-output-format".into());
            a.push(container.as_str().into());
        }
    }

    push_embed_args(&mut a, spec);
    push_downloader_args(&mut a, spec);
    push_proxy(&mut a, spec.proxy.as_deref());
    push_cookies(&mut a, spec.cookies.as_ref());

    if let Some(rate) = &spec.limit_rate {
        a.push("--limit-rate".into());
        a.push(rate.clone());
    }
    if let Some(archive) = &spec.archive {
        a.push("--download-archive".into());
        a.push(archive.display().to_string());
    }
    if let Some(items) = &spec.playlist_items {
        a.push("--playlist-items".into());
        a.push(items.clone());
    }

    a.push("--".into());
    a.push(spec.url.clone());
    a
}

fn push_embed_args(a: &mut Vec<String>, spec: &DownloadSpec) {
    let e = &spec.embed;
    if !e.any() {
        return;
    }

    if e.subs {
        a.push("--embed-subs".into());
        // 只有显式给了 --write-subs，嵌入后才会保留 .vtt（DESIGN §14.5）。
        if e.keep_sub_files {
            a.push("--write-subs".into());
        }
        if e.auto_subs {
            // 自动生成字幕需要它，仅 --write-subs 拿不到（DESIGN §14.6）。
            a.push("--write-auto-subs".into());
        }
        let langs = if e.sub_langs.trim().is_empty() {
            "all,-live_chat"
        } else {
            e.sub_langs.trim()
        };
        a.push("--sub-langs".into());
        a.push(langs.to_string());
    }

    if e.thumbnail {
        a.push("--embed-thumbnail".into());
        if e.keep_thumbnail_file {
            a.push("--write-thumbnail".into());
        }
    }

    if e.metadata {
        a.push("--embed-metadata".into());
        // --embed-metadata 默认连带嵌入章节与 infojson，必须显式关闭
        // 不需要的部分，否则是隐藏副作用（DESIGN §14.1）。
        if !e.chapters {
            a.push("--no-embed-chapters".into());
        }
        if !e.info_json {
            a.push("--no-embed-info-json".into());
        }
    }
}

fn push_downloader_args(a: &mut Vec<String>, spec: &DownloadSpec) {
    if !spec.aria2c {
        return;
    }

    a.push("--external-downloader".into());
    a.push("aria2c".into());
    // aria2c 只支持 http/https/ftp/ftps，分片流必须交回原生下载器，
    // 否则 yt-dlp 的 ExternalFD::supports() 会拒绝（DESIGN §11）。
    a.push("--downloader".into());
    a.push("dash,m3u8:native".into());

    // 两处必须覆盖 yt-dlp 的硬编码默认值：
    //  · -x16/-s16 要求服务器支持 Range，不支持时 aria2c 会失败（DESIGN §11.4）
    //  · --summary-interval 默认 0，会让宿主完全拿不到进度（DESIGN §11.1/11.2）
    //  · --enable-color=false：注意选项名不是 --console-log-color（实测不存在）
    a.push("--downloader-args".into());
    a.push("aria2c:-x4 -s4 --summary-interval=1 --enable-color=false".into());
}

fn push_proxy(a: &mut Vec<String>, proxy: Option<&str>) {
    // 探测与下载两处都必须传（DESIGN §7）。
    if let Some(p) = proxy.filter(|p| !p.trim().is_empty()) {
        a.push("--proxy".into());
        a.push(p.trim().to_string());
    }
}

fn push_cookies(a: &mut Vec<String>, cookies: Option<&CookieSource>) {
    match cookies {
        Some(CookieSource::File(p)) => {
            a.push("--cookies".into());
            a.push(p.display().to_string());
        }
        Some(CookieSource::Browser(b)) => {
            a.push("--cookies-from-browser".into());
            a.push(b.clone());
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> DownloadSpec {
        DownloadSpec::new(
            "https://example.com/v",
            "task-1",
            PathBuf::from("C:/out"),
            PathBuf::from("C:/tmp/task-1"),
        )
    }

    /// 进度模板必须与 HANDOFF §3.2 实测输出一致（7 段）。
    #[test]
    fn progress_template_shape() {
        let t = progress_template();
        let parts: Vec<&str> = t.split('|').collect();
        assert_eq!(parts.len(), 7, "模板: {t}");
        assert_eq!(parts[0], "download");
        assert_eq!(parts[1], "%(progress.status)s");
        assert_eq!(parts[6], "%(progress.eta)s");
    }

    /// 关键回归：`--print` 会隐含 `--quiet` 并压掉进度，必须用 `--print-to-file`。
    #[test]
    fn never_uses_print_flag() {
        let a = build_download_args(&spec());
        assert!(
            !a.iter().any(|x| x == "--print"),
            "出现 --print 会压掉全部进度输出（实测 7 条 -> 0 条）"
        );
        assert!(a.iter().any(|x| x == "--print-to-file"));
    }

    #[test]
    fn no_colors_always_present() {
        // 少了它 stdout 会混入 ANSI，解析必然失败。
        assert!(build_download_args(&spec()).iter().any(|x| x == "--no-colors"));
    }

    #[test]
    fn temp_dir_is_stable_and_task_scoped() {
        let s = spec();
        let a = build_download_args(&s);
        let idx = a.iter().position(|x| x == "--paths").unwrap();
        assert!(a.contains(&"temp:C:/tmp/task-1".to_string()));
        assert_eq!(a[idx + 1], "home:C:/out");
        // 断点续传依赖 .part 落在稳定目录（DESIGN §5.1）。
        assert_eq!(s.filepath_file(), PathBuf::from("C:/tmp/task-1/filepath.txt"));
    }

    #[test]
    fn never_disables_part_files() {
        // --no-part 会让断点续传失效。
        assert!(!build_download_args(&spec()).iter().any(|x| x == "--no-part"));
    }

    #[test]
    fn preset_expressions() {
        assert_eq!(preset_expression(&Preset::Best), "bv*+ba/b");
        assert_eq!(
            preset_expression(&Preset::MaxHeight(1080)),
            "bv*[height<=1080]+ba/b[height<=1080]/b"
        );
        assert_eq!(
            preset_expression(&Preset::AudioOnly(AudioFormat::Mp3)),
            "ba/b"
        );
        // 表达式形态是硬契约：它被存进数据库用于重放（DESIGN §3）。
    }

    #[test]
    fn audio_only_uses_extract_audio_not_merge() {
        let mut s = spec();
        s.preset = Preset::AudioOnly(AudioFormat::Mp3);
        let a = build_download_args(&s);
        assert!(a.contains(&"--extract-audio".to_string()));
        assert!(a.contains(&"mp3".to_string()));
        assert!(
            !a.contains(&"--merge-output-format".to_string()),
            "仅音频不应走 merge"
        );
    }

    /// DESIGN §14.2 的核心结论：启用嵌入时自动切到 mkv。
    #[test]
    fn container_auto_switches_to_mkv_when_embedding() {
        let plain = EmbedOptions::default();
        assert_eq!(resolve_container(Container::Auto, &plain), Container::Mp4);

        for e in [
            EmbedOptions { subs: true, ..Default::default() },
            EmbedOptions { thumbnail: true, ..Default::default() },
            EmbedOptions { metadata: true, ..Default::default() },
        ] {
            assert_eq!(
                resolve_container(Container::Auto, &e),
                Container::Mkv,
                "启用嵌入后必须切 mkv"
            );
        }
    }

    /// 用户显式指定容器时不覆盖其选择。
    #[test]
    fn explicit_container_is_respected() {
        let e = EmbedOptions { thumbnail: true, ..Default::default() };
        assert_eq!(resolve_container(Container::Mp4, &e), Container::Mp4);
        assert_eq!(resolve_container(Container::Webm, &e), Container::Webm);
    }

    /// DESIGN §14.1：`--embed-metadata` 的章节/infojson 副作用必须显式关闭。
    #[test]
    fn embed_metadata_disables_hidden_side_effects() {
        let mut s = spec();
        s.embed.metadata = true;
        let a = build_download_args(&s);
        assert!(a.contains(&"--embed-metadata".to_string()));
        assert!(a.contains(&"--no-embed-chapters".to_string()));
        assert!(a.contains(&"--no-embed-info-json".to_string()));
    }

    #[test]
    fn embed_metadata_keeps_chapters_when_requested() {
        let mut s = spec();
        s.embed.metadata = true;
        s.embed.chapters = true;
        let a = build_download_args(&s);
        assert!(!a.contains(&"--no-embed-chapters".to_string()));
        assert!(a.contains(&"--no-embed-info-json".to_string()));
    }

    /// DESIGN §14.5：保留字幕文件与嵌入是两个独立意图。
    #[test]
    fn sub_file_retention_is_explicit() {
        let mut s = spec();
        s.embed.subs = true;
        let without = build_download_args(&s);
        assert!(!without.contains(&"--write-subs".to_string()));
        assert!(without.contains(&"--embed-subs".to_string()));

        s.embed.keep_sub_files = true;
        assert!(build_download_args(&s).contains(&"--write-subs".to_string()));
    }

    /// DESIGN §14.6：自动字幕需要 --write-auto-subs。
    #[test]
    fn auto_subs_flag() {
        let mut s = spec();
        s.embed.subs = true;
        s.embed.auto_subs = true;
        assert!(build_download_args(&s).contains(&"--write-auto-subs".to_string()));
    }

    #[test]
    fn sub_langs_defaults_and_passthrough() {
        let mut s = spec();
        s.embed.subs = true;
        let a = build_download_args(&s);
        let i = a.iter().position(|x| x == "--sub-langs").unwrap();
        assert_eq!(a[i + 1], "all,-live_chat");

        s.embed.sub_langs = "zh-Hans,en".into();
        let a = build_download_args(&s);
        let i = a.iter().position(|x| x == "--sub-langs").unwrap();
        assert_eq!(a[i + 1], "zh-Hans,en");
    }

    /// DESIGN §11：aria2c 的三条硬要求。
    #[test]
    fn aria2c_overrides() {
        let mut s = spec();
        s.aria2c = true;
        let a = build_download_args(&s);

        assert!(a.contains(&"--external-downloader".to_string()));
        assert!(a.contains(&"aria2c".to_string()));
        // 分片流必须交回原生下载器，aria2c 不支持。
        assert!(a.contains(&"dash,m3u8:native".to_string()));

        let i = a.iter().position(|x| x == "--downloader-args").unwrap();
        let args = &a[i + 1];
        // 选项名实测为 --enable-color，--console-log-color 不存在。
        assert!(args.contains("--enable-color=false"));
        assert!(!args.contains("--console-log-color"));
        // 必须削弱默认的 -x16：不支持 Range 的服务器会让下载失败。
        assert!(args.contains("-x4"));
        // 不覆盖 summary-interval 就完全拿不到进度。
        assert!(args.contains("--summary-interval=1"));
    }

    #[test]
    fn aria2c_absent_by_default() {
        let a = build_download_args(&spec());
        assert!(!a.contains(&"--external-downloader".to_string()));
    }

    /// Cookie 主路径是文件；浏览器路径仅作辅助（DESIGN §6）。
    #[test]
    fn cookies_both_paths() {
        let mut s = spec();
        s.cookies = Some(CookieSource::File(PathBuf::from("C:/c/cookies.txt")));
        let a = build_download_args(&s);
        let i = a.iter().position(|x| x == "--cookies").unwrap();
        assert_eq!(a[i + 1], "C:/c/cookies.txt");

        s.cookies = Some(CookieSource::Browser("firefox".into()));
        let a = build_download_args(&s);
        let i = a.iter().position(|x| x == "--cookies-from-browser").unwrap();
        assert_eq!(a[i + 1], "firefox");
        assert!(!a.contains(&"--cookies".to_string()));
    }

    /// Cookie 必须在探测阶段就生效，否则用户只看到「探测失败」。
    #[test]
    fn probe_carries_cookies_and_proxy() {
        let c = CookieSource::File(PathBuf::from("C:/c.txt"));
        let a = build_probe_args(
            "https://x",
            Some("http://127.0.0.1:7897"),
            Some(&c),
            false,
            &JsRuntimeOptions::default(),
        );
        assert!(a.contains(&"--cookies".to_string()));
        assert!(a.contains(&"http://127.0.0.1:7897".to_string()));
        assert!(a.contains(&"--no-playlist".to_string()));
        assert_eq!(a.last().unwrap(), "https://x");
    }

    /// DESIGN §9：播放列表先取扁平列表供勾选。
    #[test]
    fn probe_flat_playlist_switches_flag() {
        let a = build_probe_args("https://x", None, None, true, &JsRuntimeOptions::default());
        assert!(a.contains(&"--flat-playlist".to_string()));
        assert!(!a.contains(&"--no-playlist".to_string()));
    }

    #[test]
    fn proxy_applies_to_both_probe_and_download() {
        let mut s = spec();
        s.proxy = Some("socks5://127.0.0.1:1080".into());
        assert!(build_download_args(&s).contains(&"socks5://127.0.0.1:1080".to_string()));
        let p = build_probe_args(
            "https://x",
            s.proxy.as_deref(),
            None,
            false,
            &JsRuntimeOptions::default(),
        );
        assert!(p.contains(&"socks5://127.0.0.1:1080".to_string()));
    }

    #[test]
    fn blank_proxy_is_ignored() {
        let mut s = spec();
        s.proxy = Some("   ".into());
        assert!(!build_download_args(&s).contains(&"--proxy".to_string()));
    }

    /// URL 前必须有 `--`，否则以 `-` 开头的 URL 会被当成选项。
    #[test]
    fn url_is_guarded() {
        let mut s = spec();
        s.url = "-weird-url".into();
        let a = build_download_args(&s);
        let i = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(a[i + 1], "-weird-url");
    }

    #[test]
    fn archive_is_wired() {
        let mut s = spec();
        s.archive = Some(PathBuf::from("C:/a/archive.txt"));
        let a = build_download_args(&s);
        let i = a.iter().position(|x| x == "--download-archive").unwrap();
        assert_eq!(a[i + 1], "C:/a/archive.txt");
    }

    /// 播放列表选集：连续区间要合并，否则超长列表会撑爆命令行。
    #[test]
    fn playlist_items_merges_ranges() {
        assert_eq!(playlist_items_spec(&[]), None, "空选择表示不限制，而非全不选");
        assert_eq!(playlist_items_spec(&[0]), Some("1".into()));
        assert_eq!(playlist_items_spec(&[0, 1, 2]), Some("1-3".into()));
        assert_eq!(playlist_items_spec(&[0, 2, 3, 5]), Some("1,3-4,6".into()));
        // 乱序 + 重复
        assert_eq!(playlist_items_spec(&[2, 0, 1, 2]), Some("1-3".into()));
        assert_eq!(playlist_items_spec(&[4, 0]), Some("1,5".into()));
    }

    #[test]
    fn playlist_items_wired_into_args() {
        let mut s = spec();
        s.playlist_items = Some("1,3-4".into());
        let a = build_download_args(&s);
        let i = a.iter().position(|x| x == "--playlist-items").unwrap();
        assert_eq!(a[i + 1], "1,3-4");

        assert!(!build_download_args(&spec()).contains(&"--playlist-items".to_string()));
    }

    // ─────────── JS 运行时（YouTube 的 n-sig 挑战靠它）───────────

    /// 实测：不带这个参数时 YouTube 会返回「需要重载页面」或只给 storyboard。
    #[test]
    fn js_runtimes_wired_into_download_args() {
        let mut s = spec();
        s.js = JsRuntimeOptions::new(vec!["node".into()], false);
        let a = build_download_args(&s);
        let i = a.iter().position(|x| x == "--js-runtimes").unwrap();
        assert_eq!(a[i + 1], "node");
    }

    #[test]
    fn js_runtimes_wired_into_probe_args() {
        let js = JsRuntimeOptions::new(vec!["node".into()], false);
        let a = build_probe_args("https://x", None, None, true, &js);
        let i = a.iter().position(|x| x == "--js-runtimes").unwrap();
        assert_eq!(a[i + 1], "node");
        // 探测阶段必须也有——否则拿到的是降级结果
    }

    /// 多个运行时各传一次（yt-dlp 的写法是重复该选项）。
    #[test]
    fn multiple_js_runtimes_repeat_the_flag() {
        let js = JsRuntimeOptions::new(vec!["node".into(), "bun".into()], false);
        let a = build_download_args(&{
            let mut s = spec();
            s.js = js;
            s
        });
        let flags: Vec<usize> = a
            .iter()
            .enumerate()
            .filter(|(_, x)| *x == "--js-runtimes")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(flags.len(), 2);
        assert_eq!(a[flags[0] + 1], "node");
        assert_eq!(a[flags[1] + 1], "bun");
    }

    /// 未检测到运行时时不该出现这个参数（免得让 yt-dlp 报未知运行时）。
    #[test]
    fn no_js_flag_when_nothing_detected() {
        let a = build_download_args(&spec());
        assert!(!a.contains(&"--js-runtimes".to_string()));
        assert!(!a.contains(&"--remote-components".to_string()));

        let js = JsRuntimeOptions::default();
        let p = build_probe_args("https://x", None, None, false, &js);
        assert!(!p.contains(&"--js-runtimes".to_string()));
    }

    /// `--remote-components` 默认关闭：官方 exe 不需要它，且实测会慢 40 秒。
    #[test]
    fn remote_components_is_opt_in() {
        let mut s = spec();
        s.js = JsRuntimeOptions::new(vec!["node".into()], false);
        assert!(!build_download_args(&s).contains(&"--remote-components".to_string()));

        s.js = JsRuntimeOptions::new(vec!["node".into()], true);
        let a = build_download_args(&s);
        let i = a.iter().position(|x| x == "--remote-components").unwrap();
        assert_eq!(a[i + 1], "ejs:npm");
    }

    #[test]
    fn js_runtime_names_match_ytdlp() {
        assert_eq!(JS_RUNTIMES, ["node", "deno", "bun", "quickjs"]);
        assert!(JS_RUNTIMES.contains(&"node"));
    }

    #[test]
    fn js_runtime_blank_entries_skipped() {
        let js = JsRuntimeOptions::new(vec!["node".into(), "  ".into()], false);
        let mut s = spec();
        s.js = js;
        let a = build_download_args(&s);
        let n = a.iter().filter(|x| *x == "--js-runtimes").count();
        assert_eq!(n, 1, "空白项不该产生额外的 --js-runtimes");
    }

    // ───────── 高级格式表：音视频分开时怎么翻译 ─────────
    // `format_kind` 本身的用例在 probe.rs（分类属于格式解析的范畴）

    /// **这是用户实际遇到的问题**：YouTube 之类的 DASH 站点把音视频分开列，
    /// 单点一条视频轨是没有声音的。仅视频轨要自动配一条音频轨。
    #[test]
    fn video_only_format_pairs_best_audio() {
        assert_eq!(
            format_expression_for("137", Some("avc1.640028"), Some("none")),
            "137+ba/137"
        );
        assert_eq!(format_expression_for("137", Some("avc1"), None), "137+ba/137");
    }

    /// 已封装（自带音频）的行**不能**再追加 `+ba`——
    /// 早期版本对所有有视频编码的行都追加，`18` 会变成多此一举的 `18+ba/18`。
    #[test]
    fn muxed_format_is_used_as_is() {
        assert_eq!(format_expression_for("18", Some("avc1"), Some("mp4a")), "18");
        assert_eq!(format_expression_for("22", Some("avc1"), Some("mp4a")), "22");
    }

    #[test]
    fn audio_only_format_is_used_as_is() {
        assert_eq!(format_expression_for("140", Some("none"), Some("mp4a")), "140");
        assert_eq!(format_expression_for("251", None, Some("opus")), "251");
    }

    /// 用户显式选的表达式优先于预设。
    #[test]
    fn format_override_wins_over_preset() {
        let mut s = spec();
        s.preset = Preset::Best;
        s.format_override = Some("137+ba/137".into());
        let a = build_download_args(&s);
        let i = a.iter().position(|x| x == "--format").unwrap();
        assert_eq!(a[i + 1], "137+ba/137");

        assert_eq!(s.effective_format_expression(), "137+ba/137");
    }

    /// 没选、或选了空白 → 回落到预设，不能产出一个非法的 `-f ""`。
    #[test]
    fn blank_override_falls_back_to_preset() {
        let mut s = spec();
        s.preset = Preset::MaxHeight(1080);
        assert_eq!(s.effective_format_expression(), "bv*[height<=1080]+ba/b[height<=1080]/b");

        s.format_override = Some("   ".into());
        assert_eq!(s.format_override.as_deref(), Some("   "));
        assert_eq!(
            s.effective_format_expression(),
            "bv*[height<=1080]+ba/b[height<=1080]/b",
            "空白表达式必须回落到预设"
        );
    }
}
