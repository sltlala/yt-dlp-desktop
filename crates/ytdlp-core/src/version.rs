//! yt-dlp 版本号解析与比较。
//!
//! 实测 `yt-dlp --version` 输出为干净单行，形如 `2026.07.04`（HANDOFF §5）。
//! 这里按**点分数字段**逐段比较，因此对 `2026.07.04.123456` 这类
//! nightly 形态同样成立。

/// 把版本串拆成数字段。非数字段被忽略。
pub fn parse(s: &str) -> Vec<u64> {
    s.trim()
        .split(['.', '-', '_'])
        .filter_map(|seg| {
            let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                None
            } else {
                digits.parse::<u64>().ok()
            }
        })
        .collect()
}

/// `candidate` 是否比 `current` 新。
///
/// 任一侧无法解析为版本时返回 `false` —— **宁可漏更新，也不要误替换**：
/// 一个损坏或未识别的 exe 会让所有任务同时失败（DESIGN §8 约束 3）。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let a = parse(candidate);
    let b = parse(current);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    // 逐段比较，缺失段视为 0。
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 校验 `--version` 的输出是否像一版 yt-dlp。
///
/// 更新流程要求在替换前用它验证新下载的文件（DESIGN §8 约束 3）。
pub fn looks_like_version(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.lines().count() != 1 {
        return false;
    }
    let parts = parse(t);
    // 形如 2026.07.04 -> 3 段；至少要有 2 段才认为可信。
    parts.len() >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_date_version() {
        assert_eq!(parse("2026.07.04"), vec![2026, 7, 4]);
        assert_eq!(parse("  2026.07.04\n"), vec![2026, 7, 4]);
    }

    #[test]
    fn newer_by_date() {
        assert!(is_newer("2026.07.05", "2026.07.04"));
        assert!(is_newer("2026.08.01", "2026.07.31"));
        assert!(is_newer("2027.01.01", "2026.12.31"));
        assert!(!is_newer("2026.07.04", "2026.07.04"));
        assert!(!is_newer("2026.07.03", "2026.07.04"));
    }

    /// 月份/日期按数值而非字典序比较。
    #[test]
    fn numeric_not_lexicographic() {
        // 字典序下 "2026.7.10" > "2026.10.1"（因为 '7' > '1'），数值下正好相反。
        assert!(
            !is_newer("2026.7.10", "2026.10.1"),
            "7 月不应被判为比 10 月新（说明用了字典序）"
        );
        assert!(is_newer("2026.10.1", "2026.7.10"));
    }

    #[test]
    fn nightly_shape_is_comparable() {
        assert!(is_newer("2026.07.04.120000", "2026.07.04"));
        assert!(!is_newer("2026.07.04", "2026.07.04.120000"));
    }

    /// 不可解析时必须保守地返回 false，避免把坏文件换上去。
    #[test]
    fn unparseable_is_never_newer() {
        assert!(!is_newer("garbage", "2026.07.04"));
        assert!(!is_newer("2026.07.04", "garbage"));
        assert!(!is_newer("", ""));
        assert!(!is_newer("2026.07.04", ""));
    }

    #[test]
    fn validates_version_output() {
        assert!(looks_like_version("2026.07.04"));
        assert!(looks_like_version("2026.07.04\n"));
        // 多行说明拿到的不是版本号（可能是报错文本）。
        assert!(!looks_like_version("2026.07.04\nextra"));
        assert!(!looks_like_version(""));
        assert!(!looks_like_version("   "));
        assert!(!looks_like_version("not a version"));
        assert!(!looks_like_version("2026"));
    }
}
