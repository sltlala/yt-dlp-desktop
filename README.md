# yt-dlp 桌面下载工具

基于 **yt-dlp.exe（子进程）** 的桌面下载器。宿主用 **Tauri 2（Rust）**，界面用 **Vue 3 + TypeScript**。

核心决策与实测依据见 **[`DESIGN.md`](DESIGN.md)**；yt-dlp CLI 协议契约见 **[`HANDOFF.md`](HANDOFF.md)**。

---

## 为什么用 exe 而不是 Python 库

真正的收益是**解耦**：yt-dlp 因站点改版发版极频繁，作为独立 exe 时
**换一个文件就能修好全部站点问题**，宿主二进制一个字节都不用动。
若把 `yt_dlp` 编进宿主，则「B 站改了接口」和「App 版本号 +1」被绑死成同一件事。

---

## 目录结构

```
crates/ytdlp-core/     零依赖核心层：参数构造 + 输出解析（纯函数，可离线单测）
src-tauri/             Tauri 2 外壳：进程生命周期、持久化、命令面
  binaries/            随包的第三方可执行文件，见该目录下的 README-third-party.md
                       yt-dlp.exe（Unlicense）· aria2c.exe（GPLv2，附许可证文本）
src/                   Vue 3 前端
  components/          列表行 / 展开详情 / 设置抽屉 / 格式选择器
docs/screenshots/      界面截图
scripts/cdp-probe.mjs  无头浏览器诊断脚本（抓控制台错误）
scripts/cdp-attach.mjs 附着到已运行的 WebView2，执行表达式 / 抓控制台
scripts/cdp-shot.mjs   通过 CDP 截图（比 PrintWindow 可靠，见 DESIGN §9.1）
scripts/resize-window.ps1  把窗口精确调到指定 CSS 尺寸（验证最小尺寸下的布局）
```

## 开发

```bash
npm install            # 首次

# ① 桌面应用（真实 Rust 后端）—— 日常用这个
npm run tauri:dev

# ② 只调界面（浏览器 + 演示后端，不需 Rust 编译，改样式最快）
npm run dev            # http://127.0.0.1:5183

# ③ 核心层单测（零依赖，无需网络）
cargo test -p ytdlp-core

# ④ 打正式包（会走 dist/，不再依赖 dev server）
npm run tauri:build
```

### ⚠️ 不要直接双击 `target\debug\ytdlp-desktop.exe`

**debug 版通过 `devUrl` 加载前端**，必须有 vite dev server 在跑。
直接运行会得到 `ERR_CONNECTION_REFUSED` 白屏（实测）。

如果不想每次都让 Tauri CLI 重新编译，可以开两个终端：

```bash
# 终端 A
npm run dev
# 终端 B —— 复用 A 的 dev server
target\debug\ytdlp-desktop.exe
```

改 Vue 文件会热更新；改 Rust 文件需要重新 `cargo build` 或直接用 ① 。

界面支持 `?panel=settings|detail|formats|playlist` 直接打开对应视图，便于截图与视觉回归。

### ⚠️ 不要在 `tauri:dev` 运行时跑 `cargo` 命令

`tauri dev` 会**监视文件变化并自动重编译**。此时另开一个终端跑 `cargo test` /
`cargo build` 会和它争用 `target/`，出现莫名其妙的链接错误
（实测 `link.exe exit code 1120`）。**先停掉 dev，再跑 cargo。**

### 关于 yt-dlp 的打包形态（重要）

yt-dlp 有两种发行形态，**只有一种能直接放进 `externalBin`**：

| 形态 | 结构 | 来源 |
|---|---|---|
| **onefile** | 单个 `.exe`（约 17.8 MB） | ✅ 官方 GitHub Releases 的 `yt-dlp.exe` |
| **onedir** | `.exe`（8 MB）+ 同级 `_internal\`（21.8 MB） | winget / scoop 等包管理器常给这种 |

`externalBin` **只复制那一个文件**。把 onedir 的 exe 复制走会得到一个跑不起来的
残缺副本（`Failed to load Python DLL ..._internal\python310.dll`），
而它落在查找顺序里**优先于 PATH**，会遮蔽掉系统上完好的安装。

所以 `src-tauri/binaries/` 里必须是**官方 onefile 版**。
`paths::resolve_ytdlp()` 会实际跑 `--version` 验证每个候选，跳过不能用的，
并缓存结果；`paths::diagnose_ytdlp()` 可列出每个候选的状态便于排查。

## 关键设计约束（改动前请先读）

| 约束 | 原因 |
|---|---|
| **不能用 `--print`**，改用 `--print-to-file` | `--print` 隐含 `--quiet`，会把进度输出整个压掉（实测 7 条 → 0 条） |
| **temp 目录必须按 `task_id` 稳定推导** | 否则恢复时找不到 `.part`，断点续传静默失效 |
| **存 `-f` 表达式，不存 `format_id`** | 任务可重放、无需持久化 info.json |
| **启用嵌入时容器切 MKV** | WebM 上 `--embed-thumbnail` 直接报错；mp4 的缩略图走三级回落 |
| **`skipped` 必须独立于 `completed`** | 归档命中与文件已存在**都返回 exit=0**，混淆会让 UI 把「没下」显示成「下完了」 |
| **删除拆成三个独立动作** | 「移除记录」「从归档移除」「删除文件」语义不同，合成一个会让用户以为程序坏了 |
| **不用 `sidecar()` 启动** | 它的 kill 不递归，会漏杀 ffmpeg 子进程并占住文件句柄 |
| **`CommandChild.kill` 之外仍需进程树终止** | 同上；见 `runner::kill_tree` |
| **新用到的核心 API 必须在 `capabilities/` 声明权限** | Tauri 2 未声明的 `core:*` 权限会**直接拒绝**；若调用点没接住 rejection，功能会静默失效（详见下） |
| **亮色主题必须声明 `color-scheme: light`** | 不声明的话，深色系统下滚动条 / `<select>` / checkbox 这些原生控件仍是深色，白底上非常突兀（DESIGN §9.1） |
| **路径截断别用 `direction: rtl`，也别只靠 CSS `ellipsis`** | 前者会把结尾的 `\` 挪到最前面（`\E:\下载\视频`）；后者砍掉扩展名与 `[视频id]` | 用 `utils.ellipsizePath()` 做中间截断 |
| **DASH 站点音视频是分开的两条轨** | 单点一条视频轨会下出**没有声音**的视频 | 仅视频轨翻译成 `<id>+ba/<id>`；界面必须标出「视频+音频 / 仅视频 / 仅音频」（DESIGN §3.1） |
| **storyboard 不能用「两路编码都缺」来判** | generic 提取器给直链文件时也是 `vcodec: "none"`，会误伤 | 判据用 `ext == "mhtml"` |
| **单击 + 双击要做两件事** | 双击会先触发两次 `click`，直接实现会「展开又收起」闪一下 | 用 `event.detail > 1` 吞掉第二下（遵循系统双击间隔）；详见 DESIGN §9.4 |
| **别用 `cmd /C start` 或 `explorer.exe <path>` 打开文件** | 前者引号与 `&` 打架，后者把 `,` 当参数分隔符 | `shell.rs` 走 `ShellExecuteW`，不引额外 crate |
| **目录选择不能用 `<input type="file" webkitdirectory>`** | 它只给相对路径，`File.path` 是 Electron 才有的 | 用 `rfd` 弹原生对话框；命令要 `spawn_blocking`（模态对话框会卡住主线程消息循环） |
| **模板里缺 `%(ext)s` 不会自动补扩展名** | 实测 `-o "%(title)s"` 得到的文件就叫 `clip`，没有后缀 | 界面对缺 `%(ext)s` 的模板直接警告（DESIGN §9.7） |
| **输出模板字段名拼错不报错** | yt-dlp 静默填 `NA`（`%(titel)s` → `NA.mp4`） | 把模板里的字段名与已知清单比对并提示 |
| **`no_proxy` 在给了 `--proxy` 时不起作用** | 死代理 + `no_proxy=127.0.0.1` 访问本地服务依然连不上 | 绕过列表由宿主判断：命中就不传 `--proxy`（DESIGN §7.1） |
| **`--downloader` 传存在的绝对路径会被静默忽略** | 不报错，直接回落到内置下载器——以为在用 aria2c，其实没有 | 只传裸名 `aria2c`，把自带目录**前置到子进程 PATH**（DESIGN §11.6） |
| **aria2c 不在 PATH 时 yt-dlp 也是静默回落** | 同上，完全没有提示 | 宿主自己先验证 aria2c，找不到就关掉该选项并在任务里告警 |
| **aria2c 是 GPLv2，yt-dlp 是 Unlicense** | 两者许可证不同，随包分发必须一并给许可证原文与源码出处 | 许可证文本进 `bundle.resources`，见 `binaries/README-third-party.md` |
| **`-S`/`--format-sort` 是整体替换默认排序，不是追加** | `-S acodec:aac` 之后分辨率不再参与比较，1080p 的 `137` 会输给 360p 的 `18`——只想换个音频编码，画质静默塌掉 | 编码偏好用 `-f` 过滤器实现（`bv*[vcodec^=avc1]+ba[acodec^=mp4a]/…`），默认排序原封不动（DESIGN §3.3） |
| **编码名要用 yt-dlp 报出的那套** | `h264`、`aac` 这两个「熟悉的叫法」匹配不上任何东西，且**不报错**，只是静默退化成没有偏好 | 写 `avc1`（实际报 `avc1.640028`）、`mp4a`（实际报 `mp4a.40.2`）；选择项由后端白名单生成 |
| **非法的 `-f` 过滤器让 yt-dlp 直接崩** | `[vcodec@=x]`、`[vcodec^=]`、`~=` 都会抛 `SyntaxError: Invalid filter specification` + Python traceback，非零退出 | 任何进 `-f` 的用户输入都过白名单（`CodecPreference::sanitized`） |
| **归档文件有 BOM 时首行静默失效** | yt-dlp 用 `encoding='utf-8'` 读归档，**不认 BOM**；首行比对失败 → 该视频被重新下载。后面几行正常，所以很难发现 | 宿主读写归档都去 BOM、写回不写 BOM（DESIGN §12.1）；手工修归档别用 `Out-File -Encoding utf8` |
| **Windows PowerShell 5.1 的 `Get-Content -Raw` 按 ANSI 读 UTF-8** | 读改写一次就把整个文件的中文变成乱码，且**不可逆** | 改文件一律用编辑工具；确要用脚本时显式 `[System.IO.File]::ReadAllText($p, [Text.Encoding]::UTF8)` |
| **`Out-File -Encoding utf8` 在 5.1 里写 BOM** | 不只是编码问题：把 JSON 发给 GitHub API 会 `Problems parsing JSON`，给 yt-dlp 的归档会废掉首行 | 生成给机器读的文件用 `[System.IO.File]::WriteAllText($p, $s, [Text.UTF8Encoding]::new($false))` |

### 关于 capabilities（踩过的坑）

Tauri 2 的 `core:event:allow-listen` 等核心权限**必须在
`src-tauri/capabilities/*.json` 里显式声明**，否则调用会被拒绝：

```
event.listen not allowed.
Permissions associated with this command: core:event:allow-listen, core:event:default
```

`capabilities/` 是**编译期**生成的，改完要重启 `tauri:dev`（文件监视不一定会捕捉到）。

更糟的是失败方式：如果调用点没 `await`/`catch`，就变成一条看不见的
unhandled rejection——表现是「**粘贴链接后添加任务毫无反应**」。
所以 `src/stores/tasks.ts` 现在把错误存进 `lastError` 并在界面上显示横幅，
事件通道不可用时还会**降级为轮询**。

## 打包与发布

**打一个 `v*` 的 tag 就会自动出 Release**（`.github/workflows/release.yml`）：

```bash
# 先把 tauri.conf.json 里的 version 改好，然后
git tag v0.1.0
git push origin v0.1.0
```

流程会跑测试 + 类型检查，再用 `tauri-apps/tauri-action` 构建 NSIS 安装包并挂到
Release 上。**默认出草稿**——自动打出来的包人工确认过再公开比较稳妥。

也可以到 Actions 页面手动触发一次（`workflow_dispatch`）。

> 安装包没有代码签名证书，Windows 会提示「未知发布者」，点「仍要运行」即可。

## 许可证

本仓库**自己的代码**采用 **GPL-3.0**（见 [`LICENSE`](LICENSE)）。

随包分发的第三方程序**各自独立**，不是本作品的一部分：

| 程序 | 版本 | 许可证 | 源码 |
|---|---|---|---|
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | 2026.07.04 | Unlicense（公有领域） | 同左 |
| [aria2](https://github.com/aria2/aria2) | 1.37.0 | **GPL-2.0** | 同左 |

aria2c 的许可证原文随安装包一起分发（`bundle.resources`），
详见 [`src-tauri/binaries/README-third-party.md`](src-tauri/binaries/README-third-party.md)。

## 已验证的环境

yt-dlp `2026.07.04` · ffmpeg 8.0.1 · aria2c 1.37.0 · Rust 1.95 · Node 22.23.3 · .NET 10（未使用）
