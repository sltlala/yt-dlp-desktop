# yt-dlp 桌面下载工具 — 设计与调研交接

> 来源会话：`session-52f2e36f-e736-4f28-898c-d1f9a763c5bd`（工作区 `D:\Python_Files\DSH`）
> 本文件是跨工作区的上下文载体。新会话读完本文即可接续，无需回溯原对话。
> 所有标注 ✅实测 的结论均在 yt-dlp **2026.07.04**（Windows）上现场验证过。

---

## 0. 当前状态

- **阶段**：方案调研与协议验证已完成，**尚未写任何应用代码**。`D:\Python_Files\yt-dlp_UI` 目前只有本文件。
- **已确定**：采用 **宿主程序 + yt-dlp.exe sidecar（子进程 CLI）** 方案，放弃把 `yt_dlp` 作为 Python 依赖库编入宿主程序。
- **已确定（后续会话）**：宿主 **Tauri 2**（Rust）+ 前端 **Vue 3 + TS + Vite**；UI 为列表为主 + 行内缩略图。
  完整决策见 **`DESIGN.md`**（本文只保留「调研与协议验证」，架构决策一律以 DESIGN.md 为准）。
- **本机环境**：yt-dlp.exe `2026.07.04`（`D:\Download_software\yt-dlp\yt-dlp.exe`）、ffmpeg（`D:\Program_software\ffmpeg\bin\`）、Python 3.10 且 `yt_dlp` 库同版本、cargo 1.95.0、node 22.23.3、dotnet 10.0.400、**无 Go**。

---

## 1. 为什么选 exe sidecar

核心收益**不是**「方便升级」，而是：**宿主程序不必重新打包就能修复站点失效**。yt-dlp 因站点改版高频发版，若编入宿主程序则每次都要重建 + 重签名 + 重发版。

代价（必须在设计里正视）：

| 失去的能力 | 替代手段 |
|---|---|
| `YoutubeDL` 对象复用 | 见 §5「常驻进程」可选优化 |
| 自定义 `progress_hooks` | `--progress-template` + `--progress-delta` ✅ |
| 自定义 postprocessor | 解析 stderr 的 `[<PPKey>] <msg>` 标记 ✅ |
| 同进程零启动开销 | 无法消除，只能缓解（onefile 解压 0.3–1s） |

---

## 2. 宿主语言选型

内核是独立进程后，Python 不再是必需品。

| 方案 | 体积 | 评价 |
|---|---|---|
| **Tauri 2 (Rust)** | 10–20 MB | ⭐ 跨平台首选；`tokio::process` 流式读子进程最干净 |
| **C# / .NET 10 + WinUI 3** | 40–70 MB | ⭐ 只做 Windows 时首选；`Process.OutputDataReceived` 现成好用 |
| Node + Electron | 150–250 MB | 为了个下载器背 200 MB 不值 |
| Go + Wails | 10–15 MB | 本机无 Go，不引入 |
| Python + PySide6 | 60–120 MB | 既然用 exe，这条路只剩「熟练」一个理由，体积大 5 倍 |

---

## 3. IPC 协议（实测验证过的契约）

### 3.1 元数据探测

```bash
yt-dlp --dump-single-json --skip-download --no-playlist --no-warnings \
       --socket-timeout 15 --extractor-retries 3 \
       [--proxy URL] [--cookies-from-browser chrome] URL
```

- 播放列表 JSON 可能几十 MB，**建议改用 `--write-info-json` 落盘再由宿主读取**，避免 stdout 管道缓冲与编码问题。
- ✅ **格式决策可离线复现**：宿主侧可自己实现「下载什么」，把 `--simulate --print` 当查询引擎用：

```bash
yt-dlp --load-info-json info.json --simulate -f "bv*[height<=1080]+ba/b" \
       --print "PICK|%(format_id)s|%(height)s|%(ext)s"
# 实测输出： PICK|137+140|1080|mp4
```

### 3.2 进度上报 ✅

```bash
yt-dlp --newline --no-warnings --no-colors \
  --progress-delta 0.2 \
  --progress-template "download|%(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s" \
  -P "<outdir>" -o "%(title).150B [%(id)s].%(ext)s" URL
```

实测真实输出：

```
download|downloading|1024|3145728|NA|NA
download|finished|3145728|3145728|131417041.33|NA
```

三个必须知道的点：

1. `--progress-delta 0.2` 是 **yt-dlp 侧节流**，比在宿主程序里做节流更省 CPU 和 IPC。
2. `total_bytes` 可能是 `NA`，要回落到 `total_bytes_estimate`；两者皆 `NA` 时进度条切「不确定态」，**不要显示 0%**。
3. `finished` **不等于完成**，后面还有合并/转码。

### 3.3 最终文件路径 ⚠️ 有真坑（已重新实测修正）

**❌ `--print` 与 `--progress-template` 不能共存** —— 实测：

| 配置 | 进度行 | 最终路径 |
|---|---|---|
| 仅 `--progress-template` | **7** | 无 |
| 仅 `--print after_move:filepath` | **0** | 有 |
| **两者同时** | **0** ❌ | 有 |

`--print` 隐含 `--quiet`，会把进度输出整个压掉。**本文早期 §4 的「完整参数模板」把两者放在一起，是自相矛盾的。**

**✅ 正确做法：改用 `--print-to-file`**（它不隐含 `--quiet`）：

| 配置 | 进度行 | 最终路径 |
|---|---|---|
| `--print-to-file after_move:filepath <file>` | **7** ✅ | **有** ✅ |

```bash
yt-dlp --newline --no-colors \
  --progress-delta 0.2 \
  --progress-template "download|%(progress.status)s|..." \
  --print-to-file "after_move:filepath" "<task_dir>/filepath.txt"
```

三个必须知道的点：

1. **`--print-to-file` 是 append 模式**（源码 `YoutubeDL.py:3255` `open(filename, 'a', ...)`）
   → 宿主**每次派发前必须截断该文件**，读取时取**最后一行**。
2. `filepath` 仍**只在模板为裸字段名时可用**：`--print "after_move:filepath"` ✅；
   加前缀 `"after_move|FILE|%(filepath)s"` → 实测输出 `NA` ❌。
3. **`[download] Destination: <path>` 给的是 temp 路径**，不是移动后的最终路径（实测确认）。
   在 `-P temp:` 方案下它是 `<temp>/x.mp4`，而 `after_move:filepath` 给的是 `<home>/x.mp4`。

宿主侧解析规则：
- `filepath.txt` 里最后一行 → **最终路径**；
- 进度行以 `download|` 开头 → **进度事件**；
- `[Key] msg` → **后处理事件**（见 §3.4）。

### 3.4 后处理阶段：只能解析 stderr ⚠️ 已修正

**上一轮建议的 `--progress-template "postprocess:..."` 实测无效**：

```
postprocess|PP|downloading|NA      ← 打印的还是下载进度，不是后处理事件
```

判断后处理阶段**必须解析 stderr 文本标记**。从 `postprocessor/ffmpeg.py` 源码确认的标记格式为 `[<PostProcessorKey>] <msg>`：

| stderr 行 | 含义 | 验证 |
|---|---|---|
| `[download] Destination: <path>` | 开始下载，拿到临时文件名 | ✅ 实测捕获 |
| `[Merger] Merging formats into "<path>"` | 正在合并音视频 | 源码确认 |
| `[ExtractAudio] Destination: <path>` | 正在提取音频 | 源码确认 |
| `[EmbedSubtitle] Embedding subtitles in "<file>"` | 正在嵌入字幕 | ✅ 实测捕获 |
| `[Metadata] Adding metadata to "<file>"` | 正在写元数据 | ✅ 实测捕获 |
| `[EmbedThumbnail] <method>: Adding thumbnail to "<file>"` | 正在嵌入缩略图；**method 可变**（`mutagen` / `ffmpeg` / `atomicparsley`） | ✅ 实测捕获 |
| `[VideoRemuxer] Remuxing video from <a> to <b>; Destination: <path>` | 正在转封装；**会改变最终文件路径** | ✅ 实测捕获 |
| `ERROR: ...` | 失败 | ✅ 实测捕获 |

⚠️ `[EmbedThumbnail]` 的中间 token 可变，`^\[(\w+)\]\s+(.*)$` 取出的 payload 需再按第一个空格切一次。
详见 `DESIGN.md` §14.4。

一条正则 `^\[(\w+)\]\s+(.*)$` 即可分类。

### 3.5 状态机

```
pending → probing → downloading → postprocessing → completed
                                        ↘ failed / canceled / paused
```

`downloading → postprocessing` 的切换时机：收到 `finished` 进度 **且** 出现 `[Merger]`/`[ExtractAudio]` 行。**该状态是必需的** —— 用户看到 100% 却还在转圈时必须显示「正在合并」，否则会被当成卡死。

### 3.6 取消与断点续传

- **不要传 `--no-part`**，保留 `.part` 才有断点续传，配合 `--continue`（默认开启）。
- 取消后 `.part` 要保留；仅用户显式「删除任务」时才清理。
- **kill 必须连带子进程树**：ffmpeg 是 yt-dlp 的子进程，只杀父进程会留下占着输出文件句柄的 ffmpeg，下次续传直接失败。Windows 用 `taskkill /PID <pid> /T /F`，Unix 用 `os.killpg` / 进程组。

### 3.7 编码 ⚠️ 非 ASCII 必炸（已修正）

实测证据（中文标题在 GBK 控制台被打乱）：

```
before_dl|BEFORE|testid123|�ҵĲ�����Ƶ Test
```

**⚠️ 更正：本文早期版本建议「设 `PYTHONIOENCODING=utf-8` 让 yt-dlp 输出 UTF-8」——实测这条不成立。**

yt-dlp 用 `utils.preferredencoding()`（即 `locale.getpreferredencoding()`，
本机是 cp936/GBK）编码控制台输出，**主动绕过了 `PYTHONIOENCODING`**。

实测证据：设了 `PYTHONIOENCODING=utf-8` 之后，stderr 里 `you’re` 的 `’`
仍然是字节 `A1 AF`（GBK 的 U+2019）：

```
'you' 之后原始字节: 6F 75 A1 AF 72 65 20 6E 6F 74 ...
按 UTF-8 解释: ou��re not ...
按 GBK  解释: ou’re not ...
```

**正确做法（宿主侧）**：

1. **先试 UTF-8、失败再按系统代码页解码**。顺序不能反——合法 GBK 序列
   几乎不可能是合法 UTF-8，先试 UTF-8 不会误判；反过来会把正常中文解坏。
2. **绝不能用「要求合法 UTF-8」的按行读取**。例如 tokio 的 `BufReader::lines()`
   遇到不合法的行返回 `Err`，若写成 `while let Ok(Some(l)) = ...` 会**直接终止
   整个读取循环**——一条含中文路径的 `[Merger] Merging formats into "D:\...\中文.mp4"`
   就能让该任务之后所有进度与后处理事件全部丢失。要按字节读到 `\n` 再解码。

**好消息：关键路径不受影响**
- `--dump-single-json` 走 stdout，且把非 ASCII **转义成 `\uXXXX`**，整段是纯 ASCII
  （实测中文标题往返无损），`serde_json` 能正确还原
- `--print-to-file` 写文件用的是**显式 UTF-8**（`YoutubeDL.py:3255`）

受影响的只有人类可读的报错与日志文本——但那正是用户要看的。

中文/emoji 文件名是必测项（B 站、抖音标题全是表情）。

---

## 4. 完整参数模板

```bash
# ── 探测 ──
yt-dlp --dump-single-json --skip-download --no-playlist --no-warnings \
       --socket-timeout 15 [--proxy URL] [--cookies-from-browser chrome] URL

# ── 下载 ──
yt-dlp \
  --newline --no-warnings --no-colors \
  --progress-delta 0.2 \
  --progress-template "download|%(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s" \
  --print "after_move:filepath" \
  --print "before_dl|BEFORE|%(id)s|%(title)s" \
  --no-simulate --continue \
  --retries 10 --fragment-retries 10 \
  --concurrent-fragment-downloads 4 \
  --paths "home:<outdir>" --paths "temp:<tmpdir>" \
  --output "%(title).150B [%(id)s].%(ext)s" \
  --format "<宿主程序算好的表达式>" \
  --merge-output-format mp4 \
  [--limit-rate <rate>] [--proxy URL] [--cookies-from-browser chrome] \
  [--ffmpeg-location <dir>] \
  --no-mtime \
  URL
```

细节：
- **`--no-colors` 必须加**，否则 stdout 混入 ANSI 转义序列，解析器会莫名失败。
- **`%(title).150B` 截断**，防止超长标题撑爆 260 字符路径。
- **`--paths "temp:<dir>"`** 把中间文件放临时目录，成品再移到目标目录，用户目录不会看到 `.part` 与 `.f137.mp4` 碎片。
- **`--merge-output-format`** 显式指定，别指望默认行为。
- 音频提取：`--extract-audio --audio-format mp3 --audio-quality 0`（CLI 下 postprocessor 配置变成等价命令行参数）。

---

## 5. sidecar 分发与自动升级

```
<InstallDir>/
  app.exe                    ← 宿主程序
  bin/yt-dlp.exe             ← 出厂版本（只读）
  bin/ffmpeg.exe  ffprobe.exe
<AppData>/<App>/
  bin/yt-dlp.exe             ← 升级后版本，优先使用
  cache/  db.sqlite  config.json  logs/
```

**查找顺序**：`<AppData>/<App>/bin/` → `<InstallDir>/bin/` → `PATH`。这样升级**不需要管理员权限**（写 `Program Files` 通常需提权，是同类工具常见翻车点）。

**升级策略 —— 不要只依赖 `-U`**（写只读目录会失败、不支持 nightly 切换）。走 GitHub Releases API：

```
GET https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest
→ tag_name 与本地 `yt-dlp --version` 比对（实测为干净单行，如 2026.07.04）
→ 下载 assets 中的 yt-dlp.exe / yt-dlp_macos
→ 写 <AppData>/<App>/bin/，Windows 上原子替换（先写 .new 再 rename）
```

- **宿主程序自身升级**：Tauri 用自带 updater，.NET 用 Velopack / Squirrel，不要自己写。
- **ffmpeg**：更新频率远低于 yt-dlp，**内置并随应用版本走**，不做自动更新。注意 ffmpeg 为 LGPL/GPL，商业分发建议改为首次运行时引导用户下载，而非内置进安装包。

### 可选优化：常驻进程

冷启动 0.3–1s 是 exe 方案固有成本；批量添加多个链接、每个都要探测元数据时串行启动会很难受。
可选做法是自写一个常驻小型 Python 服务（内部用 `yt_dlp` 库）包装成 JSON-RPC / JSON-Lines，宿主只连它。

**但这等于把「免打包」优势还回去**（又要维护 Python 运行时）。建议：
- **v1 就用纯 CLI sidecar**，简单可靠；
- 仅在实测「探测延迟影响体验」后，再把它做成**可选加速层**，失败自动回落 CLI。

---

## 6. 必须提前规划的坑：杀软误报

从自己签名的应用里 `CreateProcess` 一个 **PyInstaller 打包、解压到临时目录再执行** 的 exe —— 该行为模式（dropper 特征）与小型木马高度相似。

- 内置 yt-dlp.exe **随安装包一起签名**，不要运行时从网络下载；
- 首次运行把内置副本**复制到 `<AppData>/<App>/bin/` 再执行**，不要直接从 `Program Files` 或临时目录执行；
- 提供「yt-dlp 无法启动 / 被杀软拦截」的排查提示；
- 应用本身做代码签名（EV 证书可直接过 SmartScreen；OV 证书需积累声誉）。

---

## 7. 已知未决 / 下一步

1. ~~**宿主语言未定**~~ → 已定：Tauri 2 + Vue 3（见 `DESIGN.md` §1）。
2. **建议先做的一层**：把「参数构造 + 输出解析」写成独立、可单测的模块 —— 它是两种方案共用、也最容易出错的部分。**UI 用什么反而不急。**
3. ~~**UI 形态未定**~~ → 已定：列表为主 + 行内缩略图 + 展开详情（见 `DESIGN.md` §9）。
4. ~~待办：任务队列与并发~~ → 已定：探测/下载双池 + 每域名限流（见 `DESIGN.md` §4）；
   持久化与崩溃恢复见 §5；Cookie 子系统见 §6；更新链路见 §8。

---

## 8. 验证环境备注

- 外站（YouTube/Vimeo）在验证时被反爬拦截，**协议结论通过本地 HTTP 服务与 `--load-info-json` 离线手段获得**，与站点无关。
- 沙箱限制：localhost 出站被拦（返回 502），后台进程会随父进程回收；本地临时测试目录为 `%TEMP%`（写入沙箱临时区，跨调用不稳定）。
- 测试产物已清理。
