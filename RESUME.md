# 项目状态

> **先读 `DESIGN.md`**（架构与实测依据）和 **`HANDOFF.md`**（yt-dlp CLI 协议契约）。
> 本文记「做完了什么、还剩什么、有哪些坑」。

---

## 目标完成情况

原目标「把 `DESIGN.md` 中已设计但未实现的功能做完」——**8 项全部完成**。

| # | 功能 | 实现位置 | 验证 |
|---|---|---|---|
| 1 | **元数据探测** | `crates/ytdlp-core/src/probe.rs` | 9 项单测 + 端到端 |
| 2 | **探测/下载双并发池 + 每域名限流** | `crates/ytdlp-core/src/url.rs`、`src-tauri/src/scheduler.rs` | 10 项单测 + 端到端 |
| 3 | **Cookie 多 profile + 预检** | `crates/ytdlp-core/src/cookies.rs`、`src-tauri/src/cookies.rs` | 23 项单测 + **真实预检** |
| 4 | **播放列表勾选** | `PlaylistPicker.vue`、`--playlist-items` | 实测 |
| 5 | **aria2c 进度解析** | `crates/ytdlp-core/src/parse.rs` | 8 项单测 + 端到端 |
| 6 | **yt-dlp 自更新** | `src-tauri/src/update.rs` | 6 项单测 + **真实替换** |
| 7 | **虚拟滚动** | `src/composables/useVirtualList.ts` | 实测（120 任务 → 渲染 12 行） |
| 8 | **SQLite 持久化** | `src-tauri/src/store.rs` | 6 项单测 + 重启恢复 |

**测试：核心 114 + 外壳 39 = 153 项全过**；`vue-tsc` 干净；`cargo build` 无警告。

### 目标之外的追加

| 功能 | 说明 |
|---|---|
| **自定义浏览器 + profile 选择** | 后端按 yt-dlp 的路径映射探测本机装了哪些浏览器、各有哪些 profile（按最近使用排序），界面不再写死三个；支持 `BROWSER[+KEYRING][:PROFILE][::CONTAINER]` 完整语法 |
| **JS 运行时（`--js-runtimes`）** | 见下方「第二个诊断教训」——这是应用能不能下 YouTube 的**开关**，默认自动检测并传入 |
| **控制台输出解码兜底** | yt-dlp 按系统代码页（GBK）写 stderr 且无视 `PYTHONIOENCODING`，宿主改为「先 UTF-8、失败按 GBK」（详见 HANDOFF §3.7 的更正） |
| **流读取不再要求合法 UTF-8** | 原来用 `BufReader::lines()`，一行 GBK 就让整个读取循环终止、任务之后的事件全丢 |
| **设置结构自动补齐** | 新增设置键后，老 `config.json` 里没有该键，前端 `s.jsRuntime.trim()` 抛 TypeError → **整个设置面板白屏**。现在 `normalize_settings` 按默认值递归补齐（`merge_defaults`），不再逐个特判 |
| **「临时目录」设置真的生效了** | 之前这个输入框**没有任何作用**——`spec_from_settings` 一直写死用 AppData。现在 `paths::temp_root` 读设置；把 temp 放到输出目录同一个卷上，合并/嵌入后不必再整文件复制一遍（4K 视频动辄几个 GB）。删除记录/批量删除/启动清理都会同时扫默认根与配置根 |
| **temp 根不再堆积垃圾** | 终态任务（`completed`/`skipped`）的 temp 目录在启动时回收——成品已落输出目录，`filepath.txt` 里的路径早就存进 `Task::filepath`，删掉不影响「删除文件」。散落的非任务文件也一并清掉 |
| **亮色主题** | 全部颜色集中在 `src/style.css` 的 `:root`，组件只引用变量。主区纯白 + 侧栏极浅灰，层次靠 1px 边框与极浅投影。三处容易漏的见 DESIGN §9.1（`color-scheme`、语义色压暗、站点标识色压暗） |
| **设置改成主区页面** | 侧栏导航分两组（筛选 / 设置，中间一条分隔线），点「设置」整个主区换成设置页。抽屉盖住的那半边窗口完全浪费，而且同级导航目标不该用两种交互。底部操作条改成「返回任务 / 还原改动 / 保存设置」——做成页面后用户会改一半就想走 |
| **多选工具条移到列表上方** | 顶栏「多选」→ 添加栏那一排换成工具条（已选 N 项 / 全选 / 反选 / 清空 / 移除所选记录 / 完成）。侧栏底部的「批量选择」删掉。点行体 = 勾选（不是展开），多选时隐藏行内操作；换筛选或进设置页会清空选择。按钮文案改成「移除所选记录」——它不删成品文件，用「删除」命名就是 §13 说的语义混淆 |
| **字号阶梯 + 窗口默认变小** | 字号集中到 `src/style.css` 的 `--fs-*`，组件不再写死 px；一律取**偶数**（12/14/16/18/20），因为 125% 缩放下半像素字号会让中文笔画粗细不均。默认窗口 `1440x920` → `1180x760`（原尺寸在 1536x816 的屏幕上底部跑出屏幕外）。见 DESIGN §9.5 |
| **单击展开、双击打开文件** | 用 `event.detail` 吞掉双击的第二下——单击零延迟、双击不闪。双击挂 `.main-line`（不是 `.row`，否则双击路径文字也会开文件）；多选模式下禁用。详情里另给「打开文件 / 打开所在文件夹」按钮（双击不可发现）。见 DESIGN §9.4 |
| **「可用格式」真的能选了** | 之前那张表**只是查看器**：选了行、点了确定，什么都不会发生（`Task.format_expression` 压根没参与拼参数）。现在选中行会翻译成 `-f` 表达式、写进任务、并立刻按新格式重下。见 DESIGN §3.1 / §3.2 |
| **路径字段支持选文件夹 + 手填** | 输出/临时/归档/cookies 四个路径都改成「输入框 + 浏览… + 打开(定位)」。原生对话框用 `rfd`（不引 `tauri-plugin-dialog`），命令走 `spawn_blocking` 免得卡住消息循环。另有即时形态校验（相对路径 / 把文件填进目录栏）。见 DESIGN §9.6 |
| **文件名模板给了字段清单 + 预览** | 30 个常用字段（可点击插到光标处，按 4 组分类）+ 6 个预设模板 + 用**真实任务标题**渲染的实时预览。两条实测警告：缺 `%(ext)s` 会真的没有扩展名；字段拼错会静默变 `NA`。见 DESIGN §9.7 |
| **代理设置照 Windows 那一页重做** | 三选一（不使用 / 跟随系统 / 手动）；手动有 HTTP·SOCKS5、主机端口、绕过列表、身份验证（含「记住密码」）。跟随系统走 `reg query` 读注册表。**绕过列表由宿主判断**（实测 `no_proxy` 在给了 `--proxy` 时不起作用），命中就不传 `--proxy`。旧配置自动迁移。见 DESIGN §7 |
| **aria2c 随包分发** | 与 yt-dlp 同一套查找顺序（随包 → AppData → PATH），每个候选跑 `--version` 验证。设置页「下载器」标签显示当前用的是哪一份。**许可证不同**（aria2c 是 GPLv2），许可证文本随包。见 DESIGN §11.5 |
| **两个静默失败点被堵住** | 实测 `--downloader <存在的绝对路径>` 会被 yt-dlp **静默忽略**（不报错、回落内置）；aria2c 不在 PATH 时同样是静默回落。对策：只传裸名 + 把自带目录前置到子进程 PATH + 宿主自己先验证，找不到就关掉并在任务里告警。见 DESIGN §11.6 |
| **Cookie 选择简化** | 删掉「手动填写完整参数」输入框（浏览器 + 配置文件两个下拉已覆盖日常所需）。Windows 的 Chromium 系读取风险**只在选中相关浏览器时**提示，依据是后端下发的 `chromium` 字段。见 DESIGN §6.3 |
| **DASH 音视频分开的处理** | 仅视频轨自动配最佳音频轨（`137+ba/137`），已封装的不再画蛇添足追加 `+ba`；表里加「视频+音频 / 仅视频 / 仅音频」三个色标并重排（有画面的在前、按分辨率降序、音轨排最后） |
| **`scripts/cdp-shot.mjs`** | 用 CDP 的 `Page.captureScreenshot` 截图。原来的 `PrintWindow` 方案会抓到合成层残影（文字重影，看着像渲染 bug） |

### 本轮端到端验证（都是真跑出来的，不是推断）

**1. YouTube 探测与下载（`tW34TyACBIQ`）** —— 应用实际派发的命令行：

```
yt-dlp.exe --dump-single-json --skip-download ... --js-runtimes node --js-runtimes bun \
  --proxy http://127.0.0.1:7897 --cookies-from-browser firefox -- <url>
```

结果：标题与 11 个格式填上、状态越过 `probing` → `downloading` → `completed`，
输出 `(4K) ... [tW34TyACBIQ].mp4`（67.2 MB，嵌入字幕/缩略图/元数据，容器 auto→mp4）。
中途重启过一次应用，`.part` 续传成功（28.9 MB → 完成）。

**2. 归档跳过**（重新添加同一个链接）：状态 `skipped` 而非 `completed`，标题与格式正常，
不再下载 —— DESIGN §12.1 语义成立。

**3. 本地 HTTP 回环测试**（`_scratch/rangeserver.py` + ffmpeg 生成的 3 秒短片）——
用来验证参数拼装，不打扰 YouTube：

| 设置 | 命令行里的体现 |
|---|---|
| `proxyEnabled=false` | ✅ 不再出现 `--proxy` |
| `archiveEnabled=false` | ✅ 不再出现 `--download-archive` |
| `aria2c=true` | ✅ `--external-downloader aria2c --downloader dash,m3u8:native` |
| `tempDir=E:\下载\视频\tmp\` | ✅ `--paths temp:E:\下载\视频\tmp\t-<id>` |

**4. temp 清理**：终态任务的 temp 目录在下次启动时被回收，两个根都清空。

### 关键实测结论（选摘，完整见 DESIGN.md）

- **aria2c 让 yt-dlp 一个进度都不发**（实测 0 条）。解法是解析 aria2c 自身的
  `*** Download Progress Summary ***`（`--summary-interval=1 --enable-color=false`），
  再映射成同一个 `Progress` 结构，界面无需改动。
- **yt-dlp 有两种打包形态**：onefile（官方发布）与 onedir（`exe` + `_internal/`）。
  `externalBin` 只复制单文件，onedir 形态被复制后会失效并**遮蔽** PATH 上完好的安装。
- **GitHub API 有速率配额**（实测撞 403），共享代理出口 IP 很容易被耗尽。
  更新的下载路径**不依赖 API**，直接走 `/releases/latest/download/<asset>`。

---

## 两次诊断教训（都值得记住）

### 一：别把「时间上先后发生」当成因果

**现象**：用户命令行能下 YouTube，应用不能。

**我犯的错**：连续做了十几次探测来「对比版本」，中途看到
「2026.07.04 成功 3/3、2026.08.19 失败 3/3」就下了「版本回归」的结论，
还据此替换了出厂副本。**这个结论证据不足**：那批测试是在累计请求更多之后跑的，
而继续测下去之后，**两个版本、所有参数组合全都失败了**。

**教训**：
- 诊断间歇性失败时，**别把「时间上先后发生」当成因果**。测试本身会改变被观测对象
  （这里是把配额耗尽），越是多测越容易得出错误结论。
- 「版本 A 全成、版本 B 全败」这种完美二分，在**样本只有 3 次**且**测试顺序与时间相关**时
  毫无统计效力。
- 停下比继续测更有价值。当时的正确动作是：先歇几分钟，再各测一次。

**代码侧仍有的改进**：`classify_error` 原来一口咬定「是机房 IP」，
现在改成列出三种可能（限流 / 机房 IP / 会话异常）按经验排序——
面对无法确证的原因，提示应当诚实而不是自信。

### 二：真正的根因是缺 `--js-runtimes`（用户提示后才发现）

用户指出「记得还需要设置 `[jsc:node] Solving JS challenges using node`」，这才是真因。

**证据**：便携版目录里的 `yt-dlp.conf` 写着

```
--js-runtimes node
--remote-components ejs:npm
```

而应用自己拼的参数**一个都没有**。同一份 yt-dlp、同一个链接、同样的 cookie 与代理，
只差这些参数：

| 参数 | 结果 |
|---|---|
| 无 | ❌ `needs to be reloaded`，16s |
| `--js-runtimes node` | ✅ 29s |
| `+ --remote-components ejs:npm` | ✅ 69s（慢 40 秒，**默认不开**） |

YouTube 的 n-sig / player 挑战需要 JS 运行时；`--no-js-runtimes` 的帮助文本提到
有 "defaults"，但 yt-dlp **不会自动启用系统上已安装的运行时**。解不了挑战时站点会降级
返回（只给 storyboard，或要求重新登录），报错信息则指向「没有可用格式」，
**完全不会提示你缺 JS 运行时**——这就是它难查的原因。

**落点**：`ytdlp-core::args::JsRuntimeOptions` + `push_js_args`，
`paths::detect_js_runtimes()` 在 PATH 上探测 `node/deno/bun/quickjs`，
`runner::js_of()` 统一给探测与下载两处取值。设置页「下载器」标签里有检测结果与手工覆盖。

**复盘**：第一轮教训是对的（我确实不该从 3 次样本推因果），但我停在了
「大概是限流」这种**不可证伪的归因**上就没再往下挖。正确的下一步不是「歇一会儿再测」，
而是**去看能跑通的那条路径（用户的命令行）到底多了什么**——
`yt-dlp.conf` 一直躺在那儿，一读就知道。

---

## 已知限制与后续可做

- **无 JSON → SQLite 迁移**：早期版本的 `tasks.json` 不会被导入。
  当前是开发阶段，直接删掉旧文件即可。
- **播放列表只支持选集，不支持中途改选**：选定后要改需重新添加链接。
- **`--embed-subs` 的告警只在任务详情里展示**，没有全局提示。
- **未做**：国际化、快捷键、拖拽添加、剪贴板监听、字幕/弹幕的细粒度控制。

---

## 现场与踩过的坑

### 运行

```bash
npm install
npm run tauri:dev        # 桌面应用（真实 Rust 后端）
npm run dev              # 只调界面（浏览器 + 演示后端）
cargo test               # 核心层 + 外壳
```

调试：`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9334`
配合 `node scripts/cdp-attach.mjs 9334 "@脚本.js"` 直连 WebView2 抓控制台、调命令。

### 坑（都踩过，别再踩）

| 坑 | 表现 | 应对 |
|---|---|---|
| **`tauri:dev` 运行时跑 cargo** | `link.exe exit 1120` | 先停 dev 再跑 cargo |
| **PowerShell 是 5.1 且 DPI-unaware** | `GetWindowRect` 返回虚拟化坐标，截图**静默裁掉右侧**，看起来像布局 bug | 用 `scripts/capture-window.ps1`（先 `SetProcessDPIAware` + `PrintWindow`） |
| **PS 5.1 按 ANSI 读 BOM-less UTF-8** | 脚本里的中文注释会让解析失败，报错完全指不到原因 | `.ps1` 一律纯 ASCII 注释 |
| **`Add-Type` 里的中文注释** | "缺少 using 指令" 之类莫名其妙的编译错 | C# 代码块内不写中文 |
| **`Start-Process -ArgumentList` 数组** | 带空格的参数被拆开，yt-dlp 报 `no such option` | 传带内嵌引号的整串 |
| **`tauri-build` 的占位 `msvcrt.lib`** | 82 字节假库污染同 workspace 的 doctest 链接 | `ytdlp-core` 已设 `doctest = false` |
| **crates.io / GitHub 直连超时** | cargo 拉不到依赖、更新失败 | 走代理 `http://127.0.0.1:7897`（`HTTP_PROXY`/`HTTPS_PROXY`） |
| **Vue 的 `reactive` 是 Proxy** | `structuredClone` 抛 `DataCloneError`，组件静默不渲染 | 用 `utils.deepClone` |
| **cargo 测试里 `set_var`** | 并行测试互相踩 `APPDATA` | 用互斥锁串行化（见 `cookies.rs` 测试） |
| **`PrintWindow` 截 WebView2** | 抓到合成层上一帧的残影，文字重影，**看着像渲染 bug 其实不是** | 用 `scripts/cdp-shot.mjs`（CDP 截图） |
| **`Get-Process ... MainWindowHandle`** | Tauri/WebView2 下可能指向一个 18×18 的辅助窗口，截图静默变成一片空白 | `capture-window.ps1` 已改为枚举窗口取**面积最大**的那个 |
| **亮色主题 + 深色系统** | 滚动条 / `<select>` / checkbox 仍是深色 | 声明 `color-scheme: light`（DESIGN §9.1） |
| **在 Vue 模板里写 Markdown** | `**加粗**` 会**原样显示**（实测设置页「嵌入」标签里就有一处，一直没人注意） | 模板里用 `<strong>`；改完随手在页面上搜一遍 `**`/`` ` `` |
| **改了 `tauri.conf.json` 的窗口尺寸却没反应** | `lib.rs` 的 `setup()` 里硬编码了 `1440x920` 覆盖掉配置，改 conf 完全无效 | 窗口尺寸的唯一真源是 conf；`setup()` 只读 `inner_size()` 往下夹，不写常量（DESIGN §9.5） |
| **`-webkit-font-smoothing: antialiased`** | Windows 上会把 ClearType 换成灰度抗锯齿，深色字压白底更糊 | 不要设这两行（它们只对 macOS 有意义） |
| **用 `direction: rtl` 做「从左边截断」** | bidi 算法会把结尾的中性字符挪到最前面：`E:\下载\视频\` 显示成 `\E:\下载\视频`，看着像路径本身坏了 | 截断放 JS 里做（`shortenPath` / `ellipsizePath`），结果可预测 |
| **用 CSS `text-overflow: ellipsis` 截路径** | 它砍结尾，而下载文件名恰恰是**结尾**最能说明问题（扩展名、`[视频id]`）——`…(4K) 🖤 검스 VS 살스` 既看不出格式也看不出是哪一集 | 用 `ellipsizePath()` 做中间截断，头尾都保留 |
| **用「两路编码都缺」判 storyboard** | generic 提取器给直链文件时也是 `vcodec: "none", acodec: null`，那样会把**能下的文件**滤掉 | 判据用 `ext == "mhtml"`（或 `format_note` 含 storyboard） |
| **前后端各写一份格式分类** | Rust 说「两路都缺 = 仅视频」、TS 说「= 仅音频」，同一行两处结论不同 | 分类只在 `ytdlp_core::probe::format_kind` 定义，TS 侧注明必须逐字对齐；`probe_formats` 直接返回解析后的结构，不再让前端做 snake→camel 字段映射 |
| **`gh api` / `gh run` 在本机走代理时全报 `EOF`** | GET、POST 都失败，但同样的请求用 `Invoke-RestMethod` 全通 | 建仓库、查运行状态改用 `Invoke-RestMethod` + `gh auth token`；`git push` 不受影响 |
| **`gh repo create` 删不掉仓库** | 默认 scope 只有 `repo`/`workflow`/`gist`/`read:org`，删除返回 403 | 误建的仓库要么网页手动删，要么 `gh auth refresh -s delete_repo` |
| **右键「检查」会让 CDP 工具接错目标** | 页面里右键 →「检查」开出一个 `devtools://` 的 page 目标；`cdp-attach.mjs` / `cdp-shot.mjs` 取「第一个 page」，于是把工具接到 DevTools 自己身上——报的错完全指不到原因（表达式语法明明是对的） | 两个脚本都改成**优先挑非 `devtools://` 的页面** |
| **WebView2 默认弹浏览器的右键菜单** | 桌面应用里冒出「返回 / 刷新 / 另存为 / 打印 / 检查」。**放行「有选中文字」或「输入框」都会带出整份浏览器菜单**（表情符号 / 导入密码 / 书写方向）——放行原生等于放行全部，没有中间态 | 一律压掉，自己实现剪切/复制/粘贴/全选——DESIGN §9.0 |
| **`navigator.clipboard.readText()` 在 WebView2 里会挂住** | 它等在 `edge://permission-request-dialog/` 上，没人点就永远不返回（`execCommand('paste')` 更是恒 false）。所以「粘贴」前端做不了 | 走后端 `read_clipboard`（Win32 FFI，不引 crate）；**写**不用，`writeText` 有用户手势就能用 |
| **用 `dispatchEvent` 测剪贴板会误判成「坏了」** | 合成事件是**不可信**的，没有 user activation，`writeText`/`execCommand('copy')` 都拒绝；文档不聚焦时还报 `Document is not focused` | 用 CDP `Input.dispatchMouseEvent` 发真实点击 + `scripts/focus-window.ps1` 置前，再用 `Get-Clipboard` 断言 |
| **id 只取毫秒时间戳会撞** | `save()`/`add_task()` 都曾用 `now_ms()` 直接当 id：同一毫秒内连续两次就撞。**后果不是重复而是覆盖**——后一份 cookie 文件顶掉前一份、后一条任务顶掉前一条（粘贴多行链接最容易中招）。CI 上 `keeps_multiple_profiles` 因此偶发失败 | 统一走 `state::unique_id(prefix)`（毫秒 + 进程内自增序号）；补了「同一毫秒连存 50 份」和「连续取 2000 个 id」两条确定性回归 |
| **Tauri 产物在仓库根的 `target/`，不是 `src-tauri/target/`** | cargo workspace 的 target 目录在根上。CI 里按 `src-tauri/target/release/bundle/nsis/*.exe` 找安装包会**静默匹配不到**——白跑一次完整构建才发现 | 路径写 `target/release/bundle/nsis/*.exe`；`upload-artifact` 一律配 `if-no-files-found: error`，让路径写错当场变红 |
| **`Out-File -Encoding utf8` 会写 BOM** | 拿去当 JSON 请求体，GitHub 回 `Problems parsing JSON` | 用 `gh api -f key=value` 构造，或 `[Text.Encoding]::UTF8.GetBytes()` 传字节 |
| **归档文件带 BOM 时首行静默失效** | yt-dlp 用 `encoding='utf-8'` 读归档，**不认 BOM**：首行比对不上 → 该视频重新下载；**后面几行正常**，所以看着像归档在工作。实测 `generic clip` 命中跳过，`\ufeffgeneric clip` 输出 `clip`（没命中） | `remove_from_archive` 读时 `strip_prefix('\u{feff}')`、写回不写 BOM（DESIGN §12.1）；`strip_archive_ids` 有单测钉住 |
| **`-S` 做编码偏好会连分辨率一起丢掉** | `-S` **整体替换**默认排序，不是追加；`-S acodec:aac` + `-f bv*+ba/b` 实测选中 360p 的 `18`（本该 1080p）——用户只想换音频编码，画质静默塌掉 | 改用 `-f` 过滤器链（DESIGN §3.3），`preset_expression_with` 有单测，生成结果逐条对 yt-dlp 实测过 |
| **`h264` / `aac` 匹配不上 yt-dlp 的任何格式** | 实测 `[vcodec^=h264]`、`[acodec^=aac]` 都静默落空，得写 `avc1` / `mp4a`（实际报 `avc1.640028` / `mp4a.40.2`）；不报错，只是没有偏好 | 选择项由 `codec_choices` 从 Rust 白名单生成，前端不另写一份 |
| **非法 `-f` 过滤器让 yt-dlp 直接崩** | `[vcodec@=x]`、`[vcodec^=]`、`~=` 都抛 `SyntaxError: Invalid filter specification` + Python traceback | `CodecPreference::sanitized` 白名单把关，界面值不进 `-f` |
| **带 `-f` 的探测会整体失败** | 表达式不可满足时 `--dump-single-json -f ...` 直接 exit=1、**连 JSON 都没有**，元数据和格式表一起丢 | `run_probe` 带 `-f` 失败后**不带 `-f` 重探一次**；只有确认是 format unavailable 才告警（DESIGN §15.2） |
| **合并选择的体积记在 `filesize_approx`** | 实测 `-f 137+140`（两条都有精确 `filesize`）合计仍只在 `filesize_approx`；单条选择才给 `filesize` | 两个都认、精确值优先；全未知给 `None` 而非 0 |
| **YouTube 同一链接的格式集会变** | 连续探测：一次 11 条 HLS（**全无体积**），一次 5 条含 DASH（`filesize_approx: 16637201`）。所以预估值可能整块缺失 | 界面显示「—」是正常结果，不是 bug；不要为此加兜底数字 |

## 测试资源

- `_scratch/rangeserver.py`：支持 Range 的本地 HTTP 服务器。
  `python _scratch/rangeserver.py <port> <dir>`。
  支持 Range 很关键——aria2c 默认 `-x16`，对不支持 Range 的服务器会**直接失败**。
- `scripts/cdp-probe.mjs`：自己起 Edge，抓控制台。
- `scripts/cdp-attach.mjs`：附着到已存在的调试目标（Tauri 的 WebView2）。
