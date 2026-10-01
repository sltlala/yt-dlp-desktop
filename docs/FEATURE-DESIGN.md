# 功能详细设计（ROADMAP 的落地手册）

> 本文是 `ROADMAP.md` 每个功能的**详细设计**：UI 草图、字段/设置清单、后端挂钩点、技术风险。
> 动手实现某个功能前，先读对应小节；实现完回填「状态」和「落地偏差」。
> 所有「现有结构」的引用以 `DESIGN.md` 和当前代码为准。

---

## F1 下载完成/失败通知

**状态**：⬜ 未开始
**优先级**：P0（建议第一件做）

### 目标
任务进入终态（completed / skipped / failed / canceled）时，弹出系统通知。用户切到别的窗口也能知道结果。

### 现状（技术前提，已核实）
- 项目**没有**装 `tauri-plugin-notification`（`Cargo.toml` 无此依赖、`lib.rs` 无 `.plugin()` 注册、grep 无 `Notification`）。
- `capabilities/default.json` 目前只有 `core:default` + `core:event:*`，没有通知权限。
- 任务终态的**唯一权威挂钩点**在 `src-tauri/src/runner.rs` 的 `run_download` 收尾段（约 line 345–390），那里已经把任务切成 `completed` / `skipped` / `failed` / `canceled`，并写日志（line 406）。

### 方案
1. 新增依赖：`tauri-plugin-notification = "2"`（Rust） + `@tauri-apps/plugin-notification`（npm）。
2. `lib.rs` 的 `Builder` 链加 `.plugin(tauri_plugin_notification::init())`。
3. `capabilities/default.json` 加 `notification:default`（或 `notification:allow-notify` + `notification:allow-request-permission`）。
4. 在终态挂钩点（run_download 收尾）发通知：
   - `completed` → 标题「下载完成」，正文 = 任务标题（截断 60 字）。
   - `failed` → 标题「下载失败」，正文 = 首行错误（截断）。
   - `skipped` → 默认**不弹**（它是「已存在跳过」，不是坏事）；可在设置里开启。
   - `canceled` → 不弹。
5. 通知策略做成**设置项**，存进 `default_settings()`：
   - `notifyOnComplete: bool`（默认 true）
   - `notifyOnFailure: bool`（默认 true）
   - `notifyOnSkip: bool`（默认 false）
6. **去重**：一次终态只弹一次。`run_download` 只在「状态从非终态 → 终态」时弹；重试路径（`should_retry_with_impersonate`）重新入队时**不要**弹「失败」，等最终结果。

### 字段清单
| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `notifyOnComplete` | bool | true | 完成时弹 |
| `notifyOnFailure` | bool | true | 失败时弹 |
| `notifyOnSkip` | bool | false | 跳过时弹 |

### UI 草图
```
设置 → 通知
☑ 下载完成时通知
☑ 下载失败时通知
☐ 「已跳过」也通知
```

### 风险
- **Windows 通知需要应用有 AppUserModelID / 开始菜单快捷方式**：Tauri 的 NSIS 安装包已处理，但 `tauri dev` 直接跑 exe 时可能弹不出来——这是已知现象，不是 bug，写进文档提醒。
- **权限请求**：桌面端 Windows 一般无需显式请求，`requestPermission` 在 Windows 上是 no-op，代码里别把它当硬前提。
- **通知正文编码**：任务标题是 GBK 控制台解码来的，已是正确 UTF-8 字符串，直接传即可，不要二次转码。

---

## F2 失败自动重试 + 智能降级

**状态**：⬜ 未开始
**优先级**：P0

### 目标
网络抖动、代理闪断、aria2c 特有失败（§11.4）时，自动用「更稳的配置」重试一次，用户不用手动点「重新下载」。

### 现状
- 已有 `--retries 10`（yt-dlp 内部重试），也有 `should_retry_with_impersonate`（Cloudflare 指纹模拟自动重试一次）。
- 但**没有**「换下载器 / 关 aria2c」这类**降级重试**。

### 方案（降级阶梯）
对 `failed` 任务，按以下顺序各自动重试一次（每级只一次，避免死循环）：

1. **原样重试**（yt-dlp 已内部 retry 过，这里再补一次调度层重试）——针对偶发网络抖动。
2. **关 aria2c → 原生下载器**——针对 §11.4 的 aria2c 特有失败（裸 deflate、gzip header）。
3. **关指纹模拟**（若开了 `impersonate`）——针对「模拟反而更慢/失败」的场景。

实现要点：
- 复用 `scheduler` 的重新入队（像 `should_retry_with_impersonate` 那样，不走本地循环）。
- 任务上加一个 `retry_tier: u8`（或 `autoRetryTries`）字段，记录已走到第几级，防止无限重试。
- 只对**可重试的错误**触发（网络错误、aria2c inflate、5xx）；对「格式不可用」「认证失效」「磁盘满」这类**确定性错误不重试**（重试也没用，还浪费）。

### 字段清单
| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `autoRetry` | bool | true | 设置项：是否自动重试 |
| `retry_tier` | u8（任务级） | 0 | 已走过的降级阶梯 |
| `retry_max` | u8 | 3 | 最多自动重试次数 |

### 风险
- **误判可重试性**是最大风险：把「认证失效」当成网络抖动重试，会反复撞同一堵墙。需要复用 §16 待定的「错误分类体系」，先落地一个最小分类器（正则匹配错误关键词），否则宁可少重试、不要瞎重试。

---

## F3 批量粘贴 / 拖多链接

**状态**：⬜ 未开始
**优先级**：P0

### 目标
一次下整个合集，不用把 URL 一条条贴进输入框。

### 现状
- `AddUrlBar.vue` 是单 URL 输入框，`store.addUrl(url)` 每次只加一条。

### 方案
1. 输入框改成**支持多行**（或用「批量」按钮弹一个 textarea）。
2. 粘贴内容按 `\n` 分割，逐行 `trim`，过滤空行、去重（按 URL 字符串）。
3. 支持**拖入文件**：拖入一个 `.txt`，读取文本（每行一个 URL）后同上述处理。Tauri 的 drag-drop 事件在 WebView2 里需要额外开启 `dragDropEnabled`（`tauri.conf.json` 的 window 配置），否则拿不到文件路径。
4. 每条 URL 走现有 `add_url`，但**批量入队**要一次性返回「成功 N 条 / 失败 M 条」，而不是逐条弹事件。

### 后端挂钩
- 新增命令 `add_urls(Vec<String>) -> BatchAddResult { added: usize, failed: Vec<(String, String)> }`，循环调 `add_url` 的核心逻辑，避免前端 N 次 invoke。
- 输入里可能混着「非 URL 文本」（比如从网页复制带了标题），用一个轻量判断（`ytdlp_core::url::is_valid_url`，若已有）过滤，无效的收集到 `failed` 里提示。

### UI 草图
```
┌──────────────────────────────────────────┐
│ [批量添加 ▾]                              │
│ ┌──────────────────────────────────────┐ │
│ │ https://.../BV1                        │ │
│ │ https://.../BV2                        │ │
│ │ （或把 .txt 拖进来）                    │ │
│ └──────────────────────────────────────┘ │
│ 检测到 12 条链接，去重后 11 条  [开始下载] │
└──────────────────────────────────────────┘
```

### 风险
- **拖拽 .txt 拿文件路径**：Tauri 2 需要在 window 配置开 `dragDropEnabled: true`，且通过 `tauri://drag-drop` 事件拿路径；WebView2 的 HTML5 drop 只能拿 `File` 对象（读内容也可以，但路径拿不到）。两选一，建议走 Tauri 事件拿绝对路径再读文件。
- **URL 校验**：不要用 `URL` 构造函数严格校验（会误杀 B站短链、磁力链接等），后端用 yt-dlp 实际能认的宽松规则。

---

## F4 剪贴板监听（默认关）

**状态**：⬜ 未开始
**优先级**：P0

### 目标
复制一个链接，应用就提示「要下载吗」，省一次粘贴。

### 现状
- 已有 `api.readClipboard()`（`read_clipboard` 命令，走 `clipboard.rs`）。
- 前端 `navigator.clipboard.readText()` 在 WebView2 会卡权限，所以必须继续走这个命令。

### 方案
1. 设置项 `watchClipboard: bool`（默认 **false**，避免误触）。
2. 前端用 `setInterval`（约 800ms）+ 上次值比对，检测到新 URL 就弹一个轻量「下载？[是] [忽略]」气泡。
3. 只认 `ytdlp_core::url::is_valid_url` 通过的内容，非 URL 忽略。
4. 记录「最近忽略的 URL」哈希，短时间内同一 URL 不再弹。

### 字段清单
| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `watchClipboard` | bool | false | 是否监听剪贴板 |

### 风险
- **隐私**：监听剪贴板是敏感操作，默认必须关，且 UI 里明确写「会读取剪贴板」。
- **轮询 vs 事件**：Windows 有剪贴板监听 API，但走 Tauri 事件成本高；轮询 800ms 足够、简单、可控。不要过度设计。

---

## F5 一键重命名 / 移动成品

**状态**：⬜ 未开始
**优先级**：P0

### 目标
改文件名模板后，把已下载的文件按新模板重命名 / 移动到别的目录，不用去资源管理器手动手。

### 现状
- `Task` 已有 `filepath`、`outputDir`、`filenameTemplate`（在 `Settings` 里，不是任务级）。
- 已有 `open_file` / `reveal_file` / `delete_file` 命令，但**没有** `move_file` / `rename_file`。

### 方案
1. completed 任务详情加两个按钮：「移动到…」（原生选目录，复用 `pick_folder`）、「按模板重命名」（用当前文件名模板重新生成名字）。
2. 后端命令 `relocate_file(task_id, new_dir)`：检查源文件存在 → 目标不存在 → `std::fs::rename` → 更新 `t.filepath`、`t.outputDir` → `emit` 刷新。
3. 「按模板重命名」用 `ytdlp_core` 里生成最终文件名的逻辑（探测时已用过），对 `Task` 的 title/id/extractor 重新渲染文件名。

### 后端挂钩
- 新增 `relocate_file(task_id: String, new_dir: Option<String>, new_name: Option<String>) -> String`（返回新路径）。
- 复用 `paths` 模块的路径规范化与「目标已存在」检查。

### 风险
- **目标已存在**：绝不静默覆盖；存在时返回明确错误让用户处理。
- **下载中不能动**：只允许 completed / skipped 状态移动；`running_handle` 存在的任务拒绝。
- **归档联动**：移动文件**不**改归档（归档按 `<站点> <视频id>` 记，与文件路径无关），所以移动后重下仍会「已在归档中」——这是正确行为，但要写清楚别让用户困惑。

---

## F6 磁盘空间预估 + 不足拦截

**状态**：⬜ 未开始
**优先级**：P0

### 目标
在**开始下载前**就告诉用户「目标盘可能放不下」，而不是下到一半报磁盘满。

### 现状
- 探测后有 `sizeEstimate`（`requested_downloads` 之和，DESIGN §15.1）。
- 有 `outputDir` / `tempDir`。

### 方案
1. 出队下载前（或探测后立即），读 `outputDir` 所在盘剩余空间（`std::fs` 或 Windows API，跨平台用 `sysinfo` 或自己读）。
2. 若 `sizeEstimate` 存在且 `剩余 < sizeEstimate × 1.2`（留 20% 余量，因为合并/嵌入后更大，DESIGN §15），弹出「磁盘空间可能不足」警告，让用户选择「仍然继续」或「取消」。
3. `sizeEstimate` 为 null 时不拦截（只提示「大小未知」）。

### 字段清单
| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `diskSpaceCheck` | bool | true | 是否启用磁盘空间检查 |

### 风险
- **预估值不准**（DESIGN §15.4 已实测预估值会整块缺失或偏差），所以只做「警告」不做「硬拦截」，用户可强制继续。
- **跨盘 temp 目录**：成品落 `outputDir`，碎片落 `tempDir`，两处都要看剩余空间（尤其 `tempDir` 可能在 C 盘，常比下载盘更紧张）。

---

## F7 下载队列：暂停全部 / 恢复全部 / 拖拽排序

**状态**：⬜ 未开始
**优先级**：P1

### 目标
任务多时能「一键全停」「一键全起」，以及调整下载顺序。

### 现状
- `scheduler` 已有 per-task 的 `pause` / `resume`。
- 没有「全局暂停」和「优先级」。

### 方案
1. **全局暂停**：`pause_all()` / `resume_all()` 命令，置一个全局 flag，scheduler 出队时检查。
2. **拖拽排序**：`Task` 加 `priority: i64`（默认按 `added_at` 排序），列表拖拽改 priority，scheduler 按 priority 出队。
3. UI：状态栏加「暂停全部 / 恢复全部」；列表拖拽仅在「进行中」筛选下启用。

### 字段清单
| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `priority` | i64 | 0 | 越大越先下 |

### 风险
- **并发上限**：改 priority 不突破 `downloadConcurrency` / `perHostConcurrency`，只是改变「谁先占坑」。
- **拖拽与虚拟滚动冲突**：虚拟列表里做拖拽要额外处理行高测量，先做「上移/下移按钮」替代拖拽可大幅降复杂度。

---

## F8 定时任务 / 定时限速

**状态**：⬜ 未开始
**优先级**：P1

### 目标
「夜里自动下」「上班时自动降速」。

### 方案
1. 设置项 `schedule`：一个简单的 cron 式配置（起止时间、限速时段）。
2. 后端一个轻量定时器（复用现有 `tokio::time`），到点触发 `resume_all` / `set_limit_rate`。
3. **限速是全局的**：yt-dlp 的 `--limit-rate` 是单任务参数，批量降速要逐个任务改，或用调度器统一在生成参数时按当前时段注入限速。

### 字段清单
| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `schedule.enabled` | bool | false | 定时开关 |
| `schedule.startTime` | "HH:mm" | "23:00" | 开始自动下载 |
| `schedule.limitRate` | string | "" | 时段内限速（空=不限） |

### 风险
- **限速的注入点**：`--limit-rate` 目前是 `Settings.limitRate` 全局单值，改时段限速要在 `spec_from_settings` 里按当前时刻重算，注意别破坏现有单值逻辑。
- **睡眠/关机**：系统睡眠期间定时器不跑，醒后要补偿判断「是否已过时段」。

---

## F9 归档分 profile

**状态**：⬜ 未开始
**优先级**：P1（对应 DESIGN §16 待定项「归档范围」）

### 目标
换账号/换设备/换目录重下时，不被同一个大归档卡死。

### 现状
- `Settings.archivePath` 是单一路径，所有任务共用一个归档（DESIGN §12）。

### 方案（三选一，建议按输出目录分）
1. **按输出目录分**（推荐）：`archivePath` 缺省 = `输出目录\.ytdlp-archive.txt`，即每个目录一个归档。
2. **按 cookie profile 分**：`归档目录\<profileId>.txt`。
3. **保持单一大归档**，但 UI 提供「切到别的归档」。

决策后只需改 `runner::spec_from_settings` 里 `--download-archive` 的取值逻辑，前端 `Settings.archivePath` 语义不变（仍可手填覆盖）。

### 风险
- **迁移**：现有用户已有一个大归档，切到「按目录分」时要**不丢历史**——把旧归档按现有任务的分目录拆成多个，或保留旧归档作为「已下过」的兜底（`--download-archive` 可以传多个）。
- **跨目录重下**：分目录后，同一视频下到两个目录会各记一次，可能重复下——这正是「分目录」的语义，UI 里写清楚。

---

## F10 任务分组 / 标签

**状态**：⬜ 未开始
**优先级**：P1

### 目标
上百条任务后能快速找到某一条。

### 方案
1. **自动分组**：侧栏加「按站点 / 按输出目录 / 按上传者」分组视图（纯前端聚合，不动后端）。
2. **手打标签**：`Task` 加 `tags: Vec<String>`，可增删；搜索框支持 `#标签` 语法。
3. 复用现有 `Filter` 机制扩展，或新增 `groupBy` 状态。

### 字段清单
| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `tags` | Vec\<String\> | [] | 手打标签 |

### 风险
- **虚拟滚动**：分组视图会打破「一维列表」，需要分组头 + 组内虚拟列表；成本中等，先做「标签过滤」更省。

---

## F11 下载历史统计面板

**状态**：⬜ 未开始
**优先级**：P1

### 目标
「这周下了多少 G、下得最多的是哪个站」。

### 方案
1. 后端聚合 SQLite：总量（`sizeActual` 求和）、条数、按站点、按时间（天/周/月）分布。
2. 前端一个简单的统计页（表格 + 柱状图，可用纯 CSS/轻量图表，不引入重库）。

### 后端挂钩
- 新增命令 `stats_summary() -> StatsSummary`。
- 只统计 `completed` 且 `sizeActual` 非空的记录。

### 风险
- **隐私**：统计只在本机，不上传；`extractor` 是站点名不是敏感数据。
- **历史记录被删**：统计基于现存任务，删除记录会「回溯性改变」统计——写明这是「当前库内统计」，不是永久日志。

---

## F12 导出 / 导入任务列表

**状态**：⬜ 未开始
**优先级**：P1

### 目标
换机器、备份时能把任务（和归档）带走。

### 方案
1. 导出：把 `tasks` + `archivePath` 内容打包成一个 JSON（或 zip）。
2. 导入：读文件 → 去重（按 URL / 归档键）→ 写库 → emit。

### 字段清单
导出格式（建议）：
```json
{ "version": 1, "tasks": [ ... ], "archive": "bilibili BV...\nyoutube ...\n" }
```

### 风险
- **凭证不导出**：cookie profile 是敏感数据，默认**只导任务元数据 + 归档**，不导 cookie。
- **路径失效**：换机器后 `outputDir` / `filepath` 可能不存在，导入时给「路径不存在」提示，允许改。

---

## F13 每任务独立覆盖设置

**状态**：⬜ 未开始
**优先级**：P1

### 目标
「就这一条要 4K」「就这一条不要字幕」。

### 现状
- `Task` 已有 `formatOverride`（`-f` 表达式覆盖）为起点。

### 方案
1. 扩展现有 per-task 覆盖为结构化字段：`overrides: { outputDir?, limitRate?, embedSubs?, ... }`（可选）。
2. 详情页加「本任务单独设置」面板。
3. `spec_from_settings` 时：per-task 覆盖 > 全局设置。

### 字段清单
| 字段 | 类型 | 说明 |
|---|---|---|
| `overrides` | TaskOverrides \| null | per-task 覆盖，null=跟随全局 |

### 风险
- **与全局设置的分叉**：DESIGN §3.2 已强调「格式覆盖 vs 跟随预设」的两态；扩展成多维覆盖后要小心「部分覆盖」的语义（只覆盖限速、其余跟全局）。建议覆盖字段全部可选，`null` 表示「跟随全局」。

---

## F14 只下音频 / 字幕 / 封面

**状态**：⬜ 未开始
**优先级**：P2

### 方案
三种 preset：
- 仅音频：`-f ba/b` + `-x --audio-format <audioFormat>`（已有 `audioFormat` 设置）
- 仅字幕：`--write-subs --skip-download`
- 仅封面：`--write-thumbnail --skip-download`

UI 在「格式预设」里加三个选项。

---

## F15 章节切分

**状态**：⬜ 未开始
**优先级**：P2

### 方案
`--split-chapters`（`PostProcessKind.SplitChapters` 已存在，只差 UI 开关 + 参数）。

---

## F16 下载后动作

**状态**：⬜ 未开始
**优先级**：P2

### 方案
`--exec`，设置「完成后执行」+ 安全确认。安全：只允许用户显式填的命令，写清「会以本机权限执行」。

---

## F17 本地重复检测

**状态**：⬜ 未开始
**优先级**：P2

### 方案
下载前查库：同 `video_id` 且 `filepath` 存在且 `sizeActual` 匹配 → 提示「本地已有」。复用 §13.3 新增的 `video_id` 字段。

---

## F18 代理按站点分流

**状态**：⬜ 未开始
**优先级**：P2

### 方案
per-host 规则表（`host → proxy`），`spec_from_settings` 时按 URL host 匹配。现有 proxy 是全局，扩展成「全局 + 例外表」。

---

## F19 浏览器扩展一键推送

**状态**：⬜ 未开始
**优先级**：P2（大工程）

### 方案
一个小浏览器扩展，右键「用 yt-dlp 下载」→ 把 URL POST 到本地 app 端口（本地 HTTP server 或 Tauri 的 deep-link）。成本最高，放最后。

---

## F20 暗色主题

**状态**：⬜ 未开始
**优先级**：P2

### 方案
CSS 变量已在 `style.css` 的 `:root` 集中定义（DESIGN §9.1 定了亮色）。做「亮/暗/跟随系统」三档只需：
1. 加 `[data-theme="dark"]` 一组 CSS 变量覆盖。
2. 设置项 `theme: 'light' | 'dark' | 'system'`。
3. `main.ts` 里按设置 / 系统 `prefers-color-scheme` 切 `data-theme`。
4. `color-scheme` 属性同步切换（已声明 `light`，需改成跟随）。

---

## F21 设置页「简单 / 高级」分档

**状态**：⬜ 未开始
**优先级**：P0（顺手，立刻降门槛）

### 目标
默认只露核心设置，降低新手压力；进阶项收进「高级」。

### 现状
设置页有 200+ 处控件/说明（`SettingsPanel.vue`），偏重。

### 方案
1. 加一个 `simpleMode: bool`（默认 true）状态，或用一个「高级」折叠区。
2. **简单档只露 6 组**：
   - 输出目录 / 临时目录
   - 格式预设（最佳画质 / 1080p / 仅音频）
   - 下载并发
   - 字幕（开关 + 语言）
   - Cookie（浏览器 / 导入）
   - 代理
3. **高级档**：编码偏好、文件名模板、aria2c、归档、限速、JS 运行时、指纹模拟、通知、磁盘检查、定时任务。
4. 危险/易错项（文件名模板、归档路径、代理）加「恢复默认」按钮。

### 字段清单
| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `simpleMode` | bool | true | 是否仅显示简单档（或叫 `advancedMode` 反过来） |

### 风险
- **状态不落盘**：`simpleMode` 可以是纯前端 UI 状态（不存 config），刷新后回默认即可；若用户常开高级，可存 localStorage。
- **不要删功能**：分档只是「隐藏」，不是「移除」；高级项默认值必须与现状一致，否则老用户升级后行为变化。

---

## 附：跨功能的技术前提清单（实现前先核）

1. **通知** 需要 `tauri-plugin-notification`（Rust + npm + capability），当前未装。
2. **拖拽文件** 需要 `tauri.conf.json` 的 window 开 `dragDropEnabled`。
3. **错误分类**（F2 依赖）是 DESIGN §16 待定项，先做最小关键词分类器再上自动重试。
4. **磁盘剩余空间** 需要一个跨平台读盘 API（`sysinfo` 或自写）。
5. **定时器** 复用 `tokio::time`（项目已依赖 tokio）。
6. **优先级排序** 需要 `Task.priority` 字段 + scheduler 出队排序。
