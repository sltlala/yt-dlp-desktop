//! yt-dlp 报错的归类：给用户看的一句话、要不要自动重试、要不要开指纹模拟。

use super::truncate;

/// 这段输出是不是 Cloudflare 反爬拦截。
///
/// 认的是 yt-dlp 自己那句话（`generic.py` 里写死的），
/// 分类后的中文提示里也原样带着它，所以两种来源都能认出。
pub(crate) fn is_cloudflare_challenge(text: &str) -> bool {
    text.contains("Cloudflare anti-bot challenge")
}

/// 是不是「generic 提取器被 HTTP 403 挡了」。
///
/// yt-dlp 只在**认出**那是 Cloudflare 挑战时才给那句建议——它要求响应头有
/// `cf-mitigated: challenge`，或者页面标题正好是 `Attention Required! | Cloudflare`。
/// 很多站点用的是自家规则（或者 CDN 没带那个头），同样是 403 却只报一句
/// `Unable to download webpage: HTTP Error 403`。
///
/// 这两种对**指纹模拟**的反应是一样的，所以自动重试的条件把两种都收进来。
/// 实测用户遇到的正是后一种：报错里根本没有 Cloudflare 字样。
pub(crate) fn is_blocked_generic_403(text: &str) -> bool {
    text.contains("[generic]") && text.contains("HTTP Error 403")
}

/// 值不值得为它开指纹模拟再试一次。
pub(crate) fn worth_impersonating(text: &str) -> bool {
    is_cloudflare_challenge(text) || is_blocked_generic_403(text)
}

/// 这次下载失败要不要**自动**带着指纹模拟重试一次。
///
/// 三个条件缺一不可：
/// - 确实失败了（不是 canceled，也不是正常跳过）；
/// - **还没试过**自动模拟——`already` 保证只重试一次，否则会死循环；
/// - 失败原因是站点把我们挡了（Cloudflare 挑战，或没带标记的普通 403）。
///
/// 抽成纯函数是因为这个判断错了的代价很大：漏判 = 用户继续看到那个
/// 看不懂的 403；误判 = 每个失败都白跑一遍。
pub(crate) fn should_retry_with_impersonate(state: &str, already: bool, error: Option<&str>) -> bool {
    state == "failed"
        && !already
        && error.map(worth_impersonating).unwrap_or(false)
}

// ─────────── 自动降级重试（ROADMAP §F2）───────────
//
// 与上面的 `should_retry_with_impersonate` 是**两套**逻辑：
// - 上面那个针对「站点 403 反爬」→ 开指纹模拟重试（已有，保留）；
// - 下面这套针对「网络抖动 / aria2c 特有失败」→ 换更稳的配置重试。
// 两者都靠 `retry_flags` 位掩码保证同一种动作只试一次，不会死循环。

/// `retry_flags` 的位定义（见 `Task::retry_flags`）。
pub(crate) const RETRY_SAME: u8 = 1;
pub(crate) const RETRY_NO_ARIA2C: u8 = 2;
pub(crate) const RETRY_NO_IMPERSONATE: u8 = 4;

/// 自动重试要采取的降级动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryAction {
    /// 不重试。
    None,
    /// 原样重试一次（针对偶发网络抖动）。
    RetrySame,
    /// 关掉 aria2c、退回原生下载器重试（针对 aria2c 特有失败，DESIGN §11.4）。
    RetryNoAria2c,
    /// 关掉指纹模拟重试（针对「模拟反而失败」）。
    RetryNoImpersonate,
}

/// 错误文本是不是 aria2c 特有失败。
///
/// 实测的 B站弹幕事故文本：`libz::inflate() failed ... incorrect header check`、
/// `ERROR: aria2c exited with code 1`（DESIGN §11.8）。这些用 aria2c 才炸，
/// 退回原生下载器就没事。
pub(crate) fn is_aria2c_error(text: &str) -> bool {
    text.contains("aria2c")
        || text.contains("inflate")
        || text.contains("incorrect header check")
        || text.contains("GZipDecodingStreamFilter")
}

/// 错误文本是不是「确定性失败」——重试也没用，不该浪费一次重试。
///
/// 包括：格式不可用、不支持的链接、需要登录/会员、站点 4xx 拒绝、
/// 副本损坏。这些和「网络抖一下」是两回事。
pub(crate) fn is_deterministic_error(text: &str) -> bool {
    text.contains("Requested format is not available")
        || text.contains("Unsupported URL")
        || text.contains("Sign in to confirm")
        || text.contains("Private video")
        || text.contains("members-only")
        || text.contains("This video is available to this channel's members")
        || text.contains("Failed to load Python DLL")
        || text.contains("[PYI-")
        || text.contains("HTTP Error 4") // 403/404 等：反爬或已删除，重试无意义
}

/// 错误文本是不是「网络抖动类」——值得原样重试一次。
pub(crate) fn is_transient_network_error(text: &str) -> bool {
    text.contains("Unable to download webpage")
        || text.contains("Failed to resolve")
        || text.contains("timed out")
        || text.contains("Connection")
        || text.contains("reset by peer")
        || text.contains("EOF")
        || text.contains("network is unreachable")
        || text.contains("HTTP Error 5") // 5xx 服务端临时故障
}

/// 决定这次失败要不要自动重试、以及用什么降级动作。
///
/// 纯函数，方便单测。规则（按优先级）：
/// 1. 非 failed 或 error 为空 → 不重试（没信息，瞎重试没意义）；
/// 2. 确定性错误 → 不重试；
/// 3. aria2c 特有失败 且 这次用了 aria2c 且 还没试过关掉它 → 关 aria2c；
/// 4. 网络抖动 且 还没试过原样重试 → 原样重试；
/// 5. 开着指纹模拟 且 不是 4xx 且 还没试过关掉它 → 关指纹模拟；
/// 6. 其余 → 不重试。
pub(crate) fn should_auto_retry(
    state: &str,
    retry_flags: u8,
    error: Option<&str>,
    used_aria2c: bool,
    impersonate: bool,
) -> RetryAction {
    if state != "failed" {
        return RetryAction::None;
    }
    let Some(text) = error else {
        return RetryAction::None;
    };
    if is_deterministic_error(text) {
        return RetryAction::None;
    }
    if is_aria2c_error(text) && used_aria2c && retry_flags & RETRY_NO_ARIA2C == 0 {
        return RetryAction::RetryNoAria2c;
    }
    if is_transient_network_error(text) && retry_flags & RETRY_SAME == 0 {
        return RetryAction::RetrySame;
    }
    if impersonate && !text.contains("HTTP Error 4") && retry_flags & RETRY_NO_IMPERSONATE == 0 {
        return RetryAction::RetryNoImpersonate;
    }
    RetryAction::None
}

/// 把 yt-dlp 的原始报错归类成用户看得懂的原因（DESIGN §6、§14）。
///
/// 返回固定两行：**第一行**是一句话的原因加怎么办（列表行只显示这一行，
/// 必须短到一行放得下）；**第二行**是原始报错，只在任务详情里以小字展示。
///
/// 原始报错不能丢：自动重试（`should_auto_retry`）和 Cloudflare 识别都是
/// 靠匹配其中的英文原文来判断的。
pub(crate) fn classify_error(stderr: &str, code: Option<i32>) -> String {
    let pick = |needle: &str| stderr.lines().find(|l| l.contains(needle));
    let with_raw = |summary: &str, line: &str| {
        let raw = line.trim();
        let raw = raw.strip_prefix("ERROR:").unwrap_or(raw).trim();
        format!("{summary}\n{raw}")
    };

    // PyInstaller 引导器报错：最常见的成因是 onedir 构建被单独复制，丢了 `_internal\`。
    if let Some(l) = pick("Failed to load Python DLL").or_else(|| pick("[PYI-")) {
        return with_raw("yt-dlp 文件不完整：请在设置里重新更新", l);
    }
    if let Some(l) = pick("Sign in to confirm") {
        return with_raw("需要登录：请在「设置 → 账号」配置 Cookie", l);
    }
    if let Some(l) = pick("Private video")
        .or_else(|| pick("members-only"))
        .or_else(|| pick("This video is available to this channel's members"))
    {
        return with_raw("私有或会员视频：需配置有权限账号的 Cookie", l);
    }
    // 站点拒绝了我们。**必须排在「网络不可达」之前**：那句
    // `Unable to download webpage` 里经常裹着 HTTP 状态码，
    // 而「站点答了、只是不肯给」和「根本连不上」是两件完全不同的事。
    if let Some(l) = pick("Cloudflare anti-bot challenge") {
        return with_raw("被站点反爬拦截：换个代理或配置 Cookie", l);
    }
    if let Some(l) = pick("Unable to download webpage").or_else(|| pick("Failed to resolve")) {
        // ⚠️ 实测踩到：`Unable to download webpage: HTTP Error 403` 被判成「网络不可达」——
        // 站点明明答了，只是拒绝了我们，这句提示会把人引到完全相反的方向去查代理。
        if let Some(status) = http_status(l).filter(|s| *s >= 400) {
            return with_raw(
                &format!("站点拒绝访问（HTTP {status}）：换个代理再试"),
                l,
            );
        }
        return with_raw("网络连接失败：请检查网络或代理", l);
    }
    if let Some(l) = pick("needs to be reloaded") {
        return with_raw("YouTube 暂时限流：稍等几分钟再试", l);
    }
    // 如果链接来自 YouTube，这通常不是格式设置的问题——拿不到真实流时
    // （只返回 storyboard 预览图）也会报这个错。
    if let Some(l) = pick("Requested format is not available") {
        return with_raw("所选格式不可用：试试「最佳画质」", l);
    }
    if let Some(l) = pick("Unsupported URL") {
        return with_raw("不支持这个链接", l);
    }
    if let Some(l) = pick("No space left on device").or_else(|| pick("Errno 28")) {
        return with_raw("磁盘空间不足：请清理或换个输出目录", l);
    }
    if let Some(l) = pick("Permission denied").or_else(|| pick("Errno 13")) {
        return with_raw("没有写入权限：换个目录或关闭占用的程序", l);
    }
    // 认不出的报错：第一行放截短的原文，完整的放第二行
    let raw = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("ERROR:"))
        .or_else(|| stderr.lines().find(|l| !l.trim().is_empty()));
    match raw {
        Some(l) => {
            let text = l.trim();
            let text = text.strip_prefix("ERROR:").unwrap_or(text).trim();
            with_raw(&format!("下载失败：{}", truncate(text, 30)), l)
        }
        None => format!("下载失败（yt-dlp 退出码 {}）", code.unwrap_or(-1)),
    }
}

/// 从 `HTTP Error 403: Forbidden` 这类文本里取出状态码。
pub(crate) fn http_status(line: &str) -> Option<u16> {
    let rest = &line[line.find("HTTP Error ")? + "HTTP Error ".len()..];
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// 报错的第一行（一句话说明）。列表行与警告里只用这一行。
pub(crate) fn error_summary(err: &str) -> &str {
    err.lines().next().unwrap_or(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─────────── 自动降级重试（ROADMAP §F2）───────────

    #[test]
    fn auto_retry_ignores_deterministic_errors() {
        // 格式不可用 / 不支持 / 需要登录：重试无意义
        assert_eq!(
            should_auto_retry("failed", 0, Some("Requested format is not available"), false, false),
            RetryAction::None
        );
        assert_eq!(
            should_auto_retry("failed", 0, Some("Unsupported URL"), false, false),
            RetryAction::None
        );
        assert_eq!(
            should_auto_retry("failed", 0, Some("Sign in to confirm you're not a bot"), false, false),
            RetryAction::None
        );
        assert_eq!(
            should_auto_retry("failed", 0, Some("HTTP Error 403: Forbidden"), false, false),
            RetryAction::None
        );
    }

    #[test]
    fn auto_retry_downgrades_aria2c() {
        // aria2c 特有失败 + 这次用了 aria2c → 关 aria2c 重试
        assert_eq!(
            should_auto_retry(
                "failed",
                0,
                Some("GZipDecodingStreamFilter.cc:108] libz::inflate() failed"),
                true,
                false
            ),
            RetryAction::RetryNoAria2c
        );
        // 已试过关 aria2c → 不再重复
        assert_eq!(
            should_auto_retry("failed", RETRY_NO_ARIA2C, Some("aria2c exited with code 1"), true, false),
            RetryAction::None
        );
        // 没用 aria2c 却报 aria2c 错 → 不归因于 aria2c（退回也没用）
        assert_eq!(
            should_auto_retry("failed", 0, Some("aria2c exited with code 1"), false, false),
            RetryAction::None
        );
    }

    #[test]
    fn auto_retry_retries_transient_network() {
        assert_eq!(
            should_auto_retry("failed", 0, Some("Unable to download webpage: timed out"), false, false),
            RetryAction::RetrySame
        );
        assert_eq!(
            should_auto_retry("failed", 0, Some("Failed to resolve host"), false, false),
            RetryAction::RetrySame
        );
        assert_eq!(
            should_auto_retry("failed", 0, Some("HTTP Error 502"), false, false),
            RetryAction::RetrySame
        );
        // 已原样重试过 → 不再重复
        assert_eq!(
            should_auto_retry("failed", RETRY_SAME, Some("Unable to download webpage"), false, false),
            RetryAction::None
        );
    }

    #[test]
    fn auto_retry_skips_non_failed_and_empty() {
        assert_eq!(should_auto_retry("completed", 0, Some("whatever"), false, false), RetryAction::None);
        assert_eq!(should_auto_retry("failed", 0, None, false, false), RetryAction::None);
        // 没错误信息时不瞎重试
        assert_eq!(should_auto_retry("failed", 0, None, true, false), RetryAction::None);
    }

    #[test]
    fn auto_retry_turns_off_impersonate() {
        // 开着模拟、失败原因既不是网络抖动也不是 4xx → 关模拟重试
        // （注意：不能用 "connection reset" 之类文本——那会先命中网络抖动分支）
        assert_eq!(
            should_auto_retry("failed", 0, Some("Unknown extractor failure"), false, true),
            RetryAction::RetryNoImpersonate
        );
        // 4xx 时关模拟也没用（是站点拒绝，不是模拟导致）
        assert_eq!(
            should_auto_retry("failed", 0, Some("HTTP Error 429"), false, true),
            RetryAction::None
        );
        // 已试过关模拟 → 不再重复
        assert_eq!(
            should_auto_retry("failed", RETRY_NO_IMPERSONATE, Some("Unknown extractor failure"), false, true),
            RetryAction::None
        );
    }

    /// 认 Cloudflare 拦截靠的是 yt-dlp 自己那句话——**分类前后的文本都要认**：
    /// 探测走 `classify_error`（中文多行），下载走到的是原始 ERROR 行。
    #[test]
    fn cloudflare_challenge_is_recognized_in_both_forms() {        // 原始形式
        assert!(is_cloudflare_challenge(
            "ERROR: [generic] Got HTTP Error 403 caused by Cloudflare anti-bot challenge; \
             try again with --extractor-args \"generic:impersonate\""
        ));
        // 分类后的中文提示里原样带着那句话
        let classified = classify_error(
            "ERROR: [generic] Got HTTP Error 403 caused by Cloudflare anti-bot challenge",
            Some(1),
        );
        assert!(error_summary(&classified).contains("反爬拦截"), "实际: {classified}");
        assert!(is_cloudflare_challenge(&classified));
        // 别的 403 不能误判
        assert!(!is_cloudflare_challenge("ERROR: HTTP Error 403: Forbidden"));
        assert!(!is_cloudflare_challenge("ERROR: Unable to download webpage"));
    }

    /// ⚠️ 实测踩到的误判：`Unable to download webpage` 里裹着 HTTP 状态码时，
    /// 那**不是**网络不通——站点答了，只是拒绝了我们。说成「网络不可达（检查代理设置）」
    /// 会把人引到完全相反的方向去查代理。
    #[test]
    fn http_error_is_not_reported_as_unreachable() {
        let real = "ERROR: [generic] watch?v=407946: Unable to download webpage: \
                    HTTP Error 403: Forbidden";
        let msg = classify_error(real, Some(1));
        assert!(!msg.contains("网络连接失败"), "403 不该说成网络不通：{msg}");
        assert_eq!(
            error_summary(&msg),
            "站点拒绝访问（HTTP 403）：换个代理再试"
        );

        // 真的连不上时仍然要指向网络/代理
        let dead = "ERROR: Unable to download webpage: <urlopen error [Errno 111] Connection refused>";
        let msg = classify_error(dead, Some(1));
        assert!(error_summary(&msg).contains("网络连接失败"), "实际: {msg}");
    }

    /// 第一行要短到列表里一行放得下；原文必须保留在第二行，
    /// 自动重试与 Cloudflare 识别都靠匹配原文。
    #[test]
    fn classified_error_is_one_short_line_plus_raw() {
        let cases = [
            "ERROR: [youtube] abc: Sign in to confirm you're not a bot",
            "ERROR: [youtube] abc: Requested format is not available. Use --list-formats",
            "ERROR: Unsupported URL: https://example.com/x",
            "ERROR: [generic] x: Unable to download webpage: HTTP Error 451: Unavailable",
            "ERROR: [download] Got error: [Errno 28] No space left on device",
            "ERROR: some brand new failure nobody has classified yet, with a very long \
             explanation that keeps going and going",
            // 下载阶段解析器已去掉 `ERROR:` 前缀
            "[youtube] abc: Sign in to confirm you're not a bot",
        ];
        for raw in cases {
            let msg = classify_error(raw, Some(1));
            let mut lines = msg.lines();
            let summary = lines.next().unwrap();
            // 按显示宽度算：中文占两格，ASCII 占一格。约 28 个汉字是窄窗口里一行放得下的上限。
            let width: usize = summary.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum();
            assert!(width <= 56, "第一行太长（宽 {width}）：{summary}");
            let rest = lines.next().expect("应保留原始报错");
            assert!(!rest.starts_with("ERROR:"), "原文里的 ERROR: 前缀没必要展示：{rest}");
            assert!(raw.contains(rest), "原文应原样保留：{rest}");
        }
        assert!(should_auto_retry_is_deterministic(&classify_error(cases[0], Some(1))));
    }

    fn should_auto_retry_is_deterministic(text: &str) -> bool {
        should_auto_retry("failed", 0, Some(text), false, false) == RetryAction::None
            && is_deterministic_error(text)
    }

    /// Cloudflare 那句话里也含 `Unable to download webpage` 之外的形式，
    /// 但**必须优先于**网络那一支——否则会被抢答成「网络不可达」。
    #[test]
    fn cloudflare_takes_priority_over_unreachable() {
        let both = "ERROR: Unable to download webpage: HTTP Error 403: Forbidden\n\
                    ERROR: [generic] Got HTTP Error 403 caused by Cloudflare anti-bot challenge";
        let msg = classify_error(both, Some(1));
        assert!(error_summary(&msg).contains("反爬拦截"), "实际: {msg}");
        assert!(!msg.contains("网络连接失败"));
    }

    /// 没带 Cloudflare 标记、但确实是 generic 提取器被 403 挡了——
    /// **用户遇到的正是这一种**（报错里根本没有 Cloudflare 字样）。
    /// 这种也值得开指纹模拟试一次。
    #[test]
    fn unmarked_generic_403_also_worth_impersonating() {
        let real = "ERROR: [generic] watch?v=407946: Unable to download webpage: \
                    HTTP Error 403: Forbidden";
        assert!(is_blocked_generic_403(real));
        assert!(worth_impersonating(real));

        // 但**非 generic** 的 403 不碰：那是别的提取器的事，模拟帮不上
        assert!(!is_blocked_generic_403("ERROR: [youtube] x: HTTP Error 403: Forbidden"));
        assert!(!worth_impersonating("ERROR: [youtube] x: HTTP Error 403: Forbidden"));
        // 404 之类的也不是反爬
        assert!(!is_blocked_generic_403("ERROR: [generic] x: HTTP Error 404: Not Found"));
    }

    /// 自动重试的三个条件，逐个钉住。
    #[test]
    fn auto_retry_requires_all_three_conditions() {
        let cf = "ERROR: [generic] Got HTTP Error 403 caused by Cloudflare anti-bot challenge";
        // 正常触发
        assert!(should_retry_with_impersonate("failed", false, Some(cf)));
        // 已经试过一次 —— **不能再试**，否则死循环
        assert!(!should_retry_with_impersonate("failed", true, Some(cf)));
        // 不是失败（跳过/取消/完成）就不该重试
        assert!(!should_retry_with_impersonate("skipped", false, Some(cf)));
        assert!(!should_retry_with_impersonate("canceled", false, Some(cf)));
        assert!(!should_retry_with_impersonate("completed", false, Some(cf)));
        // 别的失败原因不重试（每个失败都白跑一遍才是最糟的）
        assert!(!should_retry_with_impersonate("failed", false, Some("HTTP Error 403: Forbidden")));
        assert!(!should_retry_with_impersonate("failed", false, Some("网络不可达")));
        assert!(!should_retry_with_impersonate("failed", false, None));
    }
}
