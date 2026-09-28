//! `--dump-single-json` 探测结果的解析。
//!
//! yt-dlp 的 info JSON **字段随提取器差异极大**（有的没有 `duration`，
//! 有的用 `thumbnail` 有的只有 `thumbnails` 数组，`formats` 里大量字段为 null）。
//! 因此这里用 `serde_json::Value` 手工取值，而不是严格 `Deserialize`——
//! 后者会因某个站点缺一个字段就整体失败。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 一个可选格式。UI 的高级模式用它，但**选中的结果必须落回 `-f` 表达式**（DESIGN §3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatInfo {
    pub format_id: String,
    pub ext: String,
    pub resolution: String,
    pub fps: Option<f64>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    /// `filesize` 优先，缺失时回落 `filesize_approx`。
    pub filesize: Option<u64>,
    pub tbr: Option<f64>,
    pub note: String,
}

impl FormatInfo {
    /// 是否仅音频（`vcodec == "none"` 或无视频编码）。
    pub fn is_audio_only(&self) -> bool {
        !has_codec(self.vcodec.as_deref())
    }

    /// 这条格式在「音视频是否分开」上的归属。
    pub fn kind(&self) -> FormatKind {
        format_kind(self.vcodec.as_deref(), self.acodec.as_deref())
    }
}

/// yt-dlp 用字符串 `"none"` 表示「没有这一路」，`null` 也表示没有——两种都要认。
pub fn has_codec(c: Option<&str>) -> bool {
    c.map(|v| !v.is_empty() && v != "none").unwrap_or(false)
}

/// 一条格式在「音视频是否分开」这个维度上的归属。
///
/// DASH 站点（YouTube 就是）会把视频轨和音频轨**分开**列出来，
/// 单独选一条视频轨是**没有声音**的——用户看到的就是这个问题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    /// 视频+音频已封装在一起，选了就能直接用
    Muxed,
    /// 仅视频轨，必须再配一条音频轨
    VideoOnly,
    /// 仅音频轨
    AudioOnly,
}

/// 判断一条格式属于哪一类。
///
/// 四象限里 `(false, false)` 是 **generic 提取器给直链文件**的情形：
/// 它不探测流内容，所以两路编码都报 none。那是一个**完整可用的文件**，
/// 应当按「已封装」处理（表达式就是它自己），而不是当成需要配音的纯视频轨。
pub fn format_kind(vcodec: Option<&str>, acodec: Option<&str>) -> FormatKind {
    match (has_codec(vcodec), has_codec(acodec)) {
        (true, true) => FormatKind::Muxed,
        (true, false) => FormatKind::VideoOnly,
        (false, true) => FormatKind::AudioOnly,
        // 编码未知的直接文件：当已封装，选了就用它本身
        (false, false) => FormatKind::Muxed,
    }
}

/// 播放列表中的一个条目（`--flat-playlist` 下字段很有限）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistEntry {
    pub id: String,
    pub title: String,
    pub url: Option<String>,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
}

/// 探测出的媒体信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub id: String,
    pub title: String,
    pub extractor: String,
    pub thumbnail: Option<String>,
    /// 秒。直播等场景可能缺失。
    pub duration: Option<f64>,
    pub webpage_url: Option<String>,
    pub is_playlist: bool,
    /// `is_playlist` 为真时的扁平条目列表（供用户勾选，DESIGN §9）。
    pub entries: Vec<PlaylistEntry>,
    pub formats: Vec<FormatInfo>,
    /// 可用字幕语言，供设置页提示「该视频有哪些字幕」。
    pub subtitle_langs: Vec<String>,
}

fn as_str(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn as_f64(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(|x| x.as_f64())
}

fn as_u64(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(|x| x.as_u64())
}

/// 挑选缩略图。
///
/// yt-dlp 有时给 `thumbnail`（单张），有时只给 `thumbnails`（数组，按从小到大排列）。
/// 数组里取**最后一张有 url 的**——通常分辨率最高。
fn pick_thumbnail(v: &Value) -> Option<String> {
    if let Some(t) = as_str(v, "thumbnail") {
        return Some(t);
    }
    let arr = v.get("thumbnails")?.as_array()?;
    arr.iter()
        .rev()
        .find_map(|t| as_str(t, "url"))
        // 数组里也可能带 `data:` 内联缩略图
        .or_else(|| arr.iter().rev().find_map(|t| as_str(t, "data")))
}

fn parse_formats(v: &Value) -> Vec<FormatInfo> {
    v.get("formats")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let format_id = as_str(f, "format_id")?;
                    let vcodec = as_str(f, "vcodec");
                    let acodec = as_str(f, "acodec");
                    let ext = as_str(f, "ext").unwrap_or_default();
                    let note = as_str(f, "format_note").unwrap_or_default();

                    // storyboard / 预览图不是可下载的媒体，留在表里只会让用户困惑——
                    // 它们 `vcodec`/`acodec` 都是 none，实测会被当成「仅音频」。
                    //
                    // ⚠️ 判据必须用 `ext == mhtml`，**不能**用「两路编码都缺」：
                    // generic 提取器给直链文件时同样是 `vcodec: "none", acodec: null`，
                    // 那是**一个完整可用的文件**，滤掉它就直接下不了了（实测踩过）。
                    if ext.eq_ignore_ascii_case("mhtml")
                        || note.to_ascii_lowercase().contains("storyboard")
                    {
                        return None;
                    }

                    Some(FormatInfo {
                        format_id,
                        ext,
                        resolution: as_str(f, "resolution")
                            .or_else(|| {
                                // 有的提取器只给 width/height
                                match (as_u64(f, "width"), as_u64(f, "height")) {
                                    (Some(w), Some(h)) => Some(format!("{w}x{h}")),
                                    (None, Some(h)) => Some(format!("?x{h}")),
                                    _ => None,
                                }
                            })
                            .unwrap_or_else(|| "audio only".into()),
                        fps: as_f64(f, "fps"),
                        vcodec,
                        acodec,
                        filesize: as_u64(f, "filesize").or_else(|| as_u64(f, "filesize_approx")),
                        tbr: as_f64(f, "tbr"),
                        note,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_entries(v: &Value) -> Vec<PlaylistEntry> {
    v.get("entries")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|e| !e.is_null())
                .enumerate()
                .map(|(i, e)| PlaylistEntry {
                    id: as_str(e, "id").unwrap_or_else(|| format!("#{}", i + 1)),
                    title: as_str(e, "title").unwrap_or_else(|| "(无标题)".into()),
                    url: as_str(e, "url").or_else(|| as_str(e, "webpage_url")),
                    duration: as_f64(e, "duration"),
                    thumbnail: pick_thumbnail(e),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_subtitle_langs(v: &Value) -> Vec<String> {
    let mut langs: Vec<String> = v
        .get("subtitles")
        .and_then(|x| x.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    // 自动生成字幕单独一个字段，UI 需要知道（--write-auto-subs）
    if let Some(m) = v.get("automatic_captions").and_then(|x| x.as_object()) {
        for k in m.keys() {
            let tagged = format!("{k} (自动)");
            if !langs.contains(&tagged) {
                langs.push(tagged);
            }
        }
    }
    langs.sort();
    langs
}

/// 解析 `--dump-single-json` 的输出。
pub fn parse_info_json(json: &str) -> Result<MediaInfo, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("JSON 解析失败：{e}"))?;
    if !v.is_object() {
        return Err("探测结果不是对象".into());
    }

    // `_type` 为 `playlist` 或 `multi_video` 时按播放列表处理
    let type_tag = as_str(&v, "_type").unwrap_or_default();
    let entries = parse_entries(&v);
    let is_playlist = matches!(type_tag.as_str(), "playlist" | "multi_video")
        || v.get("entries").and_then(|x| x.as_array()).is_some_and(|a| !a.is_empty());

    Ok(MediaInfo {
        id: as_str(&v, "id").unwrap_or_default(),
        title: as_str(&v, "title")
            .or_else(|| as_str(&v, "fulltitle"))
            .unwrap_or_else(|| "(无标题)".into()),
        extractor: as_str(&v, "extractor_key")
            .or_else(|| as_str(&v, "extractor"))
            .unwrap_or_default(),
        thumbnail: pick_thumbnail(&v),
        duration: as_f64(&v, "duration"),
        webpage_url: as_str(&v, "webpage_url"),
        is_playlist,
        entries,
        formats: parse_formats(&v),
        subtitle_langs: parse_subtitle_langs(&v),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取自本机实测的 generic 提取器输出（真实结构，已裁剪无关字段）。
    const REAL_VIDEO: &str = r#"{
      "_type": "video",
      "id": "sample",
      "title": "sample",
      "fulltitle": "sample",
      "ext": "mp4",
      "extractor": "generic",
      "extractor_key": "Generic",
      "webpage_url": "http://127.0.0.1:8794/sample.mp4",
      "formats": [
        {
          "format_id": "mp4",
          "ext": "mp4",
          "resolution": null,
          "vcodec": "none",
          "acodec": null,
          "filesize_approx": 3145728,
          "tbr": null,
          "protocol": "http",
          "format": "mp4 - unknown"
        }
      ]
    }"#;

    #[test]
    fn parses_real_video_json() {
        let m = parse_info_json(REAL_VIDEO).unwrap();
        assert_eq!(m.id, "sample");
        assert_eq!(m.extractor, "Generic");
        assert!(!m.is_playlist);
        assert_eq!(m.formats.len(), 1);
        assert_eq!(m.formats[0].format_id, "mp4");
        // filesize 缺失时回落 filesize_approx
        assert_eq!(m.formats[0].filesize, Some(3_145_728));
        // resolution 为 null 且无 width/height → 回落 "audio only"
        assert_eq!(m.formats[0].resolution, "audio only");
        // ⚠️ generic 直链文件两路编码都是 none，**不能**因此被当成 storyboard 滤掉，
        // 也不能被当成「仅视频」而配上一条不存在的音轨。
        assert_eq!(m.formats[0].kind(), FormatKind::Muxed);
    }

    /// storyboard（`ext: mhtml`）不是可下载的媒体，必须滤掉。
    /// 它们的 vcodec/acodec 都是 none，留着会被误标成「仅音频」。
    #[test]
    fn storyboards_are_filtered_out() {
        const WITH_SB: &str = r#"{
          "_type": "video", "id": "x", "title": "x", "extractor_key": "Youtube",
          "webpage_url": "https://example.com/x",
          "formats": [
            {"format_id": "137", "ext": "mp4", "width": 1920, "height": 1080,
             "vcodec": "avc1.640028", "acodec": "none", "tbr": 4212},
            {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2", "tbr": 129},
            {"format_id": "sb0", "ext": "mhtml", "width": 320, "height": 180,
             "vcodec": "none", "acodec": "none", "format_note": "storyboard"},
            {"format_id": "sb1", "ext": "mhtml", "width": 160, "height": 90,
             "vcodec": "none", "acodec": "none"}
          ]
        }"#;
        let m = parse_info_json(WITH_SB).unwrap();
        assert_eq!(m.formats.len(), 2, "两条 storyboard 都该被滤掉");
        assert_eq!(m.formats[0].kind(), FormatKind::VideoOnly);
        assert_eq!(m.formats[1].kind(), FormatKind::AudioOnly);
    }

    /// yt-dlp 用 `"none"` 表示「没有这一路」，`null` 也表示没有——两种都要认。
    #[test]
    fn format_kind_covers_both_absent_encodings() {
        assert_eq!(format_kind(Some("avc1"), Some("mp4a")), FormatKind::Muxed);
        assert_eq!(format_kind(Some("avc1"), Some("none")), FormatKind::VideoOnly);
        assert_eq!(format_kind(Some("none"), Some("mp4a")), FormatKind::AudioOnly);
        assert_eq!(format_kind(Some("avc1"), None), FormatKind::VideoOnly);
        assert_eq!(format_kind(None, Some("mp4a")), FormatKind::AudioOnly);
        assert_eq!(format_kind(Some(""), Some("mp4a")), FormatKind::AudioOnly);
        // 两路都缺 = 编码未知的直接文件，当已封装（见 format_kind 的说明）
        assert_eq!(format_kind(None, None), FormatKind::Muxed);
    }

    /// 真实站点常见的完整形态。
    const YOUTUBE_LIKE: &str = r#"{
      "_type": "video",
      "id": "dQw4w9WgXcQ",
      "title": "Rick Astley - Never Gonna Give You Up",
      "extractor_key": "Youtube",
      "webpage_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
      "duration": 213.0,
      "thumbnails": [
        {"url": "https://i.ytimg.com/vi/x/default.jpg", "width": 120},
        {"url": "https://i.ytimg.com/vi/x/maxresdefault.jpg", "width": 1280}
      ],
      "formats": [
        {"format_id": "137", "ext": "mp4", "width": 1920, "height": 1080, "fps": 30,
         "vcodec": "avc1.640028", "acodec": "none", "filesize": 402653184, "tbr": 4212,
         "format_note": "1080p"},
        {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2",
         "filesize": 8589934, "tbr": 129, "format_note": "medium"}
      ],
      "subtitles": {"en": [{"ext": "vtt"}], "zh-Hans": [{"ext": "vtt"}]},
      "automatic_captions": {"ja": [{"ext": "vtt"}]}
    }"#;

    #[test]
    fn parses_youtube_like_json() {
        let m = parse_info_json(YOUTUBE_LIKE).unwrap();
        assert_eq!(m.title, "Rick Astley - Never Gonna Give You Up");
        assert_eq!(m.extractor, "Youtube");
        assert_eq!(m.duration, Some(213.0));
        // thumbnails 数组取最后一张（通常最大）
        assert_eq!(
            m.thumbnail.as_deref(),
            Some("https://i.ytimg.com/vi/x/maxresdefault.jpg")
        );
        assert_eq!(m.formats.len(), 2);
        // width/height 合成 resolution
        assert_eq!(m.formats[0].resolution, "1920x1080");
        assert!(!m.formats[0].is_audio_only());
        assert!(m.formats[1].is_audio_only());
        // 字幕语言合并了自动生成项
        assert!(m.subtitle_langs.contains(&"en".to_string()));
        assert!(m.subtitle_langs.contains(&"zh-Hans".to_string()));
        assert!(m.subtitle_langs.contains(&"ja (自动)".to_string()));
    }

    /// `--flat-playlist` 的条目字段很有限，也必须能解析。
    const FLAT_PLAYLIST: &str = r#"{
      "_type": "playlist",
      "id": "PL123",
      "title": "我的收藏",
      "extractor_key": "YoutubeTab",
      "entries": [
        {"_type": "url", "id": "aaa", "title": "第一集", "url": "https://x/1", "duration": 120.0},
        {"_type": "url", "id": "bbb", "title": "第二集", "url": "https://x/2"},
        null
      ]
    }"#;

    #[test]
    fn parses_flat_playlist() {
        let m = parse_info_json(FLAT_PLAYLIST).unwrap();
        assert!(m.is_playlist);
        // null 条目被跳过
        assert_eq!(m.entries.len(), 2);
        assert_eq!(m.entries[0].title, "第一集");
        assert_eq!(m.entries[0].duration, Some(120.0));
        assert_eq!(m.entries[1].url.as_deref(), Some("https://x/2"));
    }

    /// 字段大面积缺失时不能整体失败（不同提取器差异极大）。
    #[test]
    fn tolerates_minimal_json() {
        let m = parse_info_json(r#"{"id":"x"}"#).unwrap();
        assert_eq!(m.id, "x");
        assert_eq!(m.title, "(无标题)");
        assert!(m.formats.is_empty());
        assert!(m.entries.is_empty());
        assert_eq!(m.duration, None);
        assert_eq!(m.thumbnail, None);
    }

    #[test]
    fn rejects_non_object_and_bad_json() {
        assert!(parse_info_json("[]").is_err());
        assert!(parse_info_json("not json").is_err());
        assert!(parse_info_json("").is_err());
    }

    /// 内联 data: 缩略图也要能取到。
    #[test]
    fn picks_inline_data_thumbnail() {
        let j = r#"{"id":"x","thumbnails":[{"data":"data:image/png;base64,AAA"}]}"#;
        let m = parse_info_json(j).unwrap();
        assert_eq!(m.thumbnail.as_deref(), Some("data:image/png;base64,AAA"));
    }

    /// `thumbnail` 单字段优先于 `thumbnails` 数组。
    #[test]
    fn single_thumbnail_wins() {
        let j = r#"{"id":"x","thumbnail":"https://a/b.jpg",
                    "thumbnails":[{"url":"https://a/small.jpg"}]}"#;
        let m = parse_info_json(j).unwrap();
        assert_eq!(m.thumbnail.as_deref(), Some("https://a/b.jpg"));
    }

    /// 条目含 entries 数组但 _type 不是 playlist，也应判定为播放列表。
    #[test]
    fn detects_playlist_without_type_tag() {
        let j = r#"{"id":"x","entries":[{"id":"a","title":"A"}]}"#;
        assert!(parse_info_json(j).unwrap().is_playlist);
    }

    #[test]
    fn empty_entries_is_not_playlist() {
        let j = r#"{"id":"x","_type":"video","entries":[]}"#;
        assert!(!parse_info_json(j).unwrap().is_playlist);
    }
}
