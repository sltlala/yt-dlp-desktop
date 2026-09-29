/**
 * 前端与宿主（Rust）之间的唯一通道。
 *
 * - 在 Tauri 容器里：调用 `src-tauri` 暴露的 command，并监听 `task://update` 事件。
 * - 在浏览器里（`vite dev` 单独跑）：回落到 `mock.ts` 的演示后端，
 *   这样页面设计可以脱离 Rust 后端独立查看。
 */

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { demoFormats, demoTasks, defaultSettings } from './mock'
import type {
  BrowserChoice,
  CodecChoices,
  CookieProfile,
  DataDirInfo,
  FormatOption,
  FormatPreset,
  JsRuntimeInfo,
  Settings,
  Task,
} from './types'

/** 是否运行在 Tauri 容器里。 */
export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

type Listener = (tasks: Task[]) => void

export interface SchedulerStats {
  probing: number
  probeQueued: number
  downloading: number
  downloadQueued: number
  probeLimit: number
  downloadLimit: number
  perHostLimit: number
}

/* ────────────────────── Tauri 后端 ────────────────────── */

/**
 * 用官方的 `@tauri-apps/api`，不要自己拼 `plugin:event|listen`。
 *
 * 手写版本除了容易搞错命令名，还会撞上 Tauri 2 的**权限系统**：
 * 未在 `src-tauri/capabilities/*.json` 声明的 `core:event:allow-listen`
 * 会让监听直接抛错，而调用点没 await → 变成一条静默的 unhandled rejection，
 * 表现为「添加任务没反应」。
 */
const tauriBackend = {
  async onChange(l: Listener): Promise<() => void> {
    const un = await listen<Task[]>('task://update', (e) => l(e.payload))
    // 首帧：主动拉一次，避免等第一次事件才出内容。
    l(await invoke<Task[]>('list_tasks'))
    return un
  },
  addUrl: (url: string) => invoke<Task>('add_url', { url }),
  pause: (id: string) => invoke<void>('pause_task', { id }),
  resume: (id: string) => invoke<void>('resume_task', { id }),
  retry: (id: string) => invoke<void>('resume_task', { id }),
  /** 设定任务的 `-f` 表达式并立刻按新格式重下（「可用格式」对话框的落点）。 */
  setFormat: (id: string, expression: string) =>
    invoke<void>('set_task_format', { id, expression }),
  removeRecord: (id: string) => invoke<void>('remove_record', { id }),
  removeMany: (ids: string[]) => invoke<void>('remove_many', { ids }),
  removeFromArchive: (ids: string[]) => invoke<number>('remove_from_archive', { ids }),
  deleteFile: (id: string) => invoke<void>('delete_file', { id }),
  /** 用系统默认程序打开已下载的文件。文件不存在时后端会返回明确错误。 */
  openFile: (path: string) => invoke<void>('open_file', { path }),
  /** 在资源管理器中选中该文件（不打开它）。 */
  revealFile: (path: string) => invoke<void>('reveal_file', { path }),
  /** 读一次 Windows 的系统代理设置（只用于设置页展示）。 */
  systemProxy: () =>
    invoke<{
      enabled: boolean
      server: string
      bypass: string
      autoConfigUrl: string
      resolved: string | null
      usesPac: boolean
    }>('system_proxy'),
  /** 原生「选择文件夹」对话框。返回 null 表示用户取消。 */
  pickFolder: (initial?: string) => invoke<string | null>('pick_folder', { initial: initial ?? null }),
  /** 原生「选择文件」对话框。返回 null 表示用户取消。 */
  pickFile: (initial?: string) => invoke<string | null>('pick_file', { initial: initial ?? null }),
  /**
   * 探测可用格式。
   *
   * 后端直接返回**解析后的** `MediaInfo`（camelCase，storyboard 已滤掉），
   * 所以这里不再做字段映射——两套解析路径必然分叉（DESIGN §3）。
   */
  probeFormats: async (url: string): Promise<FormatOption[]> => {
    const info = await invoke<{ formats?: FormatOption[] }>('probe_formats', { url })
    return info?.formats ?? []
  },
  getSettings: () => invoke<Settings>('get_settings'),
  saveSettings: (s: Settings) => invoke<void>('save_settings', { settings: s }),
  ytdlpInfo: () => invoke<any>('ytdlp_info'),
  /** aria2c 的落点（随包副本 / AppData / PATH 上哪一份）。 */
  aria2cInfo: () =>
    invoke<{ found: boolean; path: string | null; version: string | null }>('aria2c_info'),
  /** 播放列表勾选完成后开始下载（用 --playlist-items 交给 yt-dlp）。 */
  startPlaylist: (id: string, indices: number[]) =>
    invoke<void>('start_playlist', { id, indices }),
  /** 调度器队列快照，用于底部状态栏显示「探测中 / 排队中」。 */
  schedulerStats: () => invoke<SchedulerStats>('scheduler_stats'),
  /** 全量拉取，供事件通道失效时降级轮询使用。 */
  listTasks: () => invoke<Task[]>('list_tasks'),

  // ── Cookie（DESIGN §6）──
  listCookieProfiles: () => invoke<CookieProfile[]>('list_cookie_profiles'),
  /** 导入一份 cookies.txt。格式错误会带行号返回，不会静默落盘。 */
  importCookieProfile: (name: string, content: string, origin: string) =>
    invoke<CookieProfile>('import_cookie_profile', { name, content, origin }),
  deleteCookieProfile: (id: string) => invoke<void>('delete_cookie_profile', { id }),
  inspectCookieFile: (path: string) => invoke<number>('inspect_cookie_file', { path }),
  testCookieProfile: (id: string) => invoke<ProbeResult>('test_cookie_profile', { id }),
  testCookieBrowser: (browser: string) => invoke<ProbeResult>('test_cookie_browser', { browser }),
  /** 枚举本机装了哪些浏览器、各有哪些 profile（按最近使用排序）。 */
  listBrowsers: () => invoke<BrowserChoice[]>('list_browsers'),
  /** 检测本机可用的 JS 运行时——YouTube 的 n-sig 挑战靠它。 */
  detectJsRuntimes: () => invoke<JsRuntimeInfo>('detect_js_runtimes'),
  /**
   * 读系统剪贴板纯文本，供右键菜单的「粘贴」用。
   *
   * 前端自己读不了：`navigator.clipboard.readText()` 在 WebView2 里会卡在权限
   * 弹窗上，`execCommand('paste')` 恒为 false（见 `src-tauri/src/clipboard.rs`）。
   */
  readClipboard: () => invoke<string>('read_clipboard'),
  /**
   * 「优先选择」的编码候选。**不要在前端另写一份**——白名单在后端，
   * 两边分叉的后果是静默退化（后端丢弃不认识的值，界面上看不出来）。
   */
  codecChoices: () => invoke<CodecChoices>('codec_choices'),
  /**
   * 格式选择器的预设列表。表达式由后端按**当前编码偏好**生成，
   * 所以从选择器里点「1080p」不会把设置里的偏好丢掉。
   */
  formatPresets: () => invoke<FormatPreset[]>('format_presets'),
  /** 数据目录（设置/历史/cookies 所在处）+ 是否便携模式。 */
  dataDir: () => invoke<DataDirInfo>('data_dir'),
  /**
   * 把「编辑中的设置」翻译成实际会用的 `-f` 表达式。
   *
   * 前端**不要自己拼**：`-f` 是存进数据库的硬契约，拼两份必然分叉
   * （曾经把预设表抄进 `FormatPicker.vue` 就是这个毛病）。
   */
  previewFormatExpression: (settings: Settings) =>
    invoke<string>('preview_format_expression', { settings }),

  // ── 代理与更新（DESIGN §7、§8）──
  /** 真发一次请求验证代理——只校验格式的话，代理没启动也会显示「格式正确」。 */
  testProxy: () => invoke<ProbeResult>('test_proxy'),
  checkYtdlpUpdate: () => invoke<UpdateInfo>('check_ytdlp_update'),
  applyYtdlpUpdate: () => invoke<string>('apply_ytdlp_update'),
}

export interface ProbeResult {
  ok: boolean
  summary: string
}

export interface UpdateInfo {
  current: string
  latest: string
  newer: boolean
  currentPath: string | null
  assetName: string | null
  assetSize: number | null
  notesUrl: string | null
}

/* ────────────────────── 浏览器演示后端 ────────────────────── */

/**
 * 开发期后端：用定时器推进演示任务的进度，让界面「活」起来。
 *
 * 同时刻意复现了几条真实边界：
 * - 总大小未知 → 进度条进入不确定态，**不显示 0%**（HANDOFF §3.2）
 * - `finished` 之后仍处于后处理 → 必须显示「正在嵌入字幕」（DESIGN §5.4）
 */
class MockBackend {
  private tasks: Task[] = structuredClone(demoTasks)
  private settings: Settings = structuredClone(defaultSettings)
  private listeners = new Set<Listener>()

  constructor() {
    // 只需持续推进，无需持有句柄（应用生命周期内一直运行）。
    window.setInterval(() => this.tick(), 500)
  }

  private tick() {
    let changed = false
    for (const t of this.tasks) {
      if (t.state === 'downloading') {
        const total = t.progress.total
        const done = t.progress.downloaded ?? 0
        if (total === null) {
          // 不确定态：只推进已下载字节，不编造 total。
          t.progress.downloaded = done + 2_500_000
          changed = true
          continue
        }
        const next = Math.min(total, done + (t.progress.speed ?? 4_000_000) * 0.5)
        t.progress.downloaded = next
        t.progress.speed = (t.progress.speed ?? 4_000_000) * (0.96 + Math.random() * 0.08)
        t.progress.eta = next >= total ? 0 : Math.round((total - next) / t.progress.speed)
        if (next >= total) {
          // finished ≠ 完成：后面还有合并/嵌入（HANDOFF §3.2）。
          t.state = 'postprocessing'
          t.postProcess = 'Merger'
        }
        changed = true
      } else if (t.state === 'postprocessing') {
        const order: Task['postProcess'][] = [
          'Merger',
          'VideoRemuxer',
          'EmbedSubtitle',
          'EmbedThumbnail',
          'Metadata',
        ]
        const i = order.indexOf(t.postProcess)
        if (i >= 0 && i < order.length - 1 && Math.random() > 0.55) {
          t.postProcess = order[i + 1]
        } else if (i === order.length - 1 && Math.random() > 0.6) {
          t.state = 'completed'
          t.postProcess = null
          t.finishedAt = Date.now()
          t.filepath = `${t.outputDir}\\${t.title.slice(0, 40)} [${t.id.slice(-4)}].${t.container}`
        }
        changed = true
      }
    }
    if (changed) this.emit()
  }

  private emit() {
    const snapshot = structuredClone(this.tasks)
    for (const l of this.listeners) l(snapshot)
  }

  onChange(l: Listener): () => void {
    this.listeners.add(l)
    l(structuredClone(this.tasks))
    return () => this.listeners.delete(l)
  }

  async addUrl(url: string): Promise<Task> {
    const id = `t-${Math.random().toString(16).slice(2, 6)}`
    const host = (() => {
      try {
        return new URL(url).hostname.replace(/^www\./, '')
      } catch {
        return 'unknown'
      }
    })()
    const extractor =
      host.includes('bilibili') ? 'BiliBili'
      : host.includes('youtube') || host.includes('youtu.be') ? 'YouTube'
      : host.includes('vimeo') ? 'Vimeo'
      : host

    const task: Task = {
      id,
      url,
      title: `正在解析 ${host} 的链接…`,
      extractor,
      thumbnail: null,
      durationSec: null,
      state: 'probing',
      postProcess: null,
      skipReason: null,
      progress: { downloaded: null, total: null, speed: null, eta: null },
      filepath: null,
      // 刚建的任务还没探测，大小未知
      sizeEstimate: null,
      sizeActual: null,
      formatExpression: 'bv*+ba/b',
      container: 'mkv',
      outputDir: this.settings.outputDir,
      error: null,
      warnings: [],
      addedAt: Date.now(),
      finishedAt: null,
    }
    this.tasks.unshift(task)
    this.emit()

    // 模拟探测完成。
    window.setTimeout(() => {
      const t = this.tasks.find((x) => x.id === id)
      if (!t) return
      t.title = `新添加的视频 ${id.slice(-4).toUpperCase()}`
      t.durationSec = 300 + Math.floor(Math.random() * 3000)
      t.thumbnail = demoTasks[0].thumbnail
      t.state = 'downloading'
      t.progress = {
        downloaded: 0,
        total: 200_000_000 + Math.floor(Math.random() * 800_000_000),
        speed: 5_000_000,
        eta: 60,
      }
      this.emit()
    }, 1600)

    return task
  }

  /** 删除范围刻意分开，不合成一个按钮（DESIGN §13）。 */
  async removeRecord(id: string): Promise<void> {
    this.tasks = this.tasks.filter((t) => t.id !== id)
    this.emit()
  }

  async removeMany(ids: string[]): Promise<void> {
    const set = new Set(ids)
    this.tasks = this.tasks.filter((t) => !set.has(t.id))
    this.emit()
  }

  async removeFromArchive(ids: string[]): Promise<number> {
    // 只解除「允许重新下载」，不删记录、不删文件。
    const set = new Set(ids)
    let n = 0
    for (const t of this.tasks) {
      if (set.has(t.id) && t.skipReason) {
        t.skipReason = null
        n++
      }
    }
    this.emit()
    return n
  }

  async deleteFile(id: string): Promise<void> {
    const t = this.tasks.find((x) => x.id === id)
    if (t) t.filepath = null
    this.emit()
  }

  /** 演示模式没有真实文件系统，给一个可预期的提示而不是静默成功。 */
  async openFile(_path: string): Promise<void> {
    throw new Error('演示模式没有真实文件，无法打开（用 npm run tauri:dev 跑桌面版）')
  }

  async revealFile(_path: string): Promise<void> {
    throw new Error('演示模式没有真实文件，无法定位（用 npm run tauri:dev 跑桌面版）')
  }

  /** 演示模式没有原生对话框，返回 null 表示「用户取消」，界面行为保持一致。 */
  async pickFolder(_initial?: string): Promise<string | null> {
    return null
  }

  async pickFile(_initial?: string): Promise<string | null> {
    return null
  }

  /** 演示模式没有注册表可读，返回「系统未启用代理」。 */
  async systemProxy() {
    return {
      enabled: false,
      server: '',
      bypass: '',
      autoConfigUrl: '',
      resolved: null,
      usesPac: false,
    }
  }

  async pause(id: string): Promise<void> {
    const t = this.tasks.find((x) => x.id === id)
    if (t && (t.state === 'downloading' || t.state === 'postprocessing')) {
      t.state = 'paused'
      t.progress.speed = null
      t.progress.eta = null
      this.emit()
    }
  }

  async resume(id: string): Promise<void> {
    const t = this.tasks.find((x) => x.id === id)
    if (t && (t.state === 'paused' || t.state === 'failed')) {
      t.state = 'downloading'
      t.error = null
      this.emit()
    }
  }

  async retry(id: string): Promise<void> {
    return this.resume(id)
  }

  async setFormat(id: string, expression: string): Promise<void> {
    const t = this.tasks.find((x) => x.id === id)
    if (!t) return
    t.formatExpression = expression
    t.formatOverride = expression.trim() || null
    t.state = 'downloading'
    t.error = null
    this.emit()
  }

  async probeFormats(_url: string): Promise<FormatOption[]> {
    return structuredClone(demoFormats)
  }

  async getSettings(): Promise<Settings> {
    return structuredClone(this.settings)
  }

  async saveSettings(s: Settings): Promise<void> {
    this.settings = structuredClone(s)
  }

  async ytdlpInfo(): Promise<any> {
    return { found: true, path: '(演示模式)', version: '2026.07.04', versionLooksValid: true }
  }

  async aria2cInfo() {
    return { found: true, path: '(演示模式)\\aria2c.exe', version: 'aria2 version 1.37.0' }
  }

  async listTasks(): Promise<Task[]> {
    return structuredClone(this.tasks)
  }

  async startPlaylist(id: string, indices: number[]): Promise<void> {
    const t = this.tasks.find((x) => x.id === id)
    if (!t) return
    t.playlistItems = indices.map((i) => i + 1).join(',')
    t.state = 'queued'
    t.queueHint = '排队中'
    this.emit()
  }

  async schedulerStats(): Promise<SchedulerStats> {
    const active = this.tasks.filter((t) =>
      ['probing', 'downloading', 'queued'].includes(t.state),
    ).length
    return {
      probing: this.tasks.filter((t) => t.state === 'probing').length,
      probeQueued: 0,
      downloading: this.tasks.filter((t) => t.state === 'downloading').length,
      downloadQueued: Math.max(0, active - 1),
      probeLimit: this.settings.probeConcurrency,
      downloadLimit: this.settings.downloadConcurrency,
      perHostLimit: this.settings.perHostConcurrency,
    }
  }

  // ── Cookie（演示模式：复现本机实测的浏览器结论）──

  private cookieProfiles: CookieProfile[] = [
    {
      id: 'demo1',
      name: 'B站主账号',
      source: 'file',
      origin: 'bilibili-cookies.txt',
      cookieCount: 88,
      createdAt: Date.now() - 86_400_000,
    },
  ]

  async listCookieProfiles(): Promise<CookieProfile[]> {
    return structuredClone(this.cookieProfiles)
  }

  async importCookieProfile(
    name: string,
    content: string,
    origin: string,
  ): Promise<CookieProfile> {
    if (!content.includes('\t')) {
      throw new Error('文件里没有有效的 cookie 行。\n请确认导出的是 Netscape 格式。')
    }
    const p: CookieProfile = {
      id: 'demo' + Math.random().toString(16).slice(2, 6),
      name: name.trim() || '未命名',
      source: 'file',
      origin,
      cookieCount: content.split('\n').filter((l) => l.includes('\t')).length,
      createdAt: Date.now(),
    }
    this.cookieProfiles.push(p)
    return p
  }

  async deleteCookieProfile(id: string): Promise<void> {
    this.cookieProfiles = this.cookieProfiles.filter((p) => p.id !== id)
  }

  async inspectCookieFile(_path: string): Promise<number> {
    return 88
  }

  async testCookieProfile(id: string): Promise<ProbeResult> {
    const p = this.cookieProfiles.find((x) => x.id === id)
    if (!p) return { ok: false, summary: '✘ 未找到该 profile' }
    return { ok: true, summary: `✔ ${p.name} 可用（${p.cookieCount} 条 cookie）` }
  }

  async testCookieBrowser(browser: string): Promise<ProbeResult> {
    // 与 Windows 上的实测结论一致：只有 Firefox 可用
    if (browser.startsWith('firefox')) {
      return { ok: true, summary: '✔ 已从 firefox 提取 161 条 cookie' }
    }
    if (browser.startsWith('chrome')) {
      return {
        ok: false,
        summary:
          '✘ cookie 数据库被占用。请**完全退出**该浏览器后重试（托盘图标也要退），然后重新测试。',
      }
    }
    return {
      ok: false,
      summary:
        '✘ 该浏览器的 cookie 已被系统加密保护，yt-dlp 无法读取。\n**关闭浏览器也无法解决**，请改用 cookies.txt 方式导入。',
    }
  }

  async listBrowsers(): Promise<BrowserChoice[]> {    const now = Date.now()
    return [
      {
        name: 'firefox',
        label: 'Firefox',
        installed: true,
        supportsProfiles: true,
        chromium: false,
        recommendedProfile: '9mz7ax6i.default-release',
        profiles: [
          {
            id: '9mz7ax6i.default-release',
            label: '9mz7ax6i.default-release',
            lastUsed: now - 3_600_000,
          },
          { id: 'rxtgze5d.default', label: 'rxtgze5d.default', lastUsed: now - 86_400_000 },
        ],
      },
      {
        name: 'chrome',
        label: 'Google Chrome',
        installed: true,
        supportsProfiles: true,
        chromium: true,
        recommendedProfile: 'Default',
        profiles: [{ id: 'Default', label: '默认 (Default)', lastUsed: now - 7_200_000 }],
      },
      {
        name: 'edge',
        label: 'Microsoft Edge',
        installed: true,
        supportsProfiles: true,
        chromium: true,
        recommendedProfile: null,
        profiles: [],
      },
      {
        name: 'safari',
        label: 'Safari',
        installed: false,
        supportsProfiles: false,
        chromium: false,
        recommendedProfile: null,
        profiles: [],
      },
    ]
  }

  async codecChoices(): Promise<CodecChoices> {
    // ⚠️ 镜像 `ytdlp_core::VIDEO_CODEC_CHOICES` / `AUDIO_CODEC_CHOICES`。
    // 真源在 Rust——值必须与 yt-dlp 实际报出的编码名前缀一致（`avc1` 而不是
    // `h264`），写错不会报错，只会静默退化成「没有偏好」。
    return {
      video: [
        { value: '', label: '不指定' },
        { value: 'avc1', label: 'H.264 / AVC' },
        { value: 'vp9', label: 'VP9' },
        { value: 'av01', label: 'AV1' },
      ],
      audio: [
        { value: '', label: '不指定' },
        { value: 'mp4a', label: 'AAC / m4a' },
        { value: 'opus', label: 'Opus' },
        { value: 'vorbis', label: 'Vorbis' },
      ],
    }
  }

  async formatPresets(): Promise<FormatPreset[]> {
    const expr = (patch: Partial<Settings>) =>
      this.previewFormatExpression({ ...this.settings, ...patch })
    return [
      { label: '最佳画质', expr: await expr({ preset: 'best' }), note: '自动挑最优视频轨与音频轨' },
      {
        label: '1080p',
        expr: await expr({ preset: 'maxHeight', maxHeight: 1080 }),
        note: '不超过 1920×1080',
      },
      {
        label: '720p',
        expr: await expr({ preset: 'maxHeight', maxHeight: 720 }),
        note: '省流量',
      },
      {
        label: '仅音频 MP3',
        expr: await expr({ preset: 'audioOnly' }),
        note: '提取音频并转码',
      },
    ]
  }

  async previewFormatExpression(settings: Settings): Promise<string> {
    // 镜像 `ytdlp_core::preset_expression_with`；真源在 Rust，见上面的说明。
    const v = settings.preferVcodec ? `[vcodec^=${settings.preferVcodec}]` : ''
    const a = settings.preferAcodec ? `[acodec^=${settings.preferAcodec}]` : ''
    if (settings.preset === 'audioOnly') return a ? `ba${a}/ba/b` : 'ba/b'
    const h = settings.preset === 'maxHeight' ? `[height<=${settings.maxHeight}]` : ''
    const tiers: string[] = []
    if (v && a) tiers.push(`bv*${h}${v}+ba${a}`)
    if (v) tiers.push(`bv*${h}${v}+ba`)
    if (a) tiers.push(`bv*${h}+ba${a}`)
    tiers.push(`bv*${h}+ba`)
    tiers.push(h ? `b${h}/b` : 'b')
    return tiers.join('/')
  }

  async readClipboard(): Promise<string> {
    // mock 里没有真剪贴板；返回空串 → 右键菜单的「粘贴」点了没反应，
    // 但那只是浏览器里跑 mock 时的表现，不影响桌面版。
    return ''
  }

  async dataDir(): Promise<DataDirInfo> {
    // mock 里没有真实目录，给一个看得懂的位置即可
    return { root: 'C:\\Users\\demo\\AppData\\Roaming\\ytdlp-desktop', portable: false, marker: 'portable.txt' }
  }

  async detectJsRuntimes(): Promise<JsRuntimeInfo> {
    return {
      detected: ['node', 'bun'],
      candidates: [
        { name: 'node', path: 'D:\\Program_software\\nodejs\\node.exe' },
        { name: 'deno', path: null },
        { name: 'bun', path: 'C:\\Users\\slt\\.cherrystudio\\bin\\bun.exe' },
        { name: 'quickjs', path: null },
      ],
    }
  }

  async testProxy(): Promise<ProbeResult> {
    const st = this.settings
    if (st.proxyMode === 'none') {
      return { ok: false, summary: '当前选择「不使用代理」。' }
    }
    if (st.proxyMode === 'system') {
      return { ok: false, summary: '演示模式读不到注册表，无法测试系统代理。' }
    }
    if (!st.proxyHost.trim()) {
      return { ok: false, summary: '还没填主机名。' }
    }
    await new Promise((r) => setTimeout(r, 400))
    const scheme = st.proxyProtocol === 'socks5' ? 'socks5h' : 'http'
    return { ok: true, summary: `✔ 代理可用（${scheme}://${st.proxyHost}:${st.proxyPort}，12 ms）（演示）` }
  }

  async checkYtdlpUpdate(): Promise<UpdateInfo> {
    return {
      current: '2026.07.04',
      latest: '2026.08.19',
      newer: true,
      currentPath: '(演示模式)',
      assetName: 'yt-dlp.exe',
      assetSize: 17_840_399,
      notesUrl: 'https://github.com/yt-dlp/yt-dlp/releases',
    }
  }

  async applyYtdlpUpdate(): Promise<string> {
    return '演示模式不执行更新'
  }
}

let backend: MockBackend | null = null

function mock(): MockBackend {
  if (!backend) backend = new MockBackend()
  return backend
}

/**
 * 对外统一接口。两个实现签名一致，界面代码无需感知运行环境。
 */
export const api = isTauri()
  ? tauriBackend
  : {
      onChange: (l: Listener) => Promise.resolve(mock().onChange(l)),
      addUrl: (url: string) => mock().addUrl(url),
      pause: (id: string) => mock().pause(id),
      resume: (id: string) => mock().resume(id),
      retry: (id: string) => mock().retry(id),
      setFormat: (id: string, expression: string) => mock().setFormat(id, expression),
      removeRecord: (id: string) => mock().removeRecord(id),
      removeMany: (ids: string[]) => mock().removeMany(ids),
      removeFromArchive: (ids: string[]) => mock().removeFromArchive(ids),
      deleteFile: (id: string) => mock().deleteFile(id),
      openFile: (path: string) => mock().openFile(path),
      revealFile: (path: string) => mock().revealFile(path),
      pickFolder: (initial?: string) => mock().pickFolder(initial),
      pickFile: (initial?: string) => mock().pickFile(initial),
      systemProxy: () => mock().systemProxy(),
      probeFormats: (url: string) => mock().probeFormats(url),
      getSettings: () => mock().getSettings(),
      saveSettings: (s: Settings) => mock().saveSettings(s),
      ytdlpInfo: () => mock().ytdlpInfo(),
      aria2cInfo: () => mock().aria2cInfo(),
      listTasks: () => mock().listTasks(),
      startPlaylist: (id: string, indices: number[]) => mock().startPlaylist(id, indices),
      schedulerStats: () => mock().schedulerStats(),
      listCookieProfiles: () => mock().listCookieProfiles(),
      importCookieProfile: (name: string, content: string, origin: string) =>
        mock().importCookieProfile(name, content, origin),
      deleteCookieProfile: (id: string) => mock().deleteCookieProfile(id),
      inspectCookieFile: (path: string) => mock().inspectCookieFile(path),
      testCookieProfile: (id: string) => mock().testCookieProfile(id),
      testCookieBrowser: (browser: string) => mock().testCookieBrowser(browser),
      listBrowsers: () => mock().listBrowsers(),
      detectJsRuntimes: () => mock().detectJsRuntimes(),
      readClipboard: () => mock().readClipboard(),
      codecChoices: () => mock().codecChoices(),
      formatPresets: () => mock().formatPresets(),
      dataDir: () => mock().dataDir(),
      previewFormatExpression: (s: Settings) => mock().previewFormatExpression(s),
      testProxy: () => mock().testProxy(),
      checkYtdlpUpdate: () => mock().checkYtdlpUpdate(),
      applyYtdlpUpdate: () => mock().applyYtdlpUpdate(),
    }
