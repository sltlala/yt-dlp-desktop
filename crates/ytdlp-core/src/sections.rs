//! 片段下载（`--download-sections`）的时间段解析。
//!
//! 用户在界面上手填时间段，格式要宽松（`1:30-2:45`、`90-165`、`10:00-` 表示到结尾），
//! 但交给 yt-dlp 之前必须校验并规范成 `*起-止`：不带 `*` 前缀的值会被 yt-dlp
//! 当成**章节名正则**去匹配，写错了也不报错，结果是「什么都没下」。

/// 解析 `秒`、`分:秒`、`时:分:秒`，秒可以带小数。
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut total = 0.0;
    for (i, p) in parts.iter().enumerate() {
        let last = i == parts.len() - 1;
        let v: f64 = if last {
            p.parse().ok().filter(|v: &f64| v.is_finite())?
        } else {
            f64::from(p.parse::<u32>().ok()?)
        };
        if v < 0.0 || (i > 0 && v >= 60.0) {
            return None;
        }
        total = total * 60.0 + v;
    }
    Some(total)
}

/// 秒数 → `H:MM:SS`（带小数时保留到毫秒）。
pub fn format_time(sec: f64) -> String {
    let whole = sec.trunc() as u64;
    let (h, m, s) = (whole / 3600, (whole % 3600) / 60, whole % 60);
    let frac = sec - sec.trunc();
    if frac > 0.0005 {
        format!("{h}:{m:02}:{s:02}.{:03}", (frac * 1000.0).round() as u32)
    } else {
        format!("{h}:{m:02}:{s:02}")
    }
}

/// 把用户输入解析成若干条 `--download-sections` 的值。
///
/// 多段用逗号、分号或换行分隔。空输入返回空列表（= 下载整个视频）。
/// `duration` 已知时会拒绝超出视频长度的起点。
pub fn parse_sections(input: &str, duration: Option<f64>) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for raw in input.split([',', '，', ';', '；', '\n']) {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        let Some((a, b)) = part.split_once(['-', '~', '～']) else {
            return Err(format!("「{part}」缺少结束时间，写成 开始-结束，例如 1:30-2:45"));
        };
        let start = if a.trim().is_empty() { Some(0.0) } else { parse_time(a) };
        let Some(start) = start else {
            return Err(format!("「{}」不是有效的时间", a.trim()));
        };
        let end = if b.trim().is_empty() {
            None
        } else {
            match parse_time(b) {
                Some(e) => Some(e),
                None => return Err(format!("「{}」不是有效的时间", b.trim())),
            }
        };
        if let Some(e) = end {
            if e <= start {
                return Err(format!("「{part}」结束时间要晚于开始时间"));
            }
        }
        if let Some(d) = duration {
            if start >= d {
                return Err(format!("「{part}」开始时间超出了视频长度（{}）", format_time(d)));
            }
        }
        let end = end.map_or_else(|| "inf".to_string(), format_time);
        out.push(format!("*{}-{end}", format_time(start)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_time_forms() {
        assert_eq!(parse_time("90"), Some(90.0));
        assert_eq!(parse_time("1:30"), Some(90.0));
        assert_eq!(parse_time("1:02:03"), Some(3723.0));
        assert_eq!(parse_time("0:05.5"), Some(5.5));
        assert_eq!(parse_time("1:60"), None);
        assert_eq!(parse_time("a:10"), None);
        assert_eq!(parse_time("1:2:3:4"), None);
        assert_eq!(parse_time(""), None);
    }

    #[test]
    fn normalizes_to_ytdlp_syntax() {
        assert_eq!(parse_sections("1:30-2:45", None).unwrap(), ["*0:01:30-0:02:45"]);
        assert_eq!(parse_sections("90-165", None).unwrap(), ["*0:01:30-0:02:45"]);
        assert_eq!(parse_sections("1:00:00-", None).unwrap(), ["*1:00:00-inf"]);
        assert_eq!(parse_sections("-30", None).unwrap(), ["*0:00:00-0:00:30"]);
        assert_eq!(parse_sections("0:05.5-0:10", None).unwrap(), ["*0:00:05.500-0:00:10"]);
    }

    #[test]
    fn splits_multiple_ranges() {
        assert_eq!(
            parse_sections("0:10-0:20，1:00~1:30\n2:00-", None).unwrap(),
            ["*0:00:10-0:00:20", "*0:01:00-0:01:30", "*0:02:00-inf"]
        );
    }

    #[test]
    fn empty_input_means_whole_video() {
        assert!(parse_sections("  ", None).unwrap().is_empty());
    }

    #[test]
    fn rejects_invalid_ranges_with_readable_reasons() {
        assert!(parse_sections("1:30", None).unwrap_err().contains("缺少结束时间"));
        assert!(parse_sections("2:00-1:00", None).unwrap_err().contains("晚于开始"));
        assert!(parse_sections("abc-1:00", None).unwrap_err().contains("不是有效的时间"));
        assert!(parse_sections("10:00-11:00", Some(300.0)).unwrap_err().contains("超出"));
    }
}
