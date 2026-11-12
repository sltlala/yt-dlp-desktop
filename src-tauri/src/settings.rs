//! 设置：默认值、旧配置迁移、归一化与读写。

use crate::files::temp_dir_in_use;
use crate::state::AppState;
use crate::{logfile, paths};
use serde_json::{json, Value};
use tauri::State;

/// 与 `src/mock.ts` 的 `defaultSettings` 保持同构。
pub fn default_settings() -> Value {
    json!({
        // 必须是真实可用的目录：空串会让设置页与侧栏都显示空白
        "outputDir": paths::default_output_dir().to_string_lossy(),
        "tempDir": paths::default_temp_root().to_string_lossy(),
        "probeConcurrency": 6,
        "downloadConcurrency": 2,
        "perHostConcurrency": 1,
        "preset": "best",
        "maxHeight": 1080,
        "audioFormat": "mp3",
        "container": "auto",
        "embed": {
            "subs": false, "subLangs": ytdlp_core::DEFAULT_SUB_LANGS, "autoSubs": false,
            "keepSubFiles": false, "thumbnail": false, "keepThumbnailFile": false,
            "metadata": false, "chapters": false, "infoJson": false
        },
        "cookieMode": "none",
        "cookieFile": "",
        "cookieBrowser": "firefox",
        // ── 代理（对应 Windows「设置 → 网络和 Internet → 代理」那一页）──
        // none / system（读注册表）/ manual
        "proxyMode": "none",
        "proxyProtocol": "http",
        "proxyHost": "",
        "proxyPort": 8080,
        "proxyAuth": false,
        "proxyUser": "",
        "proxyPassword": "",
        // 不勾「记住」时密码只留在内存，写盘前会被抹掉（见 save_settings）
        "proxyRemember": true,
        // ── 按站点分流（ROADMAP §F18）──
        // per-host 规则表：{ host, proxy }。proxy 为 "" 或 "direct" 表示直连。
        // 匹配优先级 = 数组顺序（界面上排前面的优先）。
        "proxyRules": [],
        "aria2c": false,
        "archiveEnabled": false,
        "archivePath": paths::archive_file().to_string_lossy(),
        "limitRate": "",
        "filenameTemplate": "%(title).150B [%(id)s].%(ext)s",
        // 「优先选择」的编码，空 = 不指定。值必须是 yt-dlp 报出的编码名前缀
        // （`avc1` / `vp9` / `av01` / `mp4a` / `opus` / `vorbis`），
        // 白名单在 `runner::codec_of` 里把关（DESIGN §3.3）。
        "preferVcodec": "",
        "preferAcodec": "",
        // JS 运行时：留空表示自动检测。**不是可选优化**——
        // 不给的话 YouTube 会返回「需要重载页面」或只给 storyboard。
        // 注意这里**没有** `jsRemoteComponents`：界面上已经不提供那个开关，
        // 但 `runner::js_of` 仍然读它，好让需要的人改 config.json 打开。
        "jsRuntime": "",
        // 对 generic 提取器开启指纹模拟，过 Cloudflare 拦截。
        // **默认关**：yt-dlp 自己默认也不做（issue #11335），它的帮助文本
        // 明确警告强制模拟会拖慢速度、降低稳定性。撞上拦截时界面上会指过来。
        "impersonate": false,
        // ── 章节切分（ROADMAP §F15）──
        // 长视频按章节拆成多个文件。默认关：会显著增加后处理时间。
        "splitChapters": false,
        // SponsorBlock：off / mark（标成章节）/ remove（剪掉赞助等片段）。只对 YouTube 生效。
        "sponsorblock": "off",
        // 点关闭按钮时只隐藏到托盘，下载继续
        "closeToTray": true,
        // 订阅多久自动检查一次；0 = 只手动检查
        "subscriptionIntervalHours": 6,
        "autoCheckAppUpdate": true,
        // ── 下载后动作（ROADMAP §F16）──
        // 完成后执行的命令（--exec）。留空 = 不执行。
        // ⚠️ 会以本机权限执行，界面必须做安全确认，绝不静默启用。
        "execCommand": "",
        // ── 系统通知（ROADMAP §F1）──
        // 任务进入终态时弹系统通知。skipped 是「已存在跳过」，不算坏事，默认不弹。
        "notifyOnComplete": true,
        "notifyOnFailure": true,
        "notifyOnSkip": false,
        // ── 剪贴板监听（ROADMAP §F4）──
        // 默认关：读剪贴板是敏感操作，且容易在用户复制别的文本时误弹。
        "watchClipboard": false,
        // ── 界面主题（ROADMAP §F20）──
        // light / dark / system。system = 跟随操作系统深浅色，前端监听
        // prefers-color-scheme 实时切换。
        "theme": "system",
        // ── 浏览器扩展一键推送（ROADMAP §F19）──
        // 是否启用本机 127.0.0.1:19090 接收端点。默认开：只监听回环地址，
        // 本机外的程序连不进来，风险很低。
        "browserPush": true
    })
}

pub fn load_settings() -> Value {
    std::fs::read_to_string(paths::settings_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(default_settings)
}

/// 递归补齐缺失的设置键。
///
/// `defaults` 的结构决定结果结构：对象逐键递归，其余以 `stored` 为准
/// （只在 `stored` 是 `null` 时回落到默认值）。
pub fn merge_defaults(defaults: Value, stored: Value) -> Value {
    match defaults {
        Value::Object(d) => match stored {
            Value::Object(mut u) => {
                for (k, dv) in d {
                    let v = match u.remove(&k) {
                        Some(uv) => merge_defaults(dv, uv),
                        None => dv,
                    };
                    u.insert(k, v);
                }
                Value::Object(u)
            }
            // 类型对不上（null、字符串…）时回到默认值，
            // 否则前端会拿到它没预期的类型
            _ => Value::Object(d),
        },
        other => {
            if stored.is_null() {
                other
            } else {
                stored
            }
        }
    }
}

/// 把早期的 `proxyEnabled` + `proxyUrl` 迁移成结构化的 `proxyMode` 配置。
///
/// 早期版本只有一个「使用代理 + 一个 URL 输入框」。升级后如果不管它，
/// 用户配好的代理会静默失效——那是「明明昨天还能下、今天全失败」的经典来源。
pub fn migrate_proxy(mut s: Value) -> Value {
    // 已经有新配置就什么都不做
    if s.get("proxyMode").is_some() {
        return s;
    }
    let Some(obj) = s.as_object_mut() else {
        return s;
    };

    let enabled = obj.get("proxyEnabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let url = obj
        .get("proxyUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let parsed = if enabled {
        ytdlp_core::parse_proxy_url(&url)
    } else {
        None
    };

    match parsed {
        Some(c) => {
            obj.insert("proxyMode".into(), Value::String("manual".into()));
            obj.insert("proxyProtocol".into(), Value::String(c.protocol.as_str().into()));
            obj.insert("proxyHost".into(), Value::String(c.host));
            obj.insert("proxyPort".into(), Value::from(c.port));
            if !c.user.is_empty() {
                obj.insert("proxyAuth".into(), Value::Bool(true));
                obj.insert("proxyUser".into(), Value::String(c.user));
                obj.insert("proxyPassword".into(), Value::String(c.password));
            }
        }
        None => {
            obj.insert("proxyMode".into(), Value::String("none".into()));
        }
    }
    s
}

/// 补全缺失或为空的设置项。
///
/// 两个真实踩过的坑：
/// 1. 早期版本把 `outputDir` 默认成空串，用户在设置页看到的是空栏、
///    侧栏也不显示路径——看起来像「没有设置项」。
/// 2. 后来新增 `jsRuntime` 键，**老配置里根本没有这个键**，前端
///    `s.jsRuntime.trim()` 抛 TypeError，整个设置面板白屏。逐个特判挡不住
///    下一个新键，所以这里直接按 `default_settings()` 递归补齐。
pub fn normalize_settings(s: Value) -> Value {
    // ⚠️ 迁移必须在补全默认值**之前**：补全会把 `proxyMode` 填成默认的 "none"，
    // 那样就再也分不清「老配置里压根没有这个键」和「用户真的选了不用代理」，
    // 升级时会把用户已经配好的代理静默丢掉。
    let s = migrate_proxy(s);
    let mut s = merge_defaults(default_settings(), s);
    // `proxyBypass` 已废弃：直连名单现在是 `proxy::LOCAL_BYPASS` 常量，用户不可配。
    // `merge_defaults` 只补键不删键，所以老配置里的这个键会一直留在 config.json 里，
    // 看起来像个能用的开关。这里显式丢掉，避免误导。
    if let Some(obj) = s.as_object_mut() {
        obj.remove("proxyBypass");
    }
    // 老配置里 `subLangs` 是旧默认值 `all,-live_chat`：它没排除 `danmaku`，而 B站弹幕
    // 既嵌不进 mkv（XML，ffmpeg 读不了），走 aria2c 又必然报错（裸 deflate 解不开），
    // 会把整条任务判失败。只迁移**恰好等于旧默认值**的配置——用户自己填过的一律不动。
    // 放在 `merge_defaults` 之后：此时缺键已被补成新默认值，剩下的就是真正的存量。
    if s.pointer("/embed/subLangs")
        .and_then(|v| v.as_str())
        .map(|v| v.trim() == ytdlp_core::LEGACY_DEFAULT_SUB_LANGS)
        .unwrap_or(false)
    {
        if let Some(embed) = s.get_mut("embed").and_then(Value::as_object_mut) {
            embed.insert(
                "subLangs".into(),
                Value::String(ytdlp_core::DEFAULT_SUB_LANGS.to_string()),
            );
        }
    }
    let blank = s
        .get("outputDir")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().is_empty())
        .unwrap_or(true);
    if blank {
        if let Some(obj) = s.as_object_mut() {
            obj.insert(
                "outputDir".into(),
                Value::String(paths::default_output_dir().to_string_lossy().into_owned()),
            );
        }
    }
    s
}

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Value {
    state
        .settings
        .lock()
        .map(|s| s.clone())
        .unwrap_or_else(|_| default_settings())
}

#[tauri::command(async)]
pub fn save_settings(state: State<AppState>, settings: Value) -> Result<(), String> {
    // ⚠️ 归一化后再落内存和磁盘。`get_settings` 是归一化过的，如果保存路径
    // 不归一化，「读到的」和「存进去的」就会不一致：新增设置键时前端拿到的是
    // `undefined`（历史上就是这样把整个设置面板打成白屏的）。
    // 这里也保证了 `normalize_settings` 里清掉的废弃键不会又被写回去。
    let settings = normalize_settings(settings);

    // ⚠️ 改 `tempDir` 会让**已经存在的 .part 找不到**：新目录里没有它们，
    // yt-dlp 会从头下，旧碎片永远留在旧目录里（那种目录只在启动时按**当前**
    // tempRoot 清理，换了根就再也扫不到）。
    //
    // 界面已经把输入框禁掉了，这里再加一道：真发生了至少要在日志里看得见，
    // 否则「为什么断点续传没了」永远查不出来。
    {
        let old = state
            .settings
            .lock()
            .map(|s| s.get("tempDir").and_then(|v| v.as_str()).unwrap_or("").to_string())
            .unwrap_or_default();
        let new = settings
            .get("tempDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if old != new {
            let busy = temp_dir_in_use(state.clone());
            if busy.get("inUse").and_then(|v| v.as_bool()).unwrap_or(false) {
                logfile::warn(format!(
                    "tempDir 从「{old}」改成「{new}」，但有 {} 个任务的临时目录里还有可续传文件——\
                     这些任务的断点续传会失效",
                    busy.get("count").and_then(|v| v.as_u64()).unwrap_or(0)
                ));
            } else {
                logfile::info(format!("tempDir 改为「{new}」"));
            }
        }
    }

    if let Ok(mut s) = state.settings.lock() {
        *s = settings.clone();
    }

    // 「记住密码」没勾时，密码只留在内存里，**写盘前抹掉**。
    // 明文密码落在 config.json 里是用户明确表示不想要的事。
    let mut on_disk = settings;
    let remember = on_disk
        .get("proxyRemember")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if !remember {
        if let Some(o) = on_disk.as_object_mut() {
            o.insert("proxyPassword".into(), Value::String(String::new()));
        }
    }

    let dir = paths::app_data_root();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&on_disk).map_err(|e| e.to_string())?;
    std::fs::write(paths::settings_file(), text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 老配置里没有新键 → 前端 `s.jsRuntime.trim()` 抛 TypeError、设置面板白屏。
    /// 这个测试锁住「缺失键必须被补齐」。
    #[test]
    fn merge_defaults_fills_missing_keys() {
        let defaults = json!({ "a": "", "jsRuntime": "" });
        let stored = json!({ "a": "keep" });
        let merged = merge_defaults(defaults, stored);

        assert_eq!(merged["a"], json!("keep"));
        assert_eq!(merged["jsRuntime"], json!(""));
    }

    /// 界面不再暴露 `jsRemoteComponents`，但它**仍是从 config.json 可开的逃生门**：
    /// 一旦它回到 `default_settings`，设置页就会重新显示一个没人改的勾选框。
    #[test]
    fn remote_components_is_not_a_default_setting() {
        let s = normalize_settings(json!({}));
        assert!(s.get("jsRemoteComponents").is_none(), "实际: {s}");
        // 手工写进去的值必须被保留，否则逃生门形同虚设。
        let s = normalize_settings(json!({ "jsRemoteComponents": true }));
        assert_eq!(s["jsRemoteComponents"], json!(true));
    }

    /// 旧默认 `all,-live_chat` 必须被迁成含 `-danmaku` 的新默认；用户自定义值原样保留。
    #[test]
    fn sub_langs_migrates_legacy_default_only() {
        // 恰好等于旧默认 → 迁移
        let s = normalize_settings(json!({ "embed": { "subs": true, "subLangs": "all,-live_chat" } }));
        assert_eq!(
            s["embed"]["subLangs"], json!(ytdlp_core::DEFAULT_SUB_LANGS),
            "实际: {}", s["embed"]["subLangs"]
        );
        // 缺键 → 补成新默认
        let s = normalize_settings(json!({}));
        assert_eq!(s["embed"]["subLangs"], json!(ytdlp_core::DEFAULT_SUB_LANGS));
        // 用户自定义 → 不动（这是逃生门）
        let s = normalize_settings(json!({ "embed": { "subLangs": "zh-Hans,en" } }));
        assert_eq!(s["embed"]["subLangs"], json!("zh-Hans,en"));
        let s = normalize_settings(json!({ "embed": { "subLangs": "all,-live_chat,danmaku" } }));
        assert_eq!(s["embed"]["subLangs"], json!("all,-live_chat,danmaku"));
    }

    #[test]
    fn merge_defaults_recurses_into_nested_objects() {
        let defaults = json!({ "embed": { "subs": false, "thumbnail": false } });
        let stored = json!({ "embed": { "subs": true } });
        let merged = merge_defaults(defaults, stored);

        assert_eq!(merged["embed"]["subs"], json!(true));
        assert_eq!(merged["embed"]["thumbnail"], json!(false));
    }

    /// 类型对不上时不能把用户的值吞掉，也不能让前端拿到错类型。
    #[test]
    fn merge_defaults_keeps_stored_values() {
        let merged = merge_defaults(json!({ "n": 1, "s": "x" }), json!({ "n": 7, "s": "y" }));
        assert_eq!(merged["n"], json!(7));
        assert_eq!(merged["s"], json!("y"));
    }

    #[test]
    fn merge_defaults_replaces_null_with_object_default() {
        let merged = merge_defaults(json!({ "embed": { "subs": false } }), json!({ "embed": null }));
        assert_eq!(merged["embed"]["subs"], json!(false));
    }

    #[test]
    fn merge_defaults_non_object_stored_falls_back_to_defaults() {
        let merged = merge_defaults(json!({ "a": 1 }), json!("not an object"));
        assert_eq!(merged["a"], json!(1));
    }

    /// 非对象默认值 + `null` 存量 → 用默认值；非 null 存量 → 原样保留。
    #[test]
    fn merge_defaults_scalar_rules() {
        assert_eq!(merge_defaults(json!(5), json!(null)), json!(5));
        assert_eq!(merge_defaults(json!(5), json!(9)), json!(9));
    }

    /// **升级不能把用户配好的代理丢掉**——否则表现是「昨天还能下，今天全失败」。
    #[test]
    fn legacy_enabled_proxy_migrates_to_manual() {
        let s = normalize_settings(json!({
            "proxyEnabled": true,
            "proxyUrl": "http://127.0.0.1:7897",
        }));
        assert_eq!(s["proxyMode"], json!("manual"));
        assert_eq!(s["proxyHost"], json!("127.0.0.1"));
        assert_eq!(s["proxyPort"], json!(7897));
        assert_eq!(s["proxyProtocol"], json!("http"));
    }

    #[test]
    fn legacy_disabled_proxy_migrates_to_none() {
        let s = normalize_settings(json!({ "proxyEnabled": false, "proxyUrl": "http://h:1" }));
        assert_eq!(s["proxyMode"], json!("none"));
    }

    /// 勾了「使用代理」但地址是空的/填坏的 → 退回不用代理，
    /// 而不是留一个 manual + 空主机（那会让每次请求都失败）。
    #[test]
    fn legacy_enabled_but_unparseable_falls_back_to_none() {
        for bad in ["", "   ", "http://:8080"] {
            let s = normalize_settings(json!({ "proxyEnabled": true, "proxyUrl": bad }));
            assert_eq!(s["proxyMode"], json!("none"), "输入: {bad:?}");
        }
    }

    #[test]
    fn legacy_proxy_with_credentials_migrates_auth() {
        let s = normalize_settings(json!({
            "proxyEnabled": true,
            "proxyUrl": "socks5://u:p%40w@h:1080",
        }));
        assert_eq!(s["proxyMode"], json!("manual"));
        assert_eq!(s["proxyProtocol"], json!("socks5h"));
        assert_eq!(s["proxyAuth"], json!(true));
        assert_eq!(s["proxyUser"], json!("u"));
        assert_eq!(s["proxyPassword"], json!("p@w"));
    }

    /// 已经是新配置时，迁移必须**原样放过**——不能把用户选的 none 又改回 manual。
    #[test]
    fn migration_does_not_touch_new_config() {
        let s = normalize_settings(json!({
            "proxyMode": "none",
            "proxyEnabled": true,
            "proxyUrl": "http://127.0.0.1:7897",
        }));
        assert_eq!(s["proxyMode"], json!("none"));
    }

    /// 绕过列表已改为**内建常量**，设置里不该再出现这个键——
    /// 留着它意味着界面上又会冒出一个没人改的输入框。
    #[test]
    fn proxy_bypass_is_no_longer_a_setting() {
        let s = normalize_settings(json!({}));
        assert!(s.get("proxyBypass").is_none(), "实际: {s}");

        // 老配置里残留的键要被清掉，否则 config.json 里一直挂着一个
        // 看起来能用的开关（`merge_defaults` 只补键不删键）。
        let s = normalize_settings(json!({ "proxyBypass": "example.com", "proxyMode": "manual" }));
        assert!(s.get("proxyBypass").is_none(), "实际: {s}");
    }
}
