//! Cookie 相关的纯逻辑：Netscape 格式校验 + 浏览器读取结果分类。
//!
//! 这两件事都必须**在宿主侧做**，不能把 yt-dlp 的原始报错甩给用户：
//! - 格式错误前移（DESIGN §6 第 2 条）：用户拿到「第 3 行只有 5 个字段」比拿到
//!   yt-dlp 的一句 `ERROR: ...` 有用得多
//! - 浏览器读取的失败原因**必须区分**（§6.1）：实测 Chrome 与 Edge 在 Windows 上
//!   都失败，但一个是数据库被占用、另一个是 DPAPI 解密失败，
//!   前者关掉浏览器可能就好了，后者关掉也没用

/// yt-dlp 支持的浏览器（对应 `cookies.py` 的
/// `CHROMIUM_BASED_BROWSERS | {'firefox', 'safari'}`）。
///
/// `supports_profiles` 与 yt-dlp 的 `browsers_without_profiles = {'opera'}` 一致——
/// 给 Opera 传 profile 会被 yt-dlp 拒绝。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserInfo {
    pub name: &'static str,
    pub label: &'static str,
    pub supports_profiles: bool,
    /// 各平台可用性。Safari 只在 macOS 上有意义。
    pub windows: bool,
    pub macos: bool,
    pub linux: bool,
    /// 是否 Chromium 系。
    ///
    /// 界面据此**只在选中这类浏览器时**提示读取风险——Windows 上它们把 cookie 库
    /// 加密了（Chrome 的 App-Bound Encryption、Edge 的 DPAPI），实测关掉浏览器也读不到。
    /// 让界面自己去猜名字的话，这份判断就会和这里分叉。
    pub chromium: bool,
}

pub const SUPPORTED_BROWSERS: &[BrowserInfo] = &[
    BrowserInfo { name: "firefox",  label: "Firefox",       supports_profiles: true,  windows: true,  macos: true,  linux: true,  chromium: false },
    BrowserInfo { name: "chrome",   label: "Google Chrome", supports_profiles: true,  windows: true,  macos: true,  linux: true,  chromium: true },
    BrowserInfo { name: "edge",     label: "Microsoft Edge", supports_profiles: true, windows: true,  macos: true,  linux: true,  chromium: true },
    BrowserInfo { name: "brave",    label: "Brave",         supports_profiles: true,  windows: true,  macos: true,  linux: true,  chromium: true },
    BrowserInfo { name: "chromium", label: "Chromium",      supports_profiles: true,  windows: true,  macos: true,  linux: true,  chromium: true },
    BrowserInfo { name: "vivaldi",  label: "Vivaldi",       supports_profiles: true,  windows: true,  macos: true,  linux: true,  chromium: true },
    BrowserInfo { name: "whale",    label: "Naver Whale",   supports_profiles: true,  windows: true,  macos: true,  linux: true,  chromium: true },
    BrowserInfo { name: "opera",    label: "Opera",         supports_profiles: false, windows: true,  macos: true,  linux: true,  chromium: true },
    BrowserInfo { name: "safari",   label: "Safari",        supports_profiles: true,  windows: false, macos: true,  linux: false, chromium: false },
];

/// 查浏览器元数据。
pub fn browser_info(name: &str) -> Option<&'static BrowserInfo> {
    let n = name.trim().to_ascii_lowercase();
    SUPPORTED_BROWSERS.iter().find(|b| b.name == n)
}

/// 该浏览器在本平台是否可用。
pub fn browser_available_here(name: &str) -> bool {
    match browser_info(name) {
        Some(b) => {
            if cfg!(windows) {
                b.windows
            } else if cfg!(target_os = "macos") {
                b.macos
            } else {
                b.linux
            }
        }
        None => false,
    }
}

/// 面向界面的显示名；未知浏览器原样返回。
pub fn browser_label(name: &str) -> String {
    browser_info(name)
        .map(|b| b.label.to_string())
        .unwrap_or_else(|| name.to_string())
}

/// 拼 `--cookies-from-browser` 的值。
///
/// yt-dlp 的语法是 `BROWSER[+KEYRING][:PROFILE][::CONTAINER]`。
/// 这里只处理我们界面上暴露的两段：浏览器与 profile。
///
/// profile 为空时**不加冒号**——`firefox:` 这种写法 yt-dlp 虽然能容忍，
/// 但传一个空 profile 与不传的行为未必一致，不如干脆不写。
pub fn build_browser_spec(browser: &str, profile: Option<&str>) -> String {
    let b = browser.trim();
    match profile.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => format!("{b}:{p}"),
        None => b.to_string(),
    }
}

/// 校验一个 `--cookies-from-browser` 取值是否像话。
///
/// 只查**浏览器名**这一段：profile 名由用户/系统决定，我们无从枚举校验。
pub fn validate_browser_spec(spec: &str) -> Result<(), String> {
    // 语法：BROWSER[+KEYRING][:PROFILE][::CONTAINER]
    let head = spec
        .split("::")
        .next()
        .unwrap_or(spec)
        .split(':')
        .next()
        .unwrap_or(spec);
    let name = head.split('+').next().unwrap_or(head).trim();

    if name.is_empty() {
        return Err("没有指定浏览器。".into());
    }
    if browser_info(name).is_none() {
        let list = SUPPORTED_BROWSERS
            .iter()
            .map(|b| b.name)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!("yt-dlp 不支持浏览器「{name}」。可用的有：{list}"));
    }
    if !browser_available_here(name) {
        return Err(format!("{name} 在当前平台上不可用（例如 Safari 只有 macOS 有）。"));
    }
    Ok(())
}

/// Netscape cookies.txt 的校验错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookieError {
    /// 文件里没有任何有效 cookie 行。
    Empty,
    /// 某一行的格式不对。
    Line { line: usize, reason: String },
}

impl CookieError {
    /// 面向用户的中文说明。
    pub fn message(&self) -> String {
        match self {
            CookieError::Empty => {
                "文件里没有有效的 cookie 行。\n\
                 请确认导出的是 Netscape 格式（以 `# Netscape HTTP Cookie File` 开头），\
                 而不是 JSON 或浏览器导出的其他格式。"
                    .to_string()
            }
            CookieError::Line { line, reason } => format!(
                "第 {line} 行格式不正确：{reason}\n\
                 Netscape 格式每行需要 7 个 **Tab 分隔** 的字段：\n\
                 域名、是否含子域(TRUE/FALSE)、路径、是否仅 HTTPS(TRUE/FALSE)、过期时间、名称、值"
            ),
        }
    }
}

/// 校验 Netscape cookies.txt 的内容，返回有效 cookie 行数。
///
/// 规则：
/// - 空行跳过
/// - `#` 开头的行是注释；**但 `#HttpOnly_` 前缀是有效行的标记**，不算注释
/// - 其余行必须恰好 7 个 Tab 分隔字段，且域名与名称非空
pub fn validate_netscape(content: &str) -> Result<usize, CookieError> {
    let mut count = 0usize;

    for (idx, raw) in content.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }

        // `#HttpOnly_` 是有效 cookie 行，其余 `#` 开头才是注释
        let body = match line.strip_prefix("#HttpOnly_") {
            Some(rest) => rest,
            None => {
                if line.starts_with('#') {
                    continue;
                }
                line
            }
        };

        let fields: Vec<&str> = body.split('\t').collect();
        if fields.len() != 7 {
            // 常见误用：用了空格或逗号分隔（很多「导出工具」这么干）
            let hint = if fields.len() == 1 && body.split_whitespace().count() >= 7 {
                "看起来是用空格而非 Tab 分隔的"
            } else {
                "字段数不对"
            };
            return Err(CookieError::Line {
                line: line_no,
                reason: format!("{}（实际 {} 个）", hint, fields.len()),
            });
        }

        let domain = fields[0].trim();
        let name = fields[5].trim();
        if domain.is_empty() {
            return Err(CookieError::Line {
                line: line_no,
                reason: "域名为空".into(),
            });
        }
        if name.is_empty() {
            return Err(CookieError::Line {
                line: line_no,
                reason: "cookie 名称为空".into(),
            });
        }

        count += 1;
    }

    if count == 0 {
        return Err(CookieError::Empty);
    }
    Ok(count)
}

/// 浏览器 cookie 读取的预检结果（DESIGN §6.2）。
///
/// **必须分类**：跳过预检的话，失败会推迟到每个任务的探测阶段才暴露，
/// 用户只会看到「探测失败」而猜不到是 cookie 的问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserProbe {
    /// 成功读取到 n 条。
    Ok { count: usize, browser: String },
    /// cookie 数据库被浏览器占用 —— 关掉浏览器可能就好了。
    DatabaseLocked,
    /// 系统加密导致无法解密 —— **关掉浏览器也没用**，只能改用 cookies.txt。
    DecryptFailed,
    /// 没找到该浏览器的 cookie 数据库。
    NotFound,
    /// yt-dlp 没有这个浏览器的支持。
    UnknownBrowser,
    /// 其他错误，原文透出。
    Other(String),
}

impl BrowserProbe {
    /// 界面上的结论文案。
    pub fn summary(&self) -> String {
        match self {
            BrowserProbe::Ok { count, browser } => {
                format!("✔ 已从 {browser} 提取 {count} 条 cookie")
            }
            BrowserProbe::DatabaseLocked => {
                "✘ cookie 数据库被占用。请**完全退出**该浏览器后重试\
                 （托盘图标也要退），然后重新测试。"
                    .to_string()
            }
            BrowserProbe::DecryptFailed => {
                "✘ 该浏览器的 cookie 已被系统加密保护，yt-dlp 无法读取。\n\
                 **关闭浏览器也无法解决**，请改用 cookies.txt 方式导入。"
                    .to_string()
            }
            BrowserProbe::NotFound => {
                "✘ 未找到该浏览器的 cookie 数据库。请确认浏览器已安装并至少登录过一次。"
                    .to_string()
            }
            BrowserProbe::UnknownBrowser => "✘ yt-dlp 不支持该浏览器。".to_string(),
            BrowserProbe::Other(e) => format!("✘ 读取失败：{e}"),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, BrowserProbe::Ok { .. })
    }
}

/// 从 yt-dlp 的输出里判定浏览器 cookie 读取结果。
///
/// 判定依据全部来自 **本机实测的真实报错文本**（DESIGN §6.1）：
/// ```text
/// chrome:  ERROR: Could not copy Chrome cookie database. See .../issues/7271
/// edge:    ERROR: Failed to decrypt with DPAPI. See .../issues/10927
/// firefox: Extracted 161 cookies from firefox
/// ```
pub fn classify_browser_probe(stdout: &str, stderr: &str) -> BrowserProbe {
    let all = format!("{stdout}\n{stderr}");

    // 成功优先：即使有别的噪声，只要报了条数就算成功
    if let Some(p) = parse_extracted(&all) {
        return p;
    }

    // 数据库被占用 / 无法复制（Chrome 在运行时就是这个）
    if all.contains("Could not copy")
        || all.contains("database is locked")
        || all.contains("unable to open database")
        || all.contains("Permission denied")
    {
        return BrowserProbe::DatabaseLocked;
    }

    // 系统加密导致解密失败（Edge 在本机就是这个）
    if all.contains("Failed to decrypt with DPAPI")
        || all.contains("failed to decrypt cookie")
        || all.contains("app-bound")
        || all.contains("App-Bound")
    {
        return BrowserProbe::DecryptFailed;
    }

    if all.contains("could not find") && all.contains("cookie") {
        return BrowserProbe::NotFound;
    }
    if all.contains("unsupported browser") || all.contains("is not a supported browser") {
        return BrowserProbe::UnknownBrowser;
    }

    let msg = stderr
        .lines()
        .find(|l| l.starts_with("ERROR:"))
        .map(|l| l.trim_start_matches("ERROR:").trim().to_string())
        .or_else(|| {
            stderr
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "未知错误".into());
    BrowserProbe::Other(msg)
}

/// 解析 `Extracted 161 cookies from firefox` 这样的成功输出。
fn parse_extracted(text: &str) -> Option<BrowserProbe> {
    let line = text
        .lines()
        .find(|l| l.contains("Extracted") && l.contains("cookies from"))?;
    // 期望形如: Extracted 161 cookies from firefox
    let after = line.split("Extracted").nth(1)?.trim();
    let count: usize = after.split_whitespace().next()?.parse().ok()?;
    let browser = after
        .split("cookies from")
        .nth(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    Some(BrowserProbe::Ok { count, browser })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "# Netscape HTTP Cookie File\n";

    #[test]
    fn accepts_valid_file() {
        let c = format!(
            "{HEADER}\
             .bilibili.com\tTRUE\t/\tFALSE\t1790000000\tSESSDATA\tabc%2Cdef\n\
             .youtube.com\tTRUE\t/\tTRUE\t1790000000\tSID\txyz\n"
        );
        assert_eq!(validate_netscape(&c).unwrap(), 2);
    }

    /// `#HttpOnly_` 前缀是有效行，不能当注释跳过——跳过了会丢 cookie。
    #[test]
    fn http_only_prefix_is_a_real_line() {
        let c = format!(
            "{HEADER}#HttpOnly_.example.com\tTRUE\t/\tFALSE\t1790000000\ttoken\tv\n"
        );
        assert_eq!(validate_netscape(&c).unwrap(), 1);
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let c = format!(
            "{HEADER}# this is a comment\n\n   \n\
             .a.com\tTRUE\t/\tFALSE\t1\tn\tv\n"
        );
        assert_eq!(validate_netscape(&c).unwrap(), 1);
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(validate_netscape(""), Err(CookieError::Empty));
        assert_eq!(
            validate_netscape("# Netscape HTTP Cookie File\n\n# nothing here\n"),
            Err(CookieError::Empty)
        );
    }

    /// 最常见的错误：用空格而不是 Tab 分隔。
    #[test]
    fn detects_space_separated() {
        let c = format!("{HEADER}.a.com TRUE / FALSE 1 n v\n");
        match validate_netscape(&c) {
            Err(CookieError::Line { line, reason }) => {
                assert_eq!(line, 2);
                assert!(reason.contains("空格"), "实际: {reason}");
            }
            other => panic!("期望 Line 错误，得到 {other:?}"),
        }
    }

    #[test]
    fn reports_wrong_field_count() {
        let c = format!("{HEADER}.a.com\tTRUE\t/\tFALSE\t1\n");
        match validate_netscape(&c) {
            Err(CookieError::Line { line, reason }) => {
                assert_eq!(line, 2);
                assert!(reason.contains("实际 5 个"), "实际: {reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reports_empty_domain_or_name() {
        let c = format!("{HEADER}\tTRUE\t/\tFALSE\t1\tn\tv\n");
        assert!(matches!(
            validate_netscape(&c),
            Err(CookieError::Line { reason, .. }) if reason.contains("域名")
        ));
        let c = format!("{HEADER}.a.com\tTRUE\t/\tFALSE\t1\t\tv\n");
        assert!(matches!(
            validate_netscape(&c),
            Err(CookieError::Line { reason, .. }) if reason.contains("名称")
        ));
    }

    /// 错误信息要能指导用户，而不是只说「错了」。
    #[test]
    fn error_messages_are_actionable() {
        let msg = CookieError::Empty.message();
        assert!(msg.contains("Netscape"));
        let msg = CookieError::Line {
            line: 3,
            reason: "字段数不对".into(),
        }
        .message();
        assert!(msg.contains("第 3 行"));
        assert!(msg.contains("Tab"));
    }

    #[test]
    fn tolerates_crlf_line_endings() {
        let c = format!(
            "{HEADER}.a.com\tTRUE\t/\tFALSE\t1\tn\tv\r\n.b.com\tTRUE\t/\tFALSE\t1\tn2\tv2\r\n"
        );
        assert_eq!(validate_netscape(&c).unwrap(), 2);
    }

    // ─────────── 浏览器预检分类（用本机实测的真实文本）───────────

    #[test]
    fn classifies_firefox_success() {
        let p = classify_browser_probe("Extracted 161 cookies from firefox\n", "");
        assert_eq!(
            p,
            BrowserProbe::Ok {
                count: 161,
                browser: "firefox".into()
            }
        );
        assert!(p.is_ok());
        assert!(p.summary().contains("161"));
    }

    /// Chrome 在运行时的真实报错。
    #[test]
    fn classifies_chrome_copy_failure_as_locked() {
        let err = "ERROR: Could not copy Chrome cookie database. \
                   See  https://github.com/yt-dlp/yt-dlp/issues/7271  for more info";
        assert_eq!(classify_browser_probe("", err), BrowserProbe::DatabaseLocked);
    }

    /// Edge 的真实报错——它**没在运行**，所以这是解密问题，关浏览器没用。
    #[test]
    fn classifies_edge_dpapi_failure_as_decrypt() {
        let err = "ERROR: Failed to decrypt with DPAPI. \
                   See  https://github.com/yt-dlp/yt-dlp/issues/10927  for more info";
        let p = classify_browser_probe("", err);
        assert_eq!(p, BrowserProbe::DecryptFailed);
        // 指引必须明确「关掉浏览器也没用」，否则用户会白折腾
        let s = p.summary();
        assert!(s.contains("关闭浏览器也无法解决"), "实际: {s}");
        assert!(s.contains("cookies.txt"));
    }

    /// 「被占用」与「解密失败」的指引必须不同。
    #[test]
    fn locked_and_decrypt_have_different_guidance() {
        assert_ne!(
            BrowserProbe::DatabaseLocked.summary(),
            BrowserProbe::DecryptFailed.summary()
        );
        assert!(BrowserProbe::DatabaseLocked.summary().contains("退出"));
    }

    #[test]
    fn classifies_not_found_and_unsupported() {
        assert_eq!(
            classify_browser_probe("", "ERROR: could not find firefox cookie database"),
            BrowserProbe::NotFound
        );
        assert_eq!(
            classify_browser_probe("", "ERROR: brave is not a supported browser"),
            BrowserProbe::UnknownBrowser
        );
    }

    /// 成功优先于噪声：即使同时有别的输出，只要报了条数就算成功。
    #[test]
    fn success_wins_over_noise() {
        let out = "Some warning\nExtracted 42 cookies from chrome\n";
        assert_eq!(
            classify_browser_probe(out, ""),
            BrowserProbe::Ok {
                count: 42,
                browser: "chrome".into()
            }
        );
    }

    #[test]
    fn unknown_error_is_surfaced() {
        let p = classify_browser_probe("", "ERROR: something entirely new happened");
        assert_eq!(
            p,
            BrowserProbe::Other("something entirely new happened".into())
        );
        assert!(p.summary().contains("something entirely new"));
    }

    #[test]
    fn empty_output_is_other_not_panic() {
        assert!(!classify_browser_probe("", "").is_ok());
    }

    // ─────────── 浏览器清单与规格构造 ───────────

    /// 清单必须与 yt-dlp 的 `SUPPORTED_BROWSERS` 一致。
    #[test]
    fn browser_list_matches_ytdlp() {
        let chromium = ["brave", "chrome", "chromium", "edge", "opera", "vivaldi", "whale"];
        for n in chromium {
            assert!(browser_info(n).is_some(), "缺少 chromium 系浏览器 {n}");
        }
        assert!(browser_info("firefox").is_some());
        assert!(browser_info("safari").is_some());
        assert_eq!(SUPPORTED_BROWSERS.len(), chromium.len() + 2);
        // 大小写不敏感
        assert!(browser_info("FireFox").is_some());
    }

    /// Opera 不支持 profile —— 与 yt-dlp 的 `browsers_without_profiles` 一致。
    #[test]
    fn opera_is_the_only_browser_without_profiles() {
        for b in SUPPORTED_BROWSERS {
            let expect = b.name != "opera";
            assert_eq!(b.supports_profiles, expect, "{} 的 supports_profiles 不对", b.name);
        }
    }

    #[test]
    fn safari_is_macos_only() {
        let s = browser_info("safari").unwrap();
        assert!(s.macos);
        assert!(!s.windows);
        assert!(!s.linux);
        // 在 Windows 上应判为不可用
        if cfg!(windows) {
            assert!(!browser_available_here("safari"));
            assert!(browser_available_here("firefox"));
        }
    }

    #[test]
    fn builds_browser_spec() {
        assert_eq!(build_browser_spec("firefox", None), "firefox");
        assert_eq!(build_browser_spec("firefox", Some("")), "firefox");
        assert_eq!(build_browser_spec("firefox", Some("   ")), "firefox");
        assert_eq!(
            build_browser_spec("firefox", Some("9mz7ax6i.default-release")),
            "firefox:9mz7ax6i.default-release"
        );
        // Chrome 的 profile 名带空格，必须原样保留
        assert_eq!(build_browser_spec("chrome", Some("Profile 1")), "chrome:Profile 1");
    }

    /// `firefox:` 这种空 profile 写法要避免——不传就别加冒号。
    #[test]
    fn empty_profile_does_not_add_colon() {
        for p in [None, Some(""), Some("  ")] {
            assert_eq!(build_browser_spec("chrome", p), "chrome");
        }
    }

    #[test]
    fn validates_browser_spec() {
        assert!(validate_browser_spec("firefox").is_ok());
        assert!(validate_browser_spec("firefox:some.profile").is_ok());
        assert!(validate_browser_spec("chrome:Profile 1").is_ok());
        // 带 keyring / container 的完整语法也要能过
        assert!(validate_browser_spec("chrome+gnomekeyring:Default").is_ok());
        assert!(validate_browser_spec("firefox::work").is_ok());
    }

    #[test]
    fn rejects_bad_browser_spec() {
        let e = validate_browser_spec("netscape").unwrap_err();
        assert!(e.contains("不支持"), "实际: {e}");
        // 错误信息要列出可选项，否则用户不知道能填什么
        assert!(e.contains("firefox"), "实际: {e}");
        assert!(e.contains("chrome"), "实际: {e}");

        assert!(validate_browser_spec("").unwrap_err().contains("没有指定"));
        assert!(validate_browser_spec("   ").unwrap_err().contains("没有指定"));
        if cfg!(windows) {
            assert!(validate_browser_spec("safari").unwrap_err().contains("不可用"));
        }
    }

    #[test]
    fn browser_label_fallback() {
        assert_eq!(browser_label("firefox"), "Firefox");
        assert_eq!(browser_label("edge"), "Microsoft Edge");
        // 未知的原样返回，不要 panic
        assert_eq!(browser_label("whatever"), "whatever");
    }
}
