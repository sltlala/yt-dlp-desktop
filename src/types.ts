/**
 * 与 Rust 核心层（`crates/ytdlp-core`）对应的类型定义。
 *
 * 刻意与 `TaskState` / `Progress` / `Event` 保持同构，避免两边语义漂移。
 */

/** 任务状态机（DESIGN §5.4）。 */
export type TaskStateKind =
  | 'pending'
  | 'probing'
  /** 探测出是播放列表，等用户勾选要下载哪几集（DESIGN §9）。 */
  | 'selecting'
  /** 已探测完成，等待调度器分配下载槽位（受并发/每域名限制）。 */
  | 'queued'
  | 'downloading'
  | 'postprocessing'
  | 'completed'
  /** 归档命中或文件已存在 —— yt-dlp 返回 exit=0，必须与 completed 区分（§13.1）。 */
  | 'skipped'
  | 'failed'
  | 'paused'
  | 'canceled'

export type SkipReason = 'archive' | 'fileExists'

/** 后处理子类型，用于「正在合并 / 正在嵌入字幕」等文案（§14.4）。 */
export type PostProcessKind =
  | 'Merger'
  | 'ExtractAudio'
  | 'EmbedSubtitle'
  | 'EmbedThumbnail'
  | 'Metadata'
  | 'VideoRemuxer'
  | 'SubtitlesConvertor'
  | 'ThumbnailsConvertor'
  | 'VideoConvertor'
  | 'SplitChapters'
  | 'Fixup'
  | 'Exec'

export const POST_PROCESS_LABEL: Record<PostProcessKind, string> = {
  Merger: '正在合并音视频',
  ExtractAudio: '正在提取音频',
  EmbedSubtitle: '正在嵌入字幕',
  EmbedThumbnail: '正在嵌入缩略图',
  Metadata: '正在写入元数据',
  VideoRemuxer: '正在转封装',
  SubtitlesConvertor: '正在转换字幕',
  ThumbnailsConvertor: '正在转换缩略图',
  VideoConvertor: '正在转换视频',
  SplitChapters: '正在切分章节',
  Fixup: '正在修复容器',
  Exec: '正在执行后处理',
}

export const TASK_STATE_LABEL: Record<TaskStateKind, string> = {
  pending: '等待中',
  probing: '正在解析',
  selecting: '待选择',
  queued: '排队中',
  downloading: '下载中',
  postprocessing: '后处理',
  completed: '已完成',
  skipped: '已跳过',
  failed: '失败',
  paused: '已暂停',
  canceled: '已取消',
}

/** 这些状态算「进行中」，侧栏计数与筛选都用它。 */
export const ACTIVE_STATES: TaskStateKind[] = [
  'pending',
  'probing',
  'selecting',
  'queued',
  'downloading',
  'postprocessing',
  'paused',
]

export interface Progress {
  /** 已完成字节。 */
  downloaded: number | null
  /** 总字节。已按 total_bytes → total_bytes_estimate 回落；为 null 表示未知。 */
  total: number | null
  speed: number | null
  eta: number | null
}

export interface Task {
  id: string
  url: string
  title: string
  /** 站点名，用于列表里的来源徽标。 */
  extractor: string
  /** 缩略图 URL；本地 demo 用内联 SVG。 */
  thumbnail: string | null
  durationSec: number | null
  state: TaskStateKind
  postProcess: PostProcessKind | null
  skipReason: SkipReason | null
  progress: Progress
  /** 已落地的最终文件路径。 */
  filepath: string | null
  /** `-f` 表达式（当前生效的那个）。**存表达式而非 format_id**（DESIGN §3）。 */
  formatExpression: string
  /**
   * 用户在「可用格式」里显式选定的表达式。
   *
   * `null` / 缺省 = 跟随设置里的预设；只有非空才覆盖。
   */
  formatOverride?: string | null
  container: 'mp4' | 'mkv' | 'webm'
  outputDir: string
  error: string | null
  /** 后处理告警（如「ASS 字幕无法正确嵌入 mp4」）。 */
  warnings: string[]
  addedAt: number
  finishedAt: number | null

  /** 探测到的可用格式。有值时格式选择器直接用，不必再探一次。 */
  formats?: FormatOption[]
  /** 该视频有哪些字幕语言（含自动生成）。 */
  subtitleLangs?: string[]
  /** 播放列表条目；非空表示处于 `selecting` 状态。 */
  playlistEntries?: PlaylistEntry[]
  /** 勾选结果对应的 `--playlist-items`。 */
  playlistItems?: string | null
  /** 排队提示文案，如「等待探测」「排队中」。 */
  queueHint?: string | null
  /**
   * 本任务的进度是否来自 aria2c 自身上报（DESIGN §11.2）。
   * 用 aria2c 时 yt-dlp 一条进度都不发，进度只能解析 aria2c 的输出，精度略低。
   */
  usedAria2c?: boolean
}

export interface PlaylistEntry {
  id: string
  title: string
  url: string | null
  duration: number | null
  thumbnail: string | null
}

export type PresetKind = 'best' | 'maxHeight' | 'audioOnly'

export interface EmbedSettings {
  subs: boolean
  subLangs: string
  autoSubs: boolean
  keepSubFiles: boolean
  thumbnail: boolean
  keepThumbnailFile: boolean
  metadata: boolean
  chapters: boolean
  infoJson: boolean
}

export interface Settings {
  outputDir: string
  tempDir: string
  /** 探测并发（轻量，可高）。 */
  probeConcurrency: number
  /** 下载并发（重，受站点限流）。 */
  downloadConcurrency: number
  /** 同一站点同时下载数。 */
  perHostConcurrency: number
  preset: PresetKind
  maxHeight: number
  audioFormat: 'mp3' | 'm4a' | 'opus' | 'flac' | 'wav'
  container: 'auto' | 'mp4' | 'mkv' | 'webm'
  embed: EmbedSettings
  /**
   * Cookie 来源（DESIGN §6）：
   * - `none`     不使用
   * - `profile`  用已导入的 profile（**多账号推荐**，宿主管路径）
   * - `file`     直接用某个 cookies.txt 路径
   * - `browser`  从浏览器读取（Windows 上实测仅 Firefox 可用）
   */
  cookieMode: 'none' | 'profile' | 'file' | 'browser'
  cookieProfileId: string
  cookieFile: string
  /**
   * 完整的 `--cookies-from-browser` 取值，形如 `firefox` 或
   * `chrome:Profile 1`。语法是 `BROWSER[+KEYRING][:PROFILE][::CONTAINER]`。
   */
  cookieBrowser: string
  /**
   * 代理模式（对应 Windows「设置 → 网络和 Internet → 代理」那一页）：
   * - `none`    不使用代理
   * - `system`  跟随系统代理（宿主读注册表）
   * - `manual`  手动配置
   */
  proxyMode: 'none' | 'system' | 'manual'
  /** `http` 或 `socks5`（宿主实际传 `socks5h`，DNS 也走代理）。 */
  proxyProtocol: 'http' | 'socks5'
  proxyHost: string
  proxyPort: number
  proxyAuth: boolean
  proxyUser: string
  proxyPassword: string
  /** 不勾时密码只留在内存，**不会写进 config.json**。 */
  proxyRemember: boolean
  aria2c: boolean
  archiveEnabled: boolean
  archivePath: string
  limitRate: string
  filenameTemplate: string
  /**
   * JS 运行时（逗号分隔，如 `node` 或 `node,bun`）。
   * **留空表示自动检测**。
   *
   * 不是可选优化：yt-dlp 不会自动启用已安装的运行时，而 YouTube 的
   * n-sig 挑战需要它——不给就会返回「需要重载页面」或只给 storyboard。
   */
  jsRuntime: string
  /**
   * 「优先选择」的视频编码，空串 = 不指定。
   *
   * 值必须是 **yt-dlp 实际报出的编码名前缀**（`avc1` / `vp9` / `av01`），
   * 白名单在 `ytdlp_core::VIDEO_CODEC_CHOICES`。界面的选择项由后端生成，
   * 不要在这里另写一份。
   */
  preferVcodec: string
  /** 「优先选择」的音频编码（`mp4a` / `opus` / `vorbis`）。 */
  preferAcodec: string
}

/** 一个可选的编码偏好。 */
export interface CodecChoice {
  /** 空串表示「不指定」。 */
  value: string
  label: string
}

/** 「优先选择」的两组候选，由后端白名单生成。 */
export interface CodecChoices {
  video: CodecChoice[]
  audio: CodecChoice[]
}

/** 格式选择器里的一条预设。`expr` 由后端按当前编码偏好算好。 */
export interface FormatPreset {
  label: string
  note: string
  expr: string
}

/** JS 运行时检测结果。 */
export interface JsRuntimeInfo {
  /** 实际检测到的运行时名，按偏好排序。 */
  detected: string[]
  /** yt-dlp 认识的全部候选，以及各自在本机的路径（未安装为 null）。 */
  candidates: { name: string; path: string | null }[]
}

/** 浏览器的某个 profile。 */
export interface BrowserProfile {
  id: string
  label: string
  /** cookie 库最后修改时间（毫秒），用于显示「最近用过」。 */
  lastUsed: number | null
}

/** 界面上可选的一个浏览器（由后端探测本机实际情况得出）。 */
export interface BrowserChoice {
  name: string
  label: string
  /** 本机是否装了。 */
  installed: boolean
  /** Opera / Safari 不支持 profile 选择。 */
  supportsProfiles: boolean
  /**
   * 是否 Chromium 系。
   *
   * 界面据此**只在选中这类浏览器时**提示「Windows 上多半读不到」。
   * 以后端下发的字段为准，不在前端猜名字——否则这份判断会和后端分叉。
   */
  chromium: boolean
  /** 最近用过的在前。 */
  profiles: BrowserProfile[]
  recommendedProfile: string | null
}

/** 已导入的 cookie profile。**不含 cookie 内容**——凭证不回传前端。 */
export interface CookieProfile {
  id: string
  name: string
  source: string
  origin: string
  cookieCount: number
  createdAt: number
}

/** 探测结果中的一条可选格式（高级模式用）。 */
export interface FormatOption {
  formatId: string
  ext: string
  resolution: string
  fps: number | null
  vcodec: string | null
  acodec: string | null
  filesize: number | null
  tbr: number | null
  note: string
}
