# yt-dlp 桌面下载工具 — 已锁定架构决策

> 承接 `HANDOFF.md`。分工：**HANDOFF.md 记录「调研与协议验证」**（yt-dlp CLI 契约、实测结论），
> **本文记录「已决策的架构」**。两者冲突时以 HANDOFF 的 ✅实测 为准。
> 标注 ✅实测 的契约来自 HANDOFF，已在 yt-dlp 2026.07.04 (Windows) 现场验证。

---

## 0. 决策总表

| 项 | 决定 | 理由 |
|---|---|---|
| 与 yt-dlp 的集成方式 | **CLI sidecar 子进程**，不用 `yt_dlp` Python 库 | 站点失效修复不必重打包宿主 |
| 宿主 | **Tauri 2** (Rust) | 体积 10–20 MB；`tokio::process` 流式读子进程最干净 |
| 前端 | **Vue 3 + TypeScript + Vite** | 虚拟滚动生态成熟 |
| 主界面形态 | **列表为主**，行内缩略图 + 点击展开详情 | 视频下载器缩略图有意义；卡片式在多任务时崩 |
| 格式选择 | **预设为主 + 高级模式展开完整表**，两者都落到 `-f` 表达式 | 见 §3 |
| 并发 | **探测池与下载池分离** + 每域名限流 | 见 §4 |
| Cookie | **一等子系统**，多 profile，主路径 `--cookies <file>` | 见 §6 |
| 代理 | 全局配置 + 按任务覆盖，探测/下载两处都传 | 刚需 |
| 常驻加速层 | **v1 不做** | 见 §4.3 |

**目标场景**：各类站点都要兼顾；**proxy 和 cookie 均为刚需**，不是可选功能。

---

## 1. 目录结构

```
yt-dlp_UI/
├─ src/                              ← Vue 3 前端
│  ├─ components/
│  │   ├─ TaskList.vue               ← 虚拟滚动列表
│  │   ├─ TaskRow.vue                ← 缩略图 + 标题 + 进度条 + 状态
│  │   ├─ TaskDetail.vue             ← 展开详情
│  │   ├─ FormatPicker.vue           ← 预设 + 高级模式
│  │   ├─ PlaylistPicker.vue         ← 播放列表条目勾选
│  │   └─ Settings/{Cookie,Proxy,General}Settings.vue
│  ├─ stores/                        ← Pinia
│  └─ ipc.ts                         ← invoke / listen 封装
├─ src-tauri/
│  ├─ binaries/yt-dlp-x86_64-pc-windows-msvc.exe
│  ├─ src/
│  │   ├─ ytdlp/
│  │   │   ├─ args.rs                ← 参数构造（纯函数，可单测）
│  │   │   ├─ parse.rs               ← 输出解析（纯函数，可单测）★最易出错
│  │   │   ├─ locate.rs              ← exe 查找顺序
│  │   │   ├─ probe.rs               ← 元数据探测
│  │   │   └─ runner.rs              ← 进程启动 / 流式读取 / 进程树取消
│  │   ├─ cookie/
│  │   ├─ task.rs                    ← 任务状态机
│  │   ├─ queue.rs                   ← 双并发池 + 域名限流
│  │   ├─ store.rs                   ← SQLite 持久化
│  │   └─ update.rs                  ← yt-dlp.exe 自更新
│  └─ Cargo.toml
├─ DESIGN.md                         ← 本文
└─ HANDOFF.md
```

**`args.rs` 与 `parse.rs` 必须是零依赖纯函数**——两种宿主方案共用、最易出错，也最容易单测。

---

## 2. Tauri 2 sidecar：为什么不用它启动 ⚠️（已修正）

> **更正**：本文早期版本称「`sidecar()` 只从安装目录解析，所以 AppData 里的新版永远不会被用到」。
> **该说法是错的**，见 §2.2 的源码与实测。结论（改用 `tokio::process`）不变，但**理由完全不同**。

### 2.1 `externalBin` 仍然要配

`bundle.externalBin` 负责在构建时把出厂版 exe 复制进安装目录，保证**开箱可用**，
且该 exe **随安装包一起签名**（关系到 HANDOFF §6 的杀软误报问题）。

命名要求：`src-tauri/binaries/yt-dlp-x86_64-pc-windows-msvc.exe`
（本机 triple 由 `rustc --print host-tuple` 确认）。

### 2.2 `sidecar()` 的实际解析逻辑

源码 `tauri-plugin-shell` 2.4.0 `process/mod.rs`：

```rust
fn relative_command_path(command: &Path) -> crate::Result<PathBuf> {
    let exe_path = platform::current_exe()?;
    let exe_dir = exe_path.parent().ok_or(Error::CurrentExeHasNoParent)?;
    let base_dir = if exe_dir.ends_with("deps") { exe_dir.parent().unwrap_or(exe_dir) } else { exe_dir };
    let mut command_path = base_dir.join(command);          // ← 关键
    #[cfg(windows)] { /* 后缀不是 .exe 就补上 .exe */ }
    Ok(command_path)
}

pub(crate) fn new_sidecar<S: AsRef<Path>>(program: S) -> crate::Result<Self> {
    Ok(Self::new(relative_command_path(program.as_ref())?))
}
```

关键在 `base_dir.join(command)` 用的是 **`Path::join`**，而 Rust 的 `join` **遇到绝对路径会丢弃 base**。实测：

```
base                 = "C:\\Program Files\\MyApp"
join(裸文件名)        = "C:\\Program Files\\MyApp\\yt-dlp.exe"
join(D:\\ 绝对路径)   = "D:\\AppData\\MyApp\\bin\\yt-dlp.exe"    ← 绝对路径整个替换
join(\\\\server UNC)  = "\\\\server\\share\\yt-dlp.exe"
```

→ **`sidecar("D:\\...\\yt-dlp.exe")` 是可行的**，它不会去找安装目录。

另外注意 `relative_command_path` **没有存在性检查、没有查找顺序、没有回退**——它只是拼一个路径字符串。

### 2.3 真正不能用的理由：进程树

```rust
impl CommandChild {
    /// Sends a kill signal to the child.
    pub fn kill(self) -> crate::Result<()> {
        self.inner.kill()?;          // SharedChild::kill → 只杀直接子进程
        Ok(())
    }
}
```

`SharedChild::kill()` 底层是 `std::process::Child::kill()`（Windows 上为 `TerminateProcess`），
**只终止那一个进程，不递归**。而 §5.5 的硬要求是：取消任务时必须**连带 ffmpeg 一起杀**，
否则残留的 ffmpeg 会占住输出文件句柄，下次续传必然失败。

次要理由：
- `sidecar()` 是面向 JS 的 API，其 scope 体系围绕「已声明的 sidecar 名」构建；
  用绝对路径调它属于**未文档化行为**，依赖 `join` 语义，跨版本不保证
- 插件在 `RunEvent::Exit` 时同样只 `child.kill()`，也不覆盖进程树

### 2.4 结论

| | 做法 |
|---|---|
| `bundle.externalBin` | **仍然配置**——出厂副本随安装包签名 |
| 启动方式 | **完全不用 `sidecar()`**，一律走 `tokio::process::Command` + 显式路径 |
| 路径来源 | `locate()` 解析（见下） |
| 终止方式 | **Job Object** 实现进程树终止；同时可设 `CREATE_NO_WINDOW` |

两条代码路径（出厂副本 / 升级副本）合并为一条，避免分叉。

**exe 查找顺序**：`<AppData>/<App>/bin/` → `<InstallDir>/bin/` → `PATH`。
这样升级 yt-dlp **不需要管理员权限**。

### 2.5 ⚠️ 实测更正：yt-dlp 有两种打包形态，`externalBin` 只对其中一种有效

原设计假设「yt-dlp.exe 是自包含单文件」。**实测这个假设不总成立。**

| 形态 | 结构 | 官方发布 | 本机实测 |
|---|---|---|---|
| **onefile** | 单个 `.exe`（约 17.8 MB） | ✅ `yt-dlp.exe` | — |
| **onedir** | `.exe`（8 MB）+ 同级 `_internal\`（145 文件 / 21.8 MB） | — | ✅ `D:\Download_software\yt-dlp\` |

包管理器（winget / scoop 等）常给的是 **onedir** 形态。

**后果**：Tauri 的 `externalBin` **只复制那一个 .exe**。把 onedir 的 exe 复制走，
得到的是一个跑不起来的残缺副本：

```
[PYI-31464:ERROR] Failed to load Python DLL
'...\target\debug\_internal\python310.dll'
```

这个副本落在 `<InstallDir>`，而 `<InstallDir>` 在查找顺序里**优先于 PATH** ——
于是它**遮蔽掉 PATH 上完好的安装**，表现为「明明装了 yt-dlp，却所有任务都失败」。

**两条修正**：

1. **`resolve_ytdlp()` 必须验证 exe 真能运行**，不能只 `is_file()`。
   实现上按顺序对候选跑 `--version` 并用 `looks_like_version()` 校验，
   首个通过的才采用，结果缓存（实测启动开销 < 1s，且只在首次解析时发生）。
   另外暴露 `diagnose_ytdlp()` 列出每个候选「不存在 / 存在但无法运行 / 可用」，
   否则这类失败无从排查。

2. **出厂副本必须用官方 onefile 版**（GitHub Releases 的 `yt-dlp.exe`），
   否则 `externalBin` 天然不完整。若确实要内置 onedir 形态，
   得改用 `bundle.resources` 打包整个目录，并让 `locate()` 认得这个目录结构 ——
   代价是升级不再是「换一个文件」。

`classify_error()` 已把 `Failed to load Python DLL` / `[PYI-` 单独归类并给出可操作提示。

---

## 3. 格式决策：存 `-f` 表达式，绝不存 format_id

探测返回的 `formats` 数组动辄几十条。若 UI 让用户勾选并把 `format_id` 列表入库，会导致：
info.json 过期后失效、任务无法重放、数据库被撑大。

**规则：UI 选语义选项 → 宿主翻译成 `-f` 表达式 → 数据库存表达式本身。**

```
UI: 1080p + mp4   →   存 "bv*[height<=1080]+ba/b[ext=mp4]/b"
```

- 任务永远可重放，**完全不需要持久化 info.json**
- 表达式可用 `--load-info-json ... --simulate -f "<selector>" --print` **离线验证**（HANDOFF §3.1 ✅），
  因此预设行为可写单测

| 层级 | 交互 | 覆盖 |
|---|---|---|
| L0 默认 | 最佳 / 1080p / 720p / 仅音频 | ~90% |
| L1 高级 | 展开完整 format 表（等同 `yt-dlp -F`），选中行**反解回表达式** | 其余 |

⚠️ L1 也必须落到表达式，不可存 format_id 清单，否则两条代码路径必然分叉。

### 3.1 ⚠️ DASH 站点把音视频分开列，选一行不等于选一个文件

YouTube 之类的站点把**视频轨和音频轨分开列**（`137` 只有画面、`140` 只有声音）。
用户看到的就是「有些行的音频编码是 `—`，有些行的视频是 `—`，那该怎么选」。
不处理的话，选了 `137` 会下出一个**没有声音的视频**。

**规则（`ytdlp_core::args::format_expression_for`）**：

| 选中行的类型 | 翻译成 | 说明 |
|---|---|---|
| 仅视频（有 vcodec、无 acodec） | `<id>+ba/<id>` | 自动配一条最佳音频轨；`/` 后面是「配不上就只要视频」的回退 |
| 视频+音频（两路都有） | `<id>` | 已经封装好，**不能再追加 `+ba`** |
| 仅音频 | `<id>` | 就是一条音轨 |

早期版本对**所有「有视频编码」**的行都追加 `+ba`，于是选了 `18`（本来就带音频）
会变成多此一举的 `18+ba/18`。

界面上必须**把类型标出来**（`视频+音频` / `仅视频` / `仅音频` 三个色标），并按
「有画面的在前、分辨率从高到低、音轨排最后」排序。只给一张原始表，用户学不会
该点哪一行。

⚠️ **两个容易踩的坑**：

1. **storyboard / 预览图要滤掉**。它们 `ext` 是 `mhtml`、`vcodec`/`acodec` 都是 `none`，
   留着会被误标成「仅音频」。判据必须用 **`ext == "mhtml"`**，**不能**用「两路编码都缺」——
   generic 提取器给直链文件时同样是 `vcodec: "none", acodec: null`，那是一个
   **完整可用的文件**，滤掉它就直接下不了了（实测：这条差点把本地回环测试搞挂）。
2. **「两路编码都缺」要当已封装处理**，表达式就是它自己，既不能配 `+ba`，也不能当纯音轨。

### 3.2 任务的格式覆盖与「跟随预设」的区别

任务详情里的「选择格式」是**任务级**覆盖（`Task.format_override`）：

- `None`（默认）= **跟随设置里的预设**。这样改了设置里的预设，还没下载的任务会跟着变，
  而不是被建任务那一刻的快照永久钉死。
- `Some(expr)` = 用户显式选的，下载时覆盖预设。
- 传空串即**恢复默认**（清掉 override，并把展示用的表达式重置成当前预设）。

点「按此格式重新下载」后走的是和「重新下载」完全一样的链路（探测 → 下载）。
**存下来但什么都不发生是绝对不行的**——那正是本文件反复强调的「静默无反应」。
按钮文案也写明会重新下载，而不是含糊的「确定」。

### 3.3 「优先选择」编码：只能用 `-f` 过滤器，**不能用 `-S`**

设置页「格式」里有视频 / 音频两个编码偏好（H.264 / VP9 / AV1；AAC / Opus / Vorbis）。
它只是一种**偏好**：站点没有首选编码时逐级回落，绝不会因此下不到。

**⚠️ 不要用 `--format-sort`（`-S`）实现，这是个陷阱。**
`-S` 会**整体替换** yt-dlp 的默认排序，而不是追加。默认排序里 `res` 权重很高，
一旦被替换掉分辨率就不再参与比较：

| 配置 | 实测选中 |
|---|---|
| `-f bv*+ba/b`（默认排序） | `399+251` |
| `-f bv*+ba/b -S vcodec:h264` | `137+251` ✅ |
| `-f bv*+ba/b -S acodec:aac` | **`18`** ❌ 360p 封装格式 |

第二行看着对，第三行暴露了问题：只想换个音频编码，**画质从 1080p 塌到 360p**。
用户完全不会意识到。

**实际做法**是往 `-f` 里加过滤器（`args::preset_expression_with`），
默认排序原封不动，偏好只加在候选集上，按 `/` 逐级回落：

```
bv*[vcodec^=avc1]+ba[acodec^=mp4a]     ← 两样都命中
/bv*[vcodec^=avc1]+ba                  ← 只命中视频
/bv*+ba[acodec^=mp4a]                  ← 只命中音频
/bv*+ba                                ← 都不命中（等于原预设）
/b                                     ← 本站只有封装好的单文件
```

高度上限要同时挂在 `bv*` 与最后的合并回落上，否则「1080p + 首选 H.264」
会退化成完全不限高度：`bv*[height<=1080][vcodec^=avc1]+…/b[height<=1080]/b`
（实测连续两个过滤器是合法的）。

**值必须与 yt-dlp 报出的编码名一致。** 实测大家熟悉的叫法**匹配不上任何东西**：

| 想选 | 写这个 ❌ | 要写 ✅ |
|---|---|---|
| H.264 | `h264` | `avc1`（实际报 `avc1.640028`） |
| AAC | `aac` | `mp4a`（实际报 `mp4a.40.2`） |

写错**不报错**，只是静默退化成「没有偏好」——所以界面的选择项由后端
白名单生成（`codec_choices`），前端不另写一份。

**白名单同时是安全边界**：实测任何非法过滤器（`[vcodec@=x]`、`[vcodec^=]`、
`~=`）都会让 yt-dlp 抛 `SyntaxError: Invalid filter specification`、
打印 Python traceback 并非零退出——不是优雅报错。所以
`CodecPreference::sanitized` 会把白名单外的值一律丢掉，用户输入绝不原样进 `-f`。

**作用范围**：偏好织进的是**预设表达式**。任务详情里在高级格式表显式点的那一行
（`Task.format_override`）原样使用，不再替他改动——那是用户自己的决定。
任务详情里显示的「格式表达式」就是实际会用到的那个（含偏好），不是裸预设。

---

## 4. 并发模型

### 4.1 两个独立的池

| 池 | 并发 | 依据 |
|---|---|---|
| **探测** | 4–6 | 仅拉元数据，请求轻；冷启动 0.3–1s 才是主要成本 |
| **下载** | 1–2 | 重、占带宽、触发站点限流 |

探测并发高**不会**导致封禁；下载并发高**会**。故不可共用一个上限。

### 4.2 每域名限流

比「全局降并发」更有效：**同一站点同时只跑 1 个下载**，不同站点可并行。
20 个 B 站链接排队、同时跑 1 个 YouTube —— 快得多，又不触发单站限流。

### 4.3 常驻进程加速层：v1 明确不做

探测并发 4–6 使 20 个链接的探测从 20s 压到约 3–4s，已足够。
且常驻层会把「免打包」的核心收益还回去（又要维护 Python 运行时）。
**仅在实测探测延迟影响体验后**，再作为可选加速层加入，失败自动回落 CLI。

### 4.4 ⚠️ Tauri 的**同步**命令会堵住整个窗口（不只是慢一点）

**Tauri 里 `#[tauri::command]`（不加 `async`）的函数跑在主线程上**，
而主线程就是 Windows 消息循环所在的线程。于是一个「有点慢」的命令不是
「卡一下界面」，而是**整个窗口失去响应**——连标题栏、拖动、关闭都点不动。
（WebView2 的渲染进程是独立的，所以页面看起来还画得出来，但输入进不来。）

**规则：任何会做 IO 的命令都必须写成 `#[tauri::command(async)]`。**
包括读写文件、起进程、走网络、读剪贴板。纯读内存的（`get_settings`、
`list_tasks`、`codec_choices`…）可以留同步。

实际踩到的场景：设置页挂载时并发调用 `aria2c_info` / `list_browsers` /
`detect_js_runtimes` / `list_cookie_profiles`，而 `aria2c_info` 会
**起一个 `aria2c --version` 进程**。那台机器上这一步偏慢，
于是「进设置页再点返回任务」就整窗卡住——用户报的就是这个。

#### 查这个问题时踩的坑：CDP 的合成输入**测不出来**

第一轮我怎么测都是「2ms、无长任务」：用
`Input.dispatchMouseEvent` / `Runtime.evaluate` 量点击延迟和帧间隔，一切正常。

原因：**CDP 的输入事件是直接投给渲染进程的，绕过了宿主的消息循环。**
所以宿主主线程被堵死时，合成点击照样秒回，而用户真实鼠标点下去毫无反应——
量出来的和用户感受到的完全是两件事。

**正确的测法**是直接问宿主窗口还活着没有：

```powershell
# WM_NULL + SMTO_ABORTIFHUNG：超时就说明窗口没在处理消息
SendMessageTimeout(hwnd, 0x0000, 0, 0, 0x0002, timeoutMs, out _)
```

实测（故意给 `aria2c_info` 加 1.5s 睡眠）：

| 命令形式 | 探针结果 |
|---|---|
| `#[tauri::command]`（同步） | 34 次里 **5 次无响应**，最长连续 ~1165ms |
| `#[tauri::command(async)]` | 71 次里 **0 次无响应** |

这条也解释了为什么「整个窗口都没反应」这种描述值得当真——
它指向的是宿主消息循环，而不是 JS 卡顿。

---

## 5. 持久化与崩溃恢复

### 5.1 temp 目录必须由 task_id 稳定推导 ⚠️

HANDOFF §3.6 要求「取消后保留 `.part` 才能续传」，但未定义 `.part` 位置。
若 `--paths "temp:<dir>"` 的 `dir` 每次随机生成（`%TEMP%/ytdlp-<random>`），
则恢复时 yt-dlp 在**新目录**中找不到旧 `.part`，**续传静默失效**，且旧碎片永久残留。

**约定**：`<temp 根>/<task_id>/`，仅删除任务时递归清理。
temp 根默认是 `<AppData>/<App>/tmp/`，但**设置里的「临时目录」可以改**。

**为什么允许改**（`paths::temp_root`）：`--paths temp:` 与输出目录在**同一个卷**上时，
合并/嵌入后的成品只要改名就能落到输出目录；跨卷则要把整个文件复制一遍
（4K 视频动辄几个 GB）。默认的 AppData 在系统盘，对「输出到别的盘」的用户
正好是最坏情况。

⚠️ **代价**：换目录会让**已经在进行的任务**在新目录里找不到旧 `.part`，
续传静默失效、旧碎片永久残留。界面上那句「断点续传依赖它稳定不变」就是这个意思。
`remove_record` / `remove_many` / 启动清理都会**同时扫默认根和配置根**，
所以改过之后旧根里不会留下垃圾。

### 5.2 进度绝不直接写库

`--progress-delta 0.2`（HANDOFF §3.2 ✅）= 每任务每 0.2s 一条事件；10 任务即 50 次/秒写入。

- 进度**只走内存 + 事件推前端**
- **仅在状态转换时落库**
- 每 5–10s flush 一次快照（用于崩溃后显示「上次到 45%」）
- SQLite 开 **WAL** + `synchronous=NORMAL`

> **当前实现状态**：用的是 `%APPDATA%\ytdlp-desktop\tasks.json` 而非 SQLite，
> 配一个「dirty 标志 + 每 3 秒节流落盘」的后台任务，写入走 `.tmp` → rename 原子替换。
>
> 之所以先不上 SQLite：现阶段任务量在几十到几百条，全量 JSON 重写的成本可接受，
> 而 SQLite 会引入 `rusqlite` 依赖与迁移管理。**上面那三条纪律已经遵守**
> （进度不落库、只标 dirty、节流写盘），所以后续换 SQLite 时数据层可整体替换，
> 不会牵动状态机。

### 5.3 崩溃恢复

启动时 `probing` / `downloading` / `postprocessing` → 统一重置为 `paused`；
`completed` / `failed` / `canceled` 不动。

⚠️ **`postprocessing` 崩在合并/转码中途最麻烦**：下载其实已完成，临时文件状态未知。
恢复策略：**重跑整条命令**，靠 `--continue` 跳过已下载部分、重新执行后处理。

### 5.4 状态机

```
pending → probing → downloading → postprocessing → completed
              ↘ failed / canceled / paused
```

`downloading → postprocessing` 的切换时机：收到 `finished` 进度 **且** 出现 `[Merger]`/`[ExtractAudio]` 行。
**该状态是必需的**——用户看到 100% 却还在转圈时必须显示「正在合并」，否则会被当成卡死。

### 5.5 取消与进程树

kill **必须连带子进程树**：ffmpeg 是 yt-dlp 的子进程，只杀父进程会留下占着输出文件句柄的 ffmpeg，
下次续传直接失败。**Job Object** 为主，`taskkill /PID <pid> /T /F` 为兜底。

「暂停」与「取消」语义区分：暂停 = 杀进程 + **保留** `.part`；删除任务 = 杀进程 + 删 `.part` + 删记录。

### 5.6 数据目录与便携模式

默认全部落在 `%APPDATA%\ytdlp-desktop\`（Unix：`~/.config/ytdlp-desktop`）：

| 内容 | 文件 |
|---|---|
| 设置 | `config.json` |
| 历史记录 | `tasks.db`（+ `-wal` / `-shm`） |
| 导入的 cookies | `cookies/` |
| yt-dlp 升级副本 | `bin/` |
| 断点续传临时文件 | `tmp/`（可被设置里的 `tempDir` 改掉） |

**便携模式**：exe 同目录下放一个 `portable.txt`，数据就改落到
`<exe目录>\data\`，整个程序目录拷走即完整迁移。

#### 为什么用标记文件，而不是「exe 旁边能写就自动用」

自动判定会让**同一个 exe 在不同机器上把数据写到不同地方**：装在
`Program Files` 时落 `%APPDATA%`，解压到 U 盘时落自己旁边。用户完全预期不到
「我的历史去哪了」——这正是本项目最想避免的失败方式。一个显式的
`portable.txt` 一眼就能看出当前是不是便携模式。

目录建不出来或写不进去（只读目录、U 盘写保护、`Program Files`）时**退回
`%APPDATA%`**，而不是让之后每次写盘都报一个看不懂的错。判定里特地真写一个
探针文件：`create_dir_all` 对「已存在但只读」的目录是成功的，光靠它判断不出能不能写。

#### 切到便携模式是「换个空目录」，不是搬家

标记一放，应用就从零开始（默认设置 + 空历史）。**不做静默搬迁**：
U 盘上跑一次就把这台机器 `%APPDATA%` 里的历史拷进去，那才是真的吓人。
需要旧数据就手动把 `%APPDATA%\ytdlp-desktop\` 里的文件复制进 `data\`。

> 实测：便携模式下 `data\` 会自动建出 `config.json` / `tasks.db`，
> 而 `%APPDATA%` 那份**一个字节都不会动**；设置页「常规」里始终显示
> 当前生效的数据目录（`data_dir` 命令），毕竟 `%APPDATA%` 在资源管理器里
> 默认是隐藏的，用户根本找不到。

---

## 6. Cookie 子系统

刚需，独立成模块（`source.rs` / `store.rs` / `validate.rs`）。

1. **多 profile，不是单个 cookie file**。用户必然有多套身份（B站账号 A / YouTube 账号 B）。
   任务记录存 `cookie_profile_id`。单一输入框在第二个账号出现时就得重构数据模型。
2. **格式校验前移**：自己校验 Netscape 格式（tab 分隔 7 字段），给出可读错误，
   而不是把 yt-dlp 的晦涩报错甩给用户。
3. **识别认证失效**：报错文本有线索（如 `Sign in to confirm you're not a bot`），
   应归类为「认证失效」而非泛泛的「下载失败」，否则用户只会反复重试。
4. **安全**：cookie 是明文凭证，存 AppData 而非项目目录，UI 不明文回显。
5. **主路径 `--cookies <file>`**，浏览器导入仅作辅助。理由：`--cookies-from-browser`
   每次调用都要重新读/解密浏览器数据库（探测+下载 = 双倍开销），
   浏览器运行时数据库被锁会失败，且 **Chrome 127+ 的 App-Bound Encryption（cookie schema v20）
   在 Windows 上无法解密** —— yt-dlp [issue #15401](https://github.com/yt-dlp/yt-dlp/issues/15401) 仍在追踪，
   社区方案是插件 [`seproDev/yt-dlp-ChromeCookieUnlock`](https://github.com/seproDev/yt-dlp-ChromeCookieUnlock)。
   Firefox 路径不加密，最可靠。

⚠️ **Cookie 必须在探测阶段就生效**。很多站点不登录连元数据都拿不到；
若只在下载时传，用户会看到「探测失败」却不知是登录问题。

### 6.1 `--cookies-from-browser` 本机实测（Windows）

三种浏览器逐一实测（yt-dlp 2026.07.04）：

| 浏览器 | 结果 | 失败原因 | 关闭浏览器能救吗 |
|---|---|---|---|
| **firefox** | ✅ **成功，提取 161 条** | 无加密 | — |
| chrome | ❌ 失败 | ① 数据库被占用（实测 31 个进程在跑）② **App-Bound Encryption 已启用** | ❌ **不能**（② 才是根因） |
| edge | ❌ 失败 | `Failed to decrypt with DPAPI`（[issue #10927](https://github.com/yt-dlp/yt-dlp/issues/10927)） | ❌ 不能 |

原始错误：

```
chrome: ERROR: Could not copy Chrome cookie database. See .../issues/7271
        cookies.py:324 _extract_chrome_cookies → cookies.py:1115 _open_database_copy
edge:   ERROR: Failed to decrypt with DPAPI. See .../issues/10927
firefox: Extracted 161 cookies from firefox          ← 唯一可用
```

**根因判定**：Chrome 的 `Local State` 中存在 `app_bound_encrypted_key`
（base64 前缀 `QVBQQg` = `APPB`）→ **App-Bound Encryption 处于启用状态**；
而本版 `yt_dlp/cookies.py` 中**不存在任何** `app_bound` / `v20` 处理代码（已 grep 确认）。
→ **关掉 Chrome 也救不回来**，这不是文件锁问题。

⚠️ 注意 `_open_database_copy` 的注释与实测：
```python
# cannot open sqlite databases if they are already in use (e.g. by the browser)
shutil.copy(database_path, database_copy_path)
```
Chrome 以独占方式持有 cookie 库，`shutil.copy` 直接失败 → **浏览器运行时必然报错**。
「数据库被占用」与「解密失败」是**两种不同的错误**，UI 必须分别给出不同指引。

### 6.2 设计结论：必须做预检，不能等到任务失败

`--cookies-from-browser` 仍然作为**可选来源**提供（Firefox 可用，其他机器/未来版本也可能可用），
但**绝不能把它当成主路径**（§6 第 5 条）。

**必须实现设置页的「测试」按钮**（与 §7 代理探测同一模式），对选中浏览器跑一次轻量提取并分类：

| 预检结果 | UI 指引 |
|---|---|
| `Ok(n)` | 显示「已提取 n 条 cookie」 |
| `DatabaseLocked` | 「请完全退出 <浏览器> 后重试」（含 31 进程这类情况） |
| `DecryptFailed` | 「该浏览器的 cookie 已被系统加密保护，yt-dlp 无法读取」→ **引导改用 cookies.txt** |
| `NotFound` | 「未找到该浏览器的 cookie 数据库」 |

若跳过预检，失败会推迟到每个任务的探测阶段，用户只会看到「探测失败」而不知原因。

### 6.3 UI 只保留「浏览器 + 配置文件」两个下拉

早期还给了一个「手动填写完整参数」的输入框（直接写
`BROWSER[+KEYRING][:PROFILE][::CONTAINER]`）。**已删除**：

- 那两个下拉已经覆盖了 `BROWSER` 与 `:PROFILE`，剩下 `+KEYRING` / `::CONTAINER`
  一年也用不到一次，却要在界面上占一行、还要解释一套语法；
- 暴露原始串等于把「界面显示的」和「实际生效的」拆成两条路径，
  用户改了一处另一处不同步——本文件反复强调的那类分叉。

> ⚠️ 代价：如果用户以前手写过 `::CONTAINER`，界面上看不到也改不了，
> 下次动下拉时会被 `syncToSpec` 覆盖掉。属于可接受的取舍。

**Windows 的读取风险只在选中相关浏览器时才提示。** 一直挂着那段
「Chrome/Edge 读不到」的警告，用 Firefox 的人得先跳过一段与自己无关的话
才看得到有用的信息。判断依据是**后端下发的 `chromium` 字段**
（`BrowserInfo.chromium`），不在前端猜名字——否则这份判断迟早和后端分叉。

---

## 7. 代理

结构照着 Windows「设置 → 网络和 Internet → 代理」那一页来，三选一：

| 模式 | 行为 |
|---|---|
| **不使用代理** | 不传 `--proxy` |
| **跟随系统代理** | 每次派发时读注册表 `HKCU\...\Internet Settings`（`ProxyEnable` / `ProxyServer` / `ProxyOverride`），拼成 `--proxy` |
| **手动配置** | HTTP / SOCKS5 + 主机名 + 端口 + 身份验证（**没有**绕过列表输入框，见 §7.1） |

设置页只用**读**注册表来展示；真正的取值在每次探测/下载时重做，所以改了系统代理
不用回来点一下。走 `reg query` 而不是注册表 FFI：`HKCU` 不需要管理员权限，
`reg.exe` 必然存在，解析逻辑还能单测。

### 7.1 ⚠️ 绕过列表必须由**宿主**判断，yt-dlp 帮不上忙

实测：`--proxy http://127.0.0.1:1` 配上 `no_proxy=127.0.0.1` 访问本地 HTTP 服务
**依然连不上**。也就是说给了 `--proxy` 之后，`no_proxy` 环境变量是不起作用的。

所以「不为以下项使用代理」只能自己实现：**命中就干脆不传 `--proxy`**
（`runner::proxy_for(settings, url)`，按 URL 的主机名判断）。
好处是结果完全可预测，不依赖 yt-dlp 的代理栈行为。

**这份名单是内建常量 `proxy::LOCAL_BYPASS`，界面上不提供输入框。**
原先给过一个可编辑的 textarea，问题有二：一是绝大多数人只会把默认值删掉或
改坏，然后本地/内网请求全走代理直接失败；二是它是纯字符串，用户很难知道自己
改的东西有没有生效。本机与内网永远不该走代理，这是**不需要用户决策**的事。

```
localhost,127.*,10.*,192.168.*,<local>
```

匹配语义与 Windows 的 `ProxyOverride` 一致（`*` 通配、`<local>` 表示不带点的主机名）。
注意 `*.example.com` 按 Windows 语义**不匹配** `example.com` 本身。
「跟随系统代理」模式下若注册表里的 `ProxyOverride` 为空，同样回落到这份常量。

> 配套的 `settings::proxyBypass` 键已废弃。`merge_defaults` 只补键不删键，
> 所以 `normalize_settings` 里显式 `remove` 掉，免得 config.json 里一直挂着
> 一个看起来还能用的开关。

### 7.2 SOCKS5 用 `socks5h`（DNS 也走代理）

yt-dlp 支持 `socks5`（官方 onefile 自带 PySocks，实测能连上 YouTube）。
但界面里选 SOCKS5 时拼的是 **`socks5h://`**：让 DNS 也在代理侧解析。
国内直连 DNS 会被污染，域名解析留在本地等于白配代理。

### 7.3 密码的存储与展示

- **拼进 URL 时要按 RFC 3986 转义** userinfo 段，否则密码里的 `@`、`:`、`/`
  会把代理地址拆坏。
- **任何给用户看的文本都要打码**（`runner::redact_proxy`）：代理串会出现在
  错误信息里，不打码就等于把密码写进界面。设置页的「实际传给 yt-dlp」预览
  自己拼、不复用完整 URL。
- **「记住密码」不勾时，密码只留在内存**，写盘前在 `save_settings` 里抹掉。

### 7.4 旧配置迁移

早期版本只有一个「使用代理 + 一个 URL 输入框」（`proxyEnabled` + `proxyUrl`）。
升级时 `migrate_proxy` 把它解析成结构化的 `manual` 配置，**不能静默丢掉**——
否则表现是「昨天还能下、今天全失败」。

⚠️ 迁移必须在 `merge_defaults` **之前**做：补全默认值会把 `proxyMode` 填成 `"none"`，
之后就再也分不清「老配置里压根没这个键」和「用户真的选了不用代理」了。

- **探测和下载两处都要传** `--proxy`（易漏）——两处统一走 `runner::proxy_for`
- 设置页提供**「检查连接」按钮**（真发一次请求），
  否则用户只能靠「任务失败」猜代理配错了

### 7.5 JS 运行时（`--js-runtimes`）⚠️ 不给就下不了 YouTube

**这是跟代理同一等级的必需参数，不是可选优化。**

YouTube 的 n-sig / player 挑战要用 JS 解。`--no-js-runtimes` 的帮助文本提到有
"defaults"，但实测 yt-dlp **不会自动启用系统上已安装的运行时**。不传的后果：

| 参数 | 结果 |
|---|---|
| 不传 | ❌ `ERROR: ... needs to be reloaded`（或只返回 storyboard 预览图），16s |
| `--js-runtimes node` | ✅ 29s |
| `+ --remote-components ejs:npm` | ✅ 69s |

**为什么难查**：解不了挑战时站点是**降级返回**，不是报「缺 JS 运行时」。
报错信息指向格式不可用 / 需要重新登录，跟 n-sig 毫无字面关联。

**实现**：`ytdlp-core::JsRuntimeOptions` + `push_js_args`（一次一个：
`--js-runtimes node --js-runtimes bun`），`paths::detect_js_runtimes()` 在 PATH 上
探测 `node / deno / bun / quickjs`，`runner::js_of()` 统一给探测与下载两处取值。
设置为空即自动检测——用户不该为了一个「不给就下不了」的必需参数去手工配置。

设置页只留三样：**候选运行时状态条**（哪个装了、路径是什么，纯诊断）、
**「指定运行时」输入框**（留空即自动检测，给极少数要手工指定的人）、
以及上面那段说明。原先还有两样，都已删掉：

| 删掉的 | 为什么 |
|---|---|
| **「当前生效：node」那行** | 纯冗余。输入框为空时占位符就是检测结果，非空时输入框本身就是生效值；状态条另有 ✓ 标记 |
| **「附加 `--remote-components ejs:npm`」勾选框** | 官方 exe 自带组件（帮助文本写明 "currently not needed if you are using an official executable"），勾上只多花 40 秒。没有理由让它在界面上占一格 |

**`--remote-components ejs:npm` 的逃生门**：后端 `js_of()` **仍然读**
`jsRemoteComponents`，只是它不在 `default_settings()` 里，所以设置页不会再出现
那个勾选框。真遇到「官方组件失效」的情况，改 `config.json` 写
`"jsRemoteComponents": true` 即可（`merge_defaults` 只补键不删键，
前端 `deepClone` 是整对象 JSON 往返，这个手工键会一路带到保存后）。

> **踩坑记录**：接线时新增了 `jsRuntime` / `jsRemoteComponents` 两个设置键，
> 但老 `config.json` 里没有它们 → 前端 `s.jsRuntime.trim()` 抛 TypeError →
> **整个设置面板白屏**。现在 `normalize_settings` 按 `default_settings()` 递归补齐
> （`merge_defaults`），新键不会再引发这类崩溃。

### 7.6 Cloudflare 反爬拦截（`generic:impersonate`）

有些站点套了 Cloudflare，yt-dlp 会报：

```
ERROR: [generic] Got HTTP Error 403 caused by Cloudflare anti-bot challenge;
try again with --extractor-args "generic:impersonate"
```

界面用户没法敲命令行，所以设置页「网络与账号 → 绕过 Cloudflare 拦截」直接对应
这个开关（`args::GENERIC_IMPERSONATE_ARG`），探测与下载**两处都带**。

#### 那句建议是怎么来的（读过 `yt_dlp/extractor/generic.py`）

```python
# Do not impersonate by default; see https://github.com/yt-dlp/yt-dlp/issues/11335
impersonate = self._configuration_arg('impersonate', ['false'])
if 'false' in impersonate:
    impersonate = None
...
except ExtractorError as e:
    if not isinstance(e.cause, HTTPError) or e.cause.status != 403:
        raise
    already_impersonating = res.extensions.get('impersonate') is not None
    if already_impersonating or (cf-mitigated 不是 challenge 且标题不是 Attention Required!):
        raise                       # ← 不是真的 CF 挑战，原样抛出
    msg = 'Got HTTP Error 403 caused by Cloudflare anti-bot challenge; '
    if not self._downloader._impersonate_target_available(ImpersonateTarget()):
        msg += 'see https://github.com/yt-dlp/yt-dlp#impersonation ... and '
    raise ExtractorError(f'{msg}try again with  --extractor-args "generic:impersonate"')
```

三个可用的推论：

1. **只有真的撞上挑战才给这句建议**（`cf-mitigated: challenge`，或页面标题是
   `Attention Required! | Cloudflare`）。所以看到这句话，就说明确实是 CF 拦截。
2. 报错里**没有**那段「去装 impersonation 依赖」时，说明
   `_impersonate_target_available()` 为真——即**随包的 yt-dlp 已经带了
   `curl_cffi`**（实测 `--list-impersonate-targets` 列出 17 个目标）。
3. **已经在模拟还撞上**的话，`already_impersonating` 为真直接 re-raise，
   **不会**再给这句建议。所以「开着还报一模一样的错」= 模拟没骗过去，
   而不是开关没生效。

#### 为什么用 `generic:impersonate` 而不是全局 `--impersonate`

- 全局那个作用于**所有**请求，连本来正常的站点也一起改 TLS 指纹。
  yt-dlp 的帮助文本明确警告：*forcing impersonation for all requests may have a
  detrimental impact on download speed and stability*。
- 报这个错的**永远是 generic 提取器**（链接没被专门的提取器认领时），
  所以只给它开，影响面最小。

**默认关**，与 yt-dlp 自己的取法一致（见上面那段 `['false']` 与 issue #11335）。
单测钉住三件事：默认不传、打开后探测与下载都带、永远不退化成全局 `--impersonate`。

> ⚠️ 未验证的部分：手里没有 Cloudflare 站点可测，所以**只验证到「参数拼对了、
> yt-dlp 接受这个写法」**（本地实测不报 invalid extractor argument），
> 没有跑通一次真实的「开启后 403 变 200」。

---

## 8. 更新链路（三条独立）

| 组件 | 更新方式 | 频率 |
|---|---|---|
| 宿主 App | Tauri updater 插件 | 低 |
| **yt-dlp.exe** | GitHub Releases API 自查 + 原子替换 | **极高** |
| ffmpeg | 随 App 版本走，**不**自动更新 | 极低 |

yt-dlp 更新流程：`GET api.github.com/repos/yt-dlp/yt-dlp/releases/latest` → `tag_name`
→ 与本地 `yt-dlp --version` 比对（HANDOFF §5 ✅ 为干净单行）→ 下载 → 写 `.new` → rename 原子替换。

**三条硬约束（HANDOFF §5 未提）**：

**1. 安装目录不可写。** 实测：非管理员进程写 `C:\Program Files` 直接抛 `UnauthorizedAccessException`。
这就是出厂副本只读、升级副本放 `<AppData>/bin/` 的根本原因。
（另：即使提权写入，NSIS/MSI 的「修复」功能也可能把它还原。）

**2. 运行中的 exe 不能原地替换，但可以重命名。** 实测（进程存活已确认）：

| 操作 | 运行中的 exe |
|---|---|
| 独占写打开 | ❌ 失败 |
| `WriteAllBytes` 覆盖 | ❌ 失败 |
| 删除 | ❌ 失败（`UnauthorizedAccessException`） |
| **重命名** | ✅ **成功** |
| 重命名后写入新版 | ✅ 成功 |

→ 这就是 Windows 经典的「先重命名再替换」模式。

⚠️ **更正**：早期版本称「更新须在所有任务空闲时执行，或标记待更新」——**该说法过强**。
实测表明**即使有任务在跑也能完成更新**，代价只是旧文件需延迟清理。

更新算法（目标 `<bin>/yt-dlp.exe`）：

1. 下载新版本 → `<bin>/yt-dlp.exe.new`
2. 用 `.new --version` **验证**（见约束 3），失败即中止
3. 若 `yt-dlp.exe` 存在 → `rename(yt-dlp.exe → yt-dlp.exe.old)`
   - 若此步因占用失败 → 再退化为「下次启动生效」
4. `rename(yt-dlp.exe.new → yt-dlp.exe)`
5. `.old` **不立即删**，下次启动时尝试删除（届时进程已退出）
6. **交换期间暂停新任务派发**，避免解析到不存在的 exe

注意：正在运行的任务**不受影响**（进程持有自己的映像），只有新派发的任务使用新版本。

**3. 替换前必须用 `--version` 重新验证。** 一个损坏的 exe 会让所有任务同时失败，比不更新糟得多。

**不要依赖 `yt-dlp -U`**（写只读目录会失败、不支持 nightly 切换）。

---

## 9. UI 形态

**列表为主**，行高较大（小缩略图 + 标题 + 进度条 + 状态），点击展开详情面板。

- 视频下载器的**缩略图是有意义的**，纯表格浪费了这一点
- 任务数可能上百 → 必须**虚拟滚动**，否则 DOM 爆炸
- **「正在合并」必须在行内有明确视觉**（理由见 §5.4）

### 9.0 右键菜单：换成我们自己的，**不要放行原生菜单**

Tauri 的 WebView2 默认弹出 **Edge 的浏览器菜单**。在一个桌面应用里这既不像原生，
也有实际危害：「刷新」会把没保存的设置改动丢掉，「检查」会在界面上开一个 DevTools。

**踩过的弯路**：第一版规则是「既不是可编辑字段、也没有选中文字时才压掉」，
想着「选中文字时留原生菜单好让用户复制」。结果用户选中链接再右键，
弹出来的还是**整份浏览器菜单**——表情符号 / 导入密码 / 书写方向 / 更多工具 / 检查。
放行原生菜单等于放行**全部**浏览器入口，没有中间态。

现在的做法：**一律压掉**，需要什么就自己实现什么（`src/contextMenu.ts`）：

| 右键位置 | 菜单 |
|---|---|
| 输入框有选中 | 剪切 / 复制 / 粘贴 / 全选 |
| 输入框无选中 | 粘贴 / 全选 |
| 非输入框、有选中文字 | 复制 |
| 其它 | 不弹任何东西 |

| 动作 | 实现 |
|---|---|
| 复制 | `navigator.clipboard.writeText`，失败回落隐藏 textarea + `execCommand('copy')` |
| 剪切 | 先复制，再 `execCommand('delete')` |
| 粘贴 | **走后端**读剪贴板（`read_clipboard` 命令），再 `execCommand('insertText')` |
| 全选 | input/textarea 用 `.select()`，其余 `execCommand('selectAll')` |

菜单用 fixed + `clientX/clientY` 定位并夹进视口，点外部 / 滚动 / Esc / 失焦都收起。
菜单项用 `mousedown` 而不是 `click`，并且 `preventDefault`——这样焦点留在原输入框上，
粘贴才有落点。

用 `execCommand('insertText')` 而不是直接改 `.value`：前者会**触发 input 事件**
（Vue 的 v-model 靠它同步）也进撤销栈，直接赋值两样都没有。

#### 粘贴为什么必须走后端

前端读剪贴板在 WebView2 里**做不到**：

- `navigator.clipboard.readText()` 会**卡住**——实测它等在一个
  `edge://permission-request-dialog/` 权限弹窗上，没人点就永远不返回；
- `document.execCommand('paste')` 恒返回 `false`（Chromium 出于安全禁掉了）。

所以 `src-tauri/src/clipboard.rs` 直接调 Win32（`user32` + `kernel32`，
**不引额外 crate**，与 `shell.rs` 同样的取舍）。**写**不需要走后端：
`writeText` 只要用户手势就能用，实测真实左键点击后确实写进去了。

> 这一版之前试过「输入框放行原生菜单」来保住粘贴，代价是输入框上仍会看到
> 导入密码 / 表情符号 / 检查。既然粘贴能自己做，就没必要再放行任何东西。

#### 验证方式

页面里 `dispatchEvent` 造的是**不可信事件**，没有 user activation，
`writeText` 与 `execCommand('copy')` 都会拒绝——这样测会误判成「复制坏了」。
必须用 CDP 的 `Input.dispatchMouseEvent` 发真实点击，并配合
`scripts/focus-window.ps1` 把窗口置前（文档不聚焦时剪贴板 API 一律报
`NotAllowedError: Document is not focused`）。

实测：

| 场景 | 结果 |
|---|---|
| 选中标题 → 右键 | 原生菜单被压掉，只出现「复制」；真实点击后 `Get-Clipboard` 里正是那段文字 |
| 「添加任务」输入框 → 右键 | 只出现「粘贴 / 全选」，无浏览器菜单 |
| 点「粘贴」 | 输入框内容变成剪贴板里的 URL（走 `read_clipboard`） |
| 输入框内有选中 → 右键 | 剪切 / 复制 / 粘贴 / 全选 四项 |

> 副作用：DevTools 的入口没了。开发期本来也不靠它——工具走
> `--remote-debugging-port` + `scripts/cdp-attach.mjs`（见 RESUME）；
> 发布版 Tauri 默认也不启用 devtools。

第二个交互难点（与格式选择并列）：**播放列表**。
探测时用 `--flat-playlist --dump-single-json` 先拿扁平条目列表（不解析每条，快得多），
UI 让用户勾选，再对选中项生成任务。
否则一个 500 集合集会被当成单个任务直接开始下。

### 9.1 视觉主题：亮色（已确定）

主区**纯白**，侧栏/状态栏 `#fafbfc` 极浅灰；层次靠 **1px 边框 + 极浅投影**，
不靠重阴影（叠多了显脏）。所有颜色集中在 `src/style.css` 的 `:root` 变量里，
组件只引用变量，所以换主题不必动组件逻辑。

三条容易漏的：

1. **必须声明 `color-scheme: light`**（`index.html` + `:root`）。系统处于深色模式时，
   滚动条、`<select>` 下拉、checkbox 这些**原生控件**不受 CSS 变量控制，
   会跟着系统渲染成深色，在白色页面上非常突兀。
2. **语义色在亮底上要压暗一档**。深色主题的 `#3fb950`（绿）放到白底上对比度不够、
   发飘；亮色主题统一改用 `--ok: #0f8a45` 这类更深的版本。
3. **站点标识色同理**：B 站粉 `#fb7299`、YouTube 红 `#ff6b6b` 在深色底上好看，
   白底上要压到 `#d94f7c` / `#cc2b1f`。

截图用 `node scripts/cdp-shot.mjs <port> <out.png>`（走 CDP 的
`Page.captureScreenshot`，拿到的是精确 CSS 像素）。
**不要用 `PrintWindow` 截图**：WebView2 是合成层，刚还原最小化窗口或正赶上重绘时
会抓到上一帧的残影，文字重影看着像渲染 bug。

### 9.2 设置是主区页面，不是右侧抽屉

侧栏导航分两组：上面是**任务筛选**，下面分隔线之后是**设置**。点「设置」时主区整体
换成设置页，而不是盖一层抽屉。

理由：

- 抽屉盖住的那半边窗口**完全浪费**，而设置项本身只需要 ~720px 宽
- 设置和任务筛选是**同一层级的导航目标**，用两种不同的交互（一个替换主区、
  一个浮在上面）承载同级内容会让人困惑——「我到底在哪一层？」
- 抽屉必须处理遮罩点击关闭、Esc、焦点陷阱；做成页面这些全都不需要

对应地，表单列宽 `max-width: 720px` 固定：设置项铺满 1200px 会让人读一行要
横跨半个屏幕。底部操作条是「返回任务 / 还原改动 / 保存设置」——
做成页面后用户会改到一半就想走，所以显式的「还原改动」比原来的「取消」更有用
（`s` 是 `store.settings` 的深拷贝，还原只是重新 `Object.assign` 一次，
**不可能污染已保存的值**）。

### 9.3 多选工具条在**列表正上方**
顶栏放一个「多选」按钮；进入后**它下面那一排**（原添加栏的位置）换成多选工具条：
`已选 N 项` ＋ 全选 / 反选 / 清空 / 移除所选记录 / 完成。侧栏底部的「批量选择」
已删除。

理由是**归属**：多选作用于列表里的东西，就该跟列表在一起；侧栏是**导航**，
不是操作区。原先把「批量选择」放在侧栏底部，「删除所选」又原地替换它，
结果是最危险的那个动作跑到了离对象最远的地方。

配套的三条：

1. **点行体 = 勾选**，不是展开详情（`TaskRow::onRowClick`）。否则「在列表里选」
   得先精确点到左边那个 15px 的小方框，点标题却只会展开——那不叫「直接在列表上选」。
2. **多选时隐藏行内操作**（暂停/继续/展开）。此刻点行体是勾选，留着 ▼ 会让人
   以为还能展开。
3. **全选/反选只覆盖当前可见项**，换筛选或进设置页时**清空选择**。否则会出现
   「已选 3 项」但列表里一个勾都看不见，而它们照样会被删掉——正是 §13 想避免的
   「用户不知道自己在删什么」。

按钮文案是「**移除所选记录**」而非「删除所选」：它只删记录、清理临时文件，
**不动磁盘上的成品文件**。用「删除」命名一个不删文件的操作，就是 §13 说的语义混淆。

### 9.4 单击展开、双击打开文件 ⚠️ 两者天然冲突

任务行：**单击展开/收起详情，双击用系统默认程序打开成品文件**。

冲突点很直白：双击会先触发两次 `click`。三种解法：

| 做法 | 结果 |
|---|---|
| 不管，两下都切换 | 净效果为零，但会「展开又收起」闪一下（约 150ms，看得见） |
| 延迟 200ms 再切换 | 没有闪烁，但给**最主要的交互**（展开）平白加了延迟 |
| **按 `event.detail` 吞掉第二下** ✅ | 单击零延迟，双击不闪 |

采用第三种。`event.detail` 就是浏览器自己的连击计数，而且**遵循系统的双击间隔设置**，
比另写一个 `setTimeout` 阈值可靠（自己定阈值会和系统设置打架）。

代价：双击一个**已展开**的行会把它收起来。可接受——用户此刻的意图是去看片子。

其余几点：

- **`@dblclick` 挂 `.main-line` 而不是 `.row`**：否则双击展开区里的内容
  （比如那条很长的文件路径）也会触发「打开文件」。
- **多选模式下禁用双击打开**：那时点行体的语义是勾选。
- **没有成品文件时双击什么都不做**，不弹无意义的错误。
- **打开失败要显式报错**（文件被移走/删掉）：双击没反应是最难排查的失败方式。
  后端先 `exists()` 再 `ShellExecuteW`。
- **双击不可发现**，所以详情里另给「打开文件 / 打开所在文件夹」两个按钮，
  并在「最终文件」那行标注「双击任务行可直接打开」。

**实现**：`src-tauri/src/shell.rs` 用 `ShellExecuteW`（走文件关联，等价于在资源管理器里双击），
只 `#[link(name = "shell32")]`，不引入额外 crate。不走 `cmd /C start`（引号与 `&` 会打架），
也不用 `explorer.exe <path>`（`,` 会被当成参数分隔符）。「打开所在文件夹」用
`explorer.exe /select,"<path>"`。

### 9.5 窗口尺寸与字号

**默认窗口 `1180x760`**（`tauri.conf.json`），最小 `940x560`。

⚠️ **窗口尺寸的唯一真源是 `tauri.conf.json`。** 原来 `lib.rs` 的 `setup()` 里
硬编码了 `1440x920` 去「夹到屏幕内」，于是改 conf 完全没效果——排查了半天才发现
在 setup 里被覆盖。现在改成：读**已经生效的** `inner_size()`，只在超出显示器时
**往下夹**，绝不往上撑（下限交给 conf 的 `minWidth`/`minHeight`）。

之所以要夹：实测开发机逻辑分辨率只有 `1536x816`，`1440x920` 的窗口底部会跑到
屏幕外，设置页的「保存」按钮点不到。`1180x760` 则留足了任务栏与边框的余量。

**字号集中在 `src/style.css` 的 `--fs-*`**，组件里不写死 px：

| 档位 | 值 | 用途 |
|---|---|---|
| `--fs-2xs` | 12px | 角标、单位 |
| `--fs-xs` | 14px | 次要信息（时间/路径/说明）、表单标签 |
| `--fs-md` | 16px | 正文、按钮、列表行 |
| `--fs-lg` | 18px | 小标题 |
| `--fs-xl` | 20px | 页面标题 |

**为什么全是偶数**：设备像素 = CSS 像素 × 缩放比。原来的档位是 13.5 / 12.5 / 11.5 / 10.5，
在 125%（Windows 笔记本极常见）下分别是 16.875 / 15.625 / 14.375 / 13.125——全落在
半个设备像素上，光栅器只能凑合画，**中文笔画粗细会不均匀**，这才是「发虚」的主因，
不是字体选得不对。偶数档在 100% / 150% / 200% 下都是整数设备像素。

**另外别再设 `-webkit-font-smoothing: antialiased`**：它在 macOS 上让字变锐，
在 Windows 上会把 ClearType 的次像素抗锯齿换成灰度抗锯齿，深色字压白底反而更糊。
（那两行是从深色主题那版抄来的，在 Windows 上是纯负收益。）

### 9.6 路径字段：对话框与手填**两条路都要有**

输出目录 / 临时目录 / 归档文件 / cookies.txt 都是「输入框 + 浏览… + 打开(定位)」：

- **手填必须留着**：路径常常是从别处（下载目录、另一个软件）复制过来的，只能粘贴；
  而想在某个盘里找那个文件夹时，手打又太痛苦。少任何一条都会有人卡住。
- **取消对话框返回 `null`，此时什么都不做**——不能把字段清空。
- **目录行的「打开」用 `open_file`**（ShellExecute 打开目录就是进资源管理器），
  文件行的「定位」用 `reveal_file`（`/select,`），**不打开**文件本身。
- **即时形态校验**（`dirWarning`）：只查「是不是绝对路径」「是不是把文件填进了目录栏」，
  **不验存在性**——网络盘、移动硬盘暂时不在线不该拦着保存。留空提示「会用默认目录」。

**原生对话框用 `rfd`，不用 `tauri-plugin-dialog`**：后者要同时加一个 npm 包、一个 crate，
还得往 `capabilities/` 里加权限；这里只需要「弹个框拿一个绝对路径」，
开一个命令更省事，前端也仍然走现有的 `invoke` 一套。

⚠️ **目录选择没有纯 Web 的替代方案**：`<input type="file" webkitdirectory>` 只给相对路径
（拿不到绝对路径），`File.path` 是 Electron 才有的。所以这个依赖是必需的，不是图省事。
（cookies.txt 的**导入**仍然用 `<input type="file">` 读内容——那里只要内容不要路径，不必弹框。）

⚠️ **对话框必须在非主线程上跑**：命令声明成 `async` + `tauri::async_runtime::spawn_blocking`。
它是模态阻塞的，跑在主线程上会把窗口和消息循环一起卡住。

### 9.7 文件名模板：字段清单 + 实时预览

yt-dlp 的输出模板字段有上百个。只给一个空输入框，等于让用户自己去翻文档——
所以这个字段给了三样东西：

| | 内容 |
|---|---|
| **可用变量** | 30 个常用字段，按「基本信息 / 上传者与时间 / 播放列表 / 站点与统计」分组，**点一下插到光标处**；另附 4 条格式修饰符（`.150B`、`.50s`、`02d`、`>%Y-%m-%d`） |
| **常用模板** | 6 个预设（默认 / 标题+日期 / 上传者+标题 / 列表序号+标题 / 列表名+序号 / 只用标题），点一下套用 |
| **实时预览** | 用**真实任务的标题**渲染一遍，长标题被截成什么样一眼就能看见 |

**两条实测确认的坑，必须显式警告**（`scripts` 里用本地 HTTP 服务实测）：

1. **模板里没有 `%(ext)s` → 下出来的文件真的没有扩展名。**
   `-o "%(title)s"` 得到的文件就叫 `clip`，**yt-dlp 不会自动补 `.mp4`**。
   这会让文件失去关联、播放器不认。
2. **字段名拼错 → yt-dlp 静默填 `NA`，不报错。**
   `%(titel)s` 得到 `NA.mp4`。所以要把模板里出现的字段名与已知清单比对，
   不一致就提示——这是唯一能在下载前抓到这个错的机会。

⚠️ 预览的渲染器**不要**图省事把格式说明那段写成 `[^sSdDjJlLq]*`：
那样它会把后面的字面量一起吞掉，`%(title).150B [%(id)s]` 会被整体当成一个字段，
预览变成 `(4K) …)s]`（实际踩过）。必须只允许真正出现在格式说明里的字符：
`([-+#0-9.]*)([sSdDjJlLqB])`。

预览只实现常见写法，**不认识的写法原样保留**——猜错了给出一个「看起来对但其实错」
的预览，比不给预览更糟。按字节截断也要按 UTF-8 算，且不能截出半个字符。

---

## 10. 编码（非 ASCII 必炸）

HANDOFF §3.7 ✅ 实测：中文标题在 GBK 控制台被打乱。**两个都要做**：

1. 子进程环境变量 `PYTHONIOENCODING=utf-8`、`PYTHONUTF8=1`
2. 宿主侧**按 UTF-8 逐行解码**（Rust 用 `String::from_utf8_lossy`），
   不要用系统默认编码

中文/emoji 文件名是必测项（B 站、抖音标题全是表情）。

另外 `--no-colors` 必须加，否则 stdout 混入 ANSI 转义序列导致解析莫名失败。

---

## 11. aria2c 外部下载器 ⚠️ 实测发现三个问题

需求：支持 `--external-downloader aria2c`。实测（本机 aria2c 1.37.0 / yt-dlp 2026.07.04）暴露三个必须处理的问题。

### 11.1 用 aria2c 会彻底失去实时进度

**实测**：同一个 24 MB 文件、同样 `--progress-delta 0.1`：

| 下载器 | 进度事件条数 |
|---|---|
| 原生 | **14 条**（`status=downloading`，含实时字节与速度） |
| aria2c | **0 条** |

**源码根因**：`downloader/external.py` 全文只有**一处** `_hook_progress`（第 75 行），且在 `retval == 0` 成功分支里，只发 `status='finished'`：

```python
if retval == 0:
    status = {'filename': ..., 'status': 'finished', 'elapsed': ...}
    ...
    self._hook_progress(status, info_dict)

def _call_process(self, cmd, info_dict):        # 第 191 行
    return Popen.run(cmd, text=True, stderr=subprocess.PIPE if self._CAPTURE_STDERR else None)
```

→ **HANDOFF §3.2 的进度协议在 aria2c 下完全失效**。aria2c 任务会在整个下载期间停在「进行中、无进度」。

### 11.2 有救：让 aria2c 自己上报

`_call_process` 只 pipe 了 **stderr**，**stdout 是继承的**——所以 aria2c 自己的输出会流到 yt-dlp 的 stdout，能到达宿主。

```bash
--downloader-args "aria2c:--summary-interval=1 --enable-color=false"
```

实测可让 aria2c 每秒输出 `*** Download Progress Summary as of ... ***` 块。

- ⚠️ 选项名是 **`--enable-color=false`**；`--console-log-color` **不存在**（实测 `unknown option`，aria2c exit 28）
- ⚠️ aria2c 的输出与 yt-dlp 自身的输出**混在同一条 stdout 流**上，解析器必须容忍两种格式交织
- 备选：`--enable-rpc` 走 aria2c JSON-RPC（`aria2.tellStatus`）拿精确进度，格式稳定但复杂度高

### 11.3 文件大小轮询不可靠（原以为的兜底方案）

实测 aria2c 下载期间对 temp 目录轮询 `s.mp4.part`：

```
t= 1.5s  s.mp4.part     0
...
t=11.5s  s.mp4.part     0        ← 连续 12 秒都是 0
t=12.0s  s.mp4.part     24,395,776
```

目录枚举看到的大小在写入期间**一直是 0**，结束时才跳到完整值。→ **不能靠轮询 temp 文件做进度兜底。**

### 11.4 ⚠️ aria2c 会让本来能成功的下载失败

aria2c 的默认命令由 yt-dlp 硬编码为 `-x16 -j16 -s16`（`external.py` 第 315 行），这要求服务器支持 **Range**。对不支持 Range 的服务器实测：

| 配置 | 结果 |
|---|---|
| 默认 `-x16` | `Invalid range header ... errorCode=8`，**exit=-1 任务失败**，但 `.part` 已写入完整的 25,165,824 字节 |
| `--downloader-args "aria2c:-x1 -s1"` | **exit=0，成功** |

**这是最阴险的失败**：字节全都下完了，任务却被标记为失败。→ 设计必须包含 **aria2c 失败后自动回落原生下载**，而不能直接把失败抛给用户。

（`_configuration_args()` 在 `external.py` 第 329 行插入，位于硬编码 flag 之前，故用户参数可覆盖 `-x16`。）

### 11.5 分发：aria2c 随包，与 yt-dlp 同一套查找顺序

aria2c 是**独立二进制**，yt-dlp 不带它。现在和 yt-dlp 一样随包分发：

```
src-tauri/binaries/
  yt-dlp-x86_64-pc-windows-msvc.exe
  aria2c-x86_64-pc-windows-msvc.exe
  aria2c-COPYING.txt              ← GPLv2 原文，必须随包
  aria2c-LICENSE.OpenSSL.txt
  README-third-party.md
```

`tauri.conf.json` 里两个都进 `externalBin`；许可证文本进 `bundle.resources`。
查找顺序也一致（`locate::Tool`）：**随包副本 → AppData → PATH**，
并且每个候选都跑一次 `--version` 验证——**文件在 ≠ 能跑**。

⚠️ **许可证不同，必须如实分发**：yt-dlp 是 Unlicense，**aria2c 是 GPLv2**
（并附 OpenSSL 链接例外）。把多个独立程序**聚合**进同一个安装包不会让宿主变成 GPL，
但分发时要一并提供它的许可证原文与源码出处。所以那两个文本文件不能省。

版本更新频率极低，随 App 版本走即可；换文件即可升级，代码不用动。

### 11.6 ⚠️ `--downloader` 传绝对路径会被**静默忽略**

实测（同一份 yt-dlp、同一个文件，只改 `--downloader` 的写法）：

| 写法 | 结果 |
|---|---|
| `--external-downloader aria2c`（裸名，PATH 上有） | ✅ 真正调用 aria2c |
| `--downloader <存在的绝对路径>` | ❌ **不报错，静默回落到内置下载器** |
| `--downloader C:\nope\fake.exe`（路径不存在） | 报错 `No such external downloader` |

也就是说：**指向一个存在文件的路径会被无声吞掉**，下载照常成功、只是没用 aria2c——
这是最难排查的一类失败。而路径不存在时反而会报错。

顺带实测：**aria2c 不在 PATH 时，yt-dlp 同样是静默回落**，不报错。
两个静默点叠在一起，用户会一直以为自己用的是多线程下载。

**对策**：

1. yt-dlp 侧**只传裸名** `--external-downloader aria2c`。
2. 宿主把**自带 aria2c 所在目录前置到子进程的 `PATH`**（`prepare_aria2c`），
   这样裸名一定命中我们自带的那份，而不是用户机器上某个旧版本。
3. 宿主**自己**先 `resolve_aria2c()` 验证一遍；找不到就**就地关掉 aria2c**
   并给任务挂一条告警：「未找到可用的 aria2c，本次改用内置下载器」。
   不能让 yt-dlp 去静默回落——用户有权知道自己实际在用哪个下载器。

### 11.7 启用策略（已确定）

**高级选项，默认关闭。** 开启后：

1. 进度改走 §11.2 的 aria2c 自身 summary 解析（不再是 §3.2 协议）
2. UI 需明示「此任务使用 aria2c，进度显示可能不精确」
3. **aria2c 失败必须自动回落原生下载**（§11.4）——这是硬要求，因为 aria2c 会让本可成功的下载失败
4. 回落发生时需在任务日志里留痕，否则用户无法理解为何变慢

aria2c 未找到时**由宿主就地关掉该选项并告警**（见 §11.6），而不是让任务失败、
也不是让 yt-dlp 静默回落。

---

## 12. download-archive

需求：支持 `--download-archive`。实测结论：

**格式**：`<extractor_key> <video_id>`，**空格分隔**，每行一条。实测写出 `generic test`。

**行为**：第二次运行正确跳过，输出 `test has already been recorded in the archive`。

**并发安全性 ✅**：源码 `YoutubeDL.record_download_archive` 用 `locked_file(fn, 'a', encoding='utf-8')` 追加，
Windows 下底层是 **`LockFileEx` 独占锁**（`utils/_utils.py` 第 1578 行，`whole_low/whole_high` 整文件锁定）。
→ **多个并发任务写同一归档不会损坏文件**，无需自行加锁。

**但有陈旧读问题 ⚠️**：`preload_download_archive`（第 844 行）在**进程启动时**把归档读进内存 set。
并发的 yt-dlp 进程**看不到彼此的新增** → 同一条视频可能被两个任务同时下载（**重复下载，不是损坏**）。
→ 调度器应在派发前用**自己的数据库**查重，不能依赖归档做并发去重。

**其他**：`--force-download-archive` 可强制写归档；`--no-download-archive` 是默认。

### 12.1 ⚠️ BOM 会让归档**首行**静默失效

yt-dlp 用 `open(archive, encoding='utf-8')` 读归档——**`utf-8` 不去 BOM**，
所以带 BOM 的文件里首行会变成 `\ufeff<extractor> <id>`，与 `id` 比对失败。

实测（本地 Range 服务器 + 一个只含一行的归档，`--simulate --print "%(id)s"`）：

| 归档首行 | 输出 | 含义 |
|---|---|---|
| `generic clip\n` | *（无输出）* | 命中归档 → 跳过 ✅ |
| `\ufeffgeneric clip\n` | `clip` | **没命中 → 会重新下载** ❌ |

只有**第一行**受影响，后面的行照常匹配——所以症状很隐蔽：归档看起来在工作，
实际上最早那条记录一直是废的。（本项目里就踩到了：用户的 `archive.txt` 带 BOM，
首行那条 bilibili 记录实际无效，而第二行的 youtube 记录正常跳过。）

**约定**：
- 任何**读**归档的宿主代码都要 `strip_prefix('\u{feff}')`；
- 任何**写**归档的宿主代码都不要写 BOM（`std::fs::write` 天然不写，
  但 PowerShell `Out-File -Encoding utf8` 会写——手工修归档时别用它）。

### 12.2 与「手动删除」的交互 ⚠️

宿主删除归档条目时，运行中的 yt-dlp 进程**持有独占锁**（尽管只在 append 的瞬间）。
Windows 上 `LockFileEx` 会阻塞其他句柄 → 宿主的写入可能遭遇 sharing violation。
**约定**：宿主改写归档必须**带重试**，并优先在所有任务空闲时执行。

删除对**已在运行**的进程不可见（它内存里是旧 set）——这是可接受的，因为该进程本就在下载中。

---

## 13. 删除语义（已确定）

「手动删除」不是一件事，而是四种**互不相同**的语义。已确定的范围：

| 删除对象 | 是否做 | 说明 |
|---|---|---|
| **归档条目** | ✅ | 从 download-archive 移除 |
| **任务记录** | ✅ | 递归清理 `tmp/<task_id>/`（§5.1） |
| **批量 / 多选删除** | ✅ | 支持一次选多条 |
| **已下载的成品文件** | ⚠️ **仅显式单文件删除** | 应用**绝不自动删**；提供带确认的「删除此文件」按钮（§13.2） |

UI 上「移除记录」/「从归档移除」/「删除此文件」必须是**三个独立动作**，不可合成一个「删除」按钮。

### 13.1 ⚠️ 静默跳过有三条路径

`s.%(ext)s` 已存在时实测：

```
[download] D:\...\keep\s.mp4 has already been downloaded
exit=0                    ← 退出码 0，等同「成功」
```

**这两种跳过都返回 `exit=0`**，在进度协议里与真正的下载完成**无法区分**：

| 触发条件 | yt-dlp 输出 | 退出码 |
|---|---|---|
| 归档中已有该视频 | `... has already been recorded in the archive` | 0 |
| 成品文件已存在 | `... has already been downloaded` | 0 |
| 宿主 DB 中已有完成记录 | （宿主自行拦截） | — |

→ **必须解析这两条消息**并映射为独立状态「已存在，跳过」，否则 UI 会把「什么都没下」显示成「下载完成」。
→ 错误分类体系（§14）需包含 `Skipped` 这一独立类别，不能归入 `Completed`。

### 13.2 ⚠️ 已定范围存在一个能力缺口

**「移除归档条目」单独并不能实现「允许重新下载」。**

实测已证明：归档条目移除后，只要**成品文件仍在磁盘上**，yt-dlp 依旧跳过（`exit=0`）。
而本设计**不删除成品文件** → 于是「允许重新下载」这个目标实际上无法达成。

**已选定方案：提供显式的单文件删除按钮。**

- 应用**永不自动删除**成品文件——用户没点就不动，这是安全底线
- 「从归档移除」时**必须显示该视频的成品文件路径**，让用户知道要处理哪个文件
- 提供带二次确认的「删除此文件」按钮，**仅针对单条记录**，不做批量文件删除
- 删除前检查文件占用（可能正被播放器打开）→ 失败要**明确提示**而非静默
- 执行顺序应为：删文件 → 移归档 → 移记录（任一步失败都要能停在中途并说明状态）

UI 文案可以承诺「可重新下载」，但前提是提示用户还需删除文件——**不能只移除归档就宣称完成**。

---

## 14. 字幕 / 缩略图 / 元数据嵌入

### 14.1 选项的真实语义（源码核对）

| 选项 | dest | 说明 |
|---|---|---|
| `--embed-subs` | `embedsubtitles` | 帮助文本称 "only for mp4, webm and mkv"，但源码 `SUPPORTED_EXTS` 实际是 **mp4/mov/m4a/webm/mkv/mka**——**帮助文本不准确** |
| `--sub-langs` | `subtitleslangs` | 支持**正则**、逗号分隔、`all`、`-` 前缀排除（如 `all,-live_chat`） |
| `--embed-thumbnail` | `embedthumbnail` | 嵌入为封面 |
| `--embed-metadata`（别名 `--add-metadata`） | `addmetadata` | ⚠️ **同时嵌入章节与 infojson**，除非加 `--no-embed-chapters --no-embed-info-json` |

### 14.2 ⚠️ 容器兼容矩阵：mkv 是唯一全绿的容器

| 容器 | embed-subs | embed-thumbnail | embed-metadata | infojson |
|---|---|---|---|---|
| **mkv** | ✅ 最佳 | ✅ 原生 `-attach` | ✅ | ✅ |
| mp4 | ⚠️ `mov_text`；**ASS 有警告** | ⚠️ 脆弱三级回落 | ✅ | ❌ 仅 mkv/mka |
| webm | ⚠️ 仅 VTT | ❌ **致命错误** | ✅ | ❌ |
| m4a | ✅ | ⚠️ 走 mutagen | ✅ | ❌ |
| mp3 | ❌ 不支持 | ✅ ID3 | ✅ | ❌ |

**已确定：启用这三个功能时，输出容器默认切到 mkv。**

⚠️ 这与 §4 参数模板写死的 `--merge-output-format mp4` **直接冲突**，实现时必须改为**按功能动态选择容器**。

mp4 的脆弱点（源码注释自承）：
- 字幕 → `-c:s mov_text`；`mp4 + ass` 触发 `ASS subtitles cannot be properly embedded in mp4 files; expect issues`
- 缩略图 → 三级回落 `mutagen → AtomicParsley → ffmpeg+ffprobe`，且 yt-dlp 注明
  "Thumbnails attached using this method doesn't show up as cover in some cases"

### 14.3 ⚠️ 三者的失败语义互不相同

| 后处理器 | 触发条件 | 后果 |
|---|---|---|
| `FFmpegEmbedSubtitlePP` | 容器不支持 / 无字幕 / 文件缺失 / JSON / webm 非 VTT / mp4+ASS | **仅 `to_screen` 或 warning，不算错误** → 任务报成功，字幕静默丢失 |
| `EmbedThumbnailPP` | 容器不支持 | **抛 `EmbedThumbnailPPError` → 致命，任务失败** |
| | 无缩略图 / 文件缺失 | `to_screen` 或 warning，不算错误 |
| `FFmpegMetadataPP` | 无元数据可加 | `There isn't any metadata to add`，不算错误 |

→ **又一条「UI 会撒谎」的路径**：`--embed-subs` 失败是完全静默的。
→ 必须像 §13.1 的 `Skipped` 一样，**解析这些 warning 并映射为独立状态**（如 `CompletedWithWarnings`）。

### 14.4 实测确认的 stderr 标记

```
[EmbedSubtitle] Embedding subtitles in "<path>"
[Metadata] Adding metadata to "<path>"
[EmbedThumbnail] mutagen: Adding thumbnail to "<path>"     ← mp4 路径
[EmbedThumbnail] ffmpeg: Adding thumbnail to "<path>"      ← mkv 路径
[VideoRemuxer] Remuxing video from mp4 to mkv; Destination: <path>
```

- ⚠️ `[EmbedThumbnail]` 的**中间 token 可变**（`mutagen:` / `ffmpeg:` / `atomicparsley:`）。
  `^\[(\w+)\]\s+(.*)$` 仍能取出 key，但 payload 需**再按第一个空格切一次**
- `[VideoRemuxer]` 会**改变最终文件路径**，`after_move:filepath` 需能覆盖此情形

→ §5.4 状态机的后处理子类型需扩充：
`Merging` / `ExtractingAudio` / `Remuxing` / `EmbeddingSubtitle` / `EmbeddingThumbnail` / `AddingMetadata` / `ConvertingSubtitle`
→ **必须显示「正在嵌入字幕/缩略图」**，理由同 §5.4：remux 大文件要数秒，无提示会被当成卡死

### 14.5 输出目录残留（实测）

- `--write-subs` **显式给出** → `.vtt` 嵌入后**保留**在输出目录
- 只给 `--embed-subs` → 嵌入后删除
- `--embed-thumbnail` 默认**删除**缩略图文件；`--write-thumbnail` 则保留

→ 「嵌入」与「同时保留文件」是**两个独立意图**，UI 需分别暴露，不能只给一个开关。

### 14.6 UI 上的两个易错点

1. **只设 `--sub-langs` 而不启用 `--write-subs`/`--embed-subs`，不会有任何效果**——
   语言选择器必须与「下载/嵌入」开关联动，否则用户以为选了语言就生效了
2. **自动生成字幕需要 `--write-auto-subs`**（YouTube 自动字幕）——仅 `--write-subs` 拿不到

---

## 15. 文件大小：预估与实际分开显示

任务上有两个独立字段，**绝不互相顶替**：

| 字段 | 来源 | 何时有值 |
|---|---|---|
| `size_estimate` | 探测结果的 `requested_downloads` | 探测成功后 |
| `size_actual` | `std::fs::metadata(成品路径).len()` | 下载结束后 |

### 15.1 预估值必须让 **yt-dlp 自己算**

`requested_downloads` 是唯一正确的来源：它是 yt-dlp **按当前 `-f` 表达式实际选中**
的那几条格式，而且体积已经**加好**了（`bv*+ba` → 视频轨 + 音频轨之和）。
自己在宿主侧根据格式表推算等于重新实现一遍 yt-dlp 的选择器，必然对不上。

所以探测时会带上 `-f`。实测带 `-f` 时 `formats` 数组**依然是完整的**，格式表不受影响。

实测的形状（`-f bv*+ba/b`）：

```json
"requested_downloads": [{ "format_id": "248+251", "filesize": null,
                          "filesize_approx": 63600000 }]
```

**⚠️ 合并选择一律落在 `filesize_approx`**，哪怕每一条都有精确 `filesize`
（实测 `-f 137+140`，两条都有精确值，合计仍报在 `filesize_approx`）；
单条选择才可能给 `filesize`。所以先取 `filesize`、再回落 `filesize_approx`。
全部未知时给 `None` 而**不是 0**——0 会被界面显示成「0 B」，那是在撒谎。

### 15.2 ⚠️ 带 `-f` 的探测会整体失败，必须能回退

实测：表达式对该站点不可满足时，`--dump-single-json -f ...` **整个探测都会失败**
（`Requested format is not available`，exit=1，**连 JSON 都没有**）。
如果直接换掉原探测，用户会同时失去元数据和格式表——比现在更糟。

**约定**（`runner::run_probe`）：带 `-f` 探一次；失败则**不带 `-f` 再探一次**。
预估值可以让步，元数据与格式表不能让。只有确认失败原因是
「Requested format is not available」时才追加告警——偶发的网络抖动不该
被说成「你的格式表达式有问题」。

### 15.3 实际大小必须读磁盘

不复用进度里的 `total`：那是下载**前**的预估，而合并音轨、嵌入字幕/缩略图
之后成品会变大，两者经常对不上。下载进程退出后对成品路径 stat 一次才是真值。
归档命中 / 文件已存在（`skipped`）同样走这条——文件本来就在磁盘上。

### 15.4 这个预估能有多准

它只反映**下载下来的音视频轨体积**，不含嵌入物；更重要的是**站点自己给的
`filesize_approx` 有时就是不准的**。

实测同一个 YouTube 链接连续探测，返回的格式集会变：

| 那次的响应 | `requested_downloads` |
|---|---|
| 11 条（HLS，91–96） | `[96]` — **完全没有体积字段** |
| 5 条（含 DASH 的 18） | `[18]` — `filesize_approx: 16637201` |

所以预估值**可能整块缺失**（界面显示「—」），这是正常的，不是 bug。
界面一律写「预估」并说明成品通常会更大（DESIGN §15 开头那张表就是给用户看的）。

---

## 16. 待定（尚未决策）

- SQLite 表结构具体字段
- 错误分类体系（认证失效 / 网络 / 格式不可用 / 磁盘满 / 目标路径不可写 / **Range 不支持** / **已存在跳过** / **嵌入告警**）
- 输出目录组织策略（按站点分目录？按上传者？）
- 弹幕的具体支持范围（B 站弹幕需额外工具，非 yt-dlp 原生）
- 路径长度处理（Windows 260 字符；`%(title).150B` 是按**字节**截断，150 字节 ≈ 50 汉字）
- 国际化 / 快捷键 / 拖拽添加 / 剪贴板监听
- **归档范围**：单一大归档 / 按 profile 分 / 按输出目录分？
- ~~§13.2 的能力缺口~~ → 已定：显式单文件删除按钮（§13.2）
- aria2c 进度解析器（§11.2）的具体实现方式：解析 summary 文本 vs 改走 JSON-RPC
- **容器选择规则的完整定义**（§14.2）：除「嵌入功能开 → mkv」外，`webm` 原始流、仅音频等场景如何取舍
- **嵌入告警的呈现方式**（§14.3）：`--embed-subs` 静默失败要如何让用户确实看见
- **cookie 预检时机**（§6.2）：启动时自动 / 仅在设置页手动？结果是否缓存？
- **cookies.txt 的获取引导**：既然它是主路径（§6 第 5 条），需要一套面向用户的导出指引
