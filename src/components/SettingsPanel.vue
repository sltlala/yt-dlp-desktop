<script setup lang="ts">
import { computed, nextTick, onMounted, reactive, ref, watch } from 'vue'
import { useTaskStore } from '../stores/tasks'
import { api, type ProbeResult, type UpdateInfo } from '../ipc'
import type { BrowserChoice, CodecChoices, CookieProfile, JsRuntimeInfo, Settings } from '../types'
import { deepClone, ellipsizePath, fmtTime } from '../utils'

const emit = defineEmits<{ close: [] }>()
const store = useTaskStore()

// store.settings 是 Vue Proxy，必须用 deepClone 而非 structuredClone。
const s = reactive<Settings>(deepClone(store.settings!))
const tab = ref<'general' | 'format' | 'embed' | 'network' | 'download'>('general')
const saved = ref(false)

/* ───────────────────── Cookie（DESIGN §6）───────────────────── */

const profiles = ref<CookieProfile[]>([])
/** 每个 profile 的预检结果，按 id 存。 */
const profileTest = ref<Record<string, ProbeResult>>({})
const browserTest = ref<ProbeResult | null>(null)
const testing = ref(false)
const importError = ref<string | null>(null)
const importing = ref(false)
const fileInput = ref<HTMLInputElement | null>(null)

/* ── 浏览器选择 ──
 *
 * 不写死下拉项：后端按 yt-dlp 自己的路径映射探测本机装了哪些浏览器、
 * 各有哪些 profile。同一个浏览器常有多套登录态（工作/个人），
 * 选错 profile 会得到一个「没登录」的假象。
 */
const browsers = ref<BrowserChoice[]>([])
const selBrowser = ref('firefox')
const selProfile = ref('')

const curBrowser = computed(() => browsers.value.find((b) => b.name === selBrowser.value))

/**
 * 选中 profile 的「上次使用」提示。
 *
 * 有用：用户常有多个 profile，选错那个会得到一个「没登录」的假象；
 * 显示上次使用时间能立刻看出选的是不是自己正在用的那个。
 */
const selProfileInfo = computed(() => {
  const p = curBrowser.value?.profiles.find((x) => x.id === selProfile.value)
  if (!p?.lastUsed) return null
  const days = Math.floor((Date.now() - p.lastUsed) / 86_400_000)
  const ago = days === 0 ? '今天' : days === 1 ? '昨天' : `${days} 天前`
  return `该配置文件最近使用：${fmtTime(p.lastUsed)}（${ago}）`
})

/** 拼 `--cookies-from-browser` 的值。规则与 Rust 侧 `build_browser_spec` 一致。 */
function buildSpec(browser: string, profile: string): string {
  const p = profile.trim()
  return p ? `${browser}:${p}` : browser
}

/** 从已保存的 spec 反解出浏览器与 profile。 */
function syncFromSpec() {
  const spec = (s.cookieBrowser || 'firefox').trim()
  const head = spec.split('::')[0] // 丢掉 ::CONTAINER
  const idx = head.indexOf(':')
  // 丢掉 +KEYRING
  const name = (idx >= 0 ? head.slice(0, idx) : head).split('+')[0]
  selBrowser.value = name || 'firefox'

  const explicit = idx >= 0 ? head.slice(idx + 1) : ''
  // 没写 profile 时，yt-dlp 自己会挑**最近用过**的那个。
  // 这里把它显式化：**界面显示的应当就是实际会用的**，
  // 否则用户看到一个空的 profile 会以为「没用任何 profile」。
  selProfile.value = explicit || curBrowser.value?.recommendedProfile || ''
  if (!explicit && selProfile.value) {
    syncToSpec()
  }
}

function syncToSpec() {
  s.cookieBrowser = buildSpec(selBrowser.value, selProfile.value)
}

/** 换浏览器时自动选它最近用过的 profile——几乎总是用户想要的那个。 */
function onBrowserChange(e: Event) {
  selBrowser.value = (e.target as HTMLSelectElement).value
  selProfile.value = curBrowser.value?.recommendedProfile ?? ''
  syncToSpec()
  browserTest.value = null
}

async function loadBrowsers() {
  try {
    browsers.value = await api.listBrowsers()
    syncFromSpec()
  } catch (e) {
    importError.value = `读取浏览器列表失败：${String(e)}`
  }
}

async function loadProfiles() {
  try {
    profiles.value = await api.listCookieProfiles()
  } catch (e) {
    importError.value = `读取 cookie 列表失败：${String(e)}`
  }
}

onMounted(() => {
  void loadProfiles()
  void loadBrowsers()
  void loadJsRuntimes()
  void loadAria2c()
  void loadCodecChoices()
  // 选「跟随系统代理」时要把系统那份读出来展示，进页面就先取一次
  if (s.proxyMode === 'system') void loadSystemProxy()
})

/* ── 优先选择编码（DESIGN §3.3）──
 *
 * 候选列表**由后端白名单生成**，前端不另写一份：值必须与 yt-dlp 报出的
 * 编码名一致（`avc1` 而非 `h264`），而白名单外的值会被后端静默丢弃——
 * 界面上完全看不出来。详见 `CodecPreference` 的说明。
 */
const codecChoices = ref<CodecChoices>({ video: [], audio: [] })

async function loadCodecChoices() {
  try {
    codecChoices.value = await api.codecChoices()
  } catch {
    /* 取不到就只剩「不指定」一项，不影响已有设置 */
  }
}

/**
 * 实际会传给 `-f` 的表达式。**由后端算**，前端不自己拼——
 * 表达式是存进数据库的硬契约（DESIGN §3），拼两份必然分叉。
 */
const codecPreview = ref('')

watch(
  () => [s.preset, s.maxHeight, s.audioFormat, s.preferVcodec, s.preferAcodec],
  async () => {
    try {
      codecPreview.value = await api.previewFormatExpression(deepClone(s))
    } catch {
      codecPreview.value = ''
    }
  },
  { immediate: true },
)

/**
 * 容器与编码的组合提示。
 *
 * 不去阻止用户——偏好本来就是「能满足最好，不能满足就算了」，
 * 而且 yt-dlp 合并时 ffmpeg 往往也能凑合。只把后果说清楚。
 */
const codecCompatHint = computed(() => {
  const v = s.preferVcodec
  const a = s.preferAcodec
  const c = s.container
  if (c === 'mp4' && (v === 'vp9' || v === 'av01' || a === 'opus' || a === 'vorbis')) {
    return 'MP4 与 VP9 / AV1 / Opus 的兼容性较差，部分播放器放不出来；想要这些编码建议用 MKV 或自动。'
  }
  if (c === 'webm' && (v === 'avc1' || a === 'mp4a')) {
    return 'WebM 装不下 H.264 / AAC，选它们的话建议改用 MP4 或自动，否则要靠 ffmpeg 重新封装。'
  }
  return ''
})

/** 展示用的 aria2c 落点信息（随包副本还是 PATH 上那份）。 */
const ariaInfo = ref<{ found: boolean; path: string | null; version: string | null } | null>(null)

async function loadAria2c() {
  try {
    ariaInfo.value = await api.aria2cInfo()
  } catch {
    /* 纯展示信息，取不到不影响使用 */
  }
}

function pickFile() {
  fileInput.value?.click()
}

/**
 * 用原生 `<input type="file">` 读文件内容再交给后端校验。
 *
 * 这样不必引入 Tauri 的 dialog 插件；而**校验仍在后端做**——
 * 前端只负责把内容送过去，格式错误的行号提示由 `validate_netscape` 给出。
 */
async function onFileChosen(e: Event) {
  const input = e.target as HTMLInputElement
  const f = input.files?.[0]
  if (!f) return
  importing.value = true
  importError.value = null
  try {
    const text = await f.text()
    const p = await api.importCookieProfile(f.name.replace(/\.txt$/i, ''), text, f.name)
    await loadProfiles()
    s.cookieProfileId = p.id
    s.cookieMode = 'profile'
  } catch (err) {
    // 后端返回的错误已经带了行号和原因，原样展示
    importError.value = String(err)
  } finally {
    importing.value = false
    input.value = ''
  }
}

async function removeProfile(id: string) {
  try {
    await api.deleteCookieProfile(id)
    if (s.cookieProfileId === id) s.cookieProfileId = ''
    delete profileTest.value[id]
    await loadProfiles()
  } catch (e) {
    importError.value = `删除失败：${String(e)}`
  }
}

async function testProfile(id: string) {
  testing.value = true
  try {
    profileTest.value[id] = await api.testCookieProfile(id)
  } finally {
    testing.value = false
  }
}

/**
 * 浏览器预检。**必须真的跑一次**：实测 Chrome 与 Edge 在 Windows 上都失败，
 * 但原因不同（数据库被占用 vs DPAPI 解密失败），指引也不同（DESIGN §6.2）。
 */
async function testBrowser() {
  testing.value = true
  browserTest.value = null
  try {
    browserTest.value = await api.testCookieBrowser(s.cookieBrowser)
  } finally {
    testing.value = false
  }
}

/* ── JS 运行时 ──
 *
 * 实测：同一份 yt-dlp、同一个链接、同样的 cookie 与代理，只差 `--js-runtimes node`
 * 就是「需要重载页面」与「成功」的区别。yt-dlp **不会**自动启用已安装的运行时，
 * 所以这里默认自动检测并传进去。
 */
const jsInfo = ref<JsRuntimeInfo | null>(null)

async function loadJsRuntimes() {
  try {
    jsInfo.value = await api.detectJsRuntimes()
  } catch {
    /* 纯诊断信息，取不到不影响使用 */
  }
}

/* ───────────────────── 代理 ───────────────────── */

const proxyProbe = ref<ProbeResult | null>(null)
const testingProxy = ref(false)

/** 系统代理快照，只在「跟随系统代理」时读取并展示。 */
const sysProxy = ref<{
  enabled: boolean
  server: string
  bypass: string
  autoConfigUrl: string
  resolved: string | null
  usesPac: boolean
} | null>(null)
const sysLoading = ref(false)

async function loadSystemProxy() {
  sysLoading.value = true
  try {
    sysProxy.value = await api.systemProxy()
  } catch (e) {
    importError.value = `读取系统代理失败：${String(e)}`
  } finally {
    sysLoading.value = false
  }
}

/**
 * 切换协议时顺便把端口带到该协议的惯例值。
 *
 * 只在端口**还是上一个协议的默认值**时才改——用户自己填过的端口不能被覆盖。
 */
function setProtocol(p: 'http' | 'socks5') {
  const defaults: Record<string, number[]> = { http: [80, 8080], socks5: [1080] }
  const others = Object.entries(defaults)
    .filter(([k]) => k !== p)
    .flatMap(([, v]) => v)
  const wasDefault = others.includes(Number(s.proxyPort)) || !s.proxyPort
  s.proxyProtocol = p
  if (wasDefault) s.proxyPort = p === 'socks5' ? 1080 : 8080
}

/**
 * 「实际传给 yt-dlp」的预览。
 *
 * ⚠️ **绝不能显示密码**——所以这里自己拼，不复用后端的完整 URL。
 */
const effectiveProxyLabel = computed(() => {
  const host = s.proxyHost.trim()
  if (!host) return '（还没填主机名）'
  const scheme = s.proxyProtocol === 'socks5' ? 'socks5h' : 'http'
  const auth = s.proxyAuth && s.proxyUser.trim() ? `${s.proxyUser.trim()}:***@` : ''
  return `${scheme}://${auth}${host}:${s.proxyPort}`
})

/** 真发一次请求验证——只校验格式的话，代理没启动也会显示「格式正确」。 */
async function testProxy() {
  testingProxy.value = true
  proxyProbe.value = null
  try {
    proxyProbe.value = await api.testProxy()
  } finally {
    testingProxy.value = false
  }
}

/* ───────────────────── yt-dlp 自更新（DESIGN §8）───────────────────── */

const updateInfo = ref<UpdateInfo | null>(null)
const updateMsg = ref<string | null>(null)
const updateOk = ref(false)
const updateBusy = ref(false)

async function checkUpdate() {
  updateBusy.value = true
  updateMsg.value = null
  updateInfo.value = null
  try {
    updateInfo.value = await api.checkYtdlpUpdate()
  } catch (e) {
    updateOk.value = false
    updateMsg.value = String(e)
  } finally {
    updateBusy.value = false
  }
}

/** 更新本身**不依赖 GitHub API**（API 有速率配额），所以即使 check 失败也能更新。 */
async function runUpdate() {
  updateBusy.value = true
  updateMsg.value = null
  try {
    updateMsg.value = await api.applyYtdlpUpdate()
    updateOk.value = true
    updateInfo.value = await api.checkYtdlpUpdate().catch(() => null)
  } catch (e) {
    updateOk.value = false
    updateMsg.value = String(e)
  } finally {
    updateBusy.value = false
  }
}

async function save() {
  await store.saveSettings(deepClone(s))
  saved.value = true
  setTimeout(() => (saved.value = false), 1600)
}

/** 丢弃未保存的改动。设置现在是主区页面，用户可能改了一半就想退出去。 */
function revert() {
  Object.assign(s, deepClone(store.settings!))
  importError.value = null
}

/* ───────────────── 路径字段：对话框 + 手填 两条路都要有 ─────────────────
 *
 * 原生对话框拿不到就手填——**两条路都必须留着**：
 * 路径常常是从别处（下载目录、另一个软件）复制过来的，只能粘贴；
 * 而要找「那个盘上的某个文件夹」时，手打又太痛苦。
 *
 * 对话框返回 `null` 表示用户取消，此时**什么都不做**（不能把字段清空）。
 */

async function browseFolder(key: 'outputDir' | 'tempDir') {
  try {
    const picked = await api.pickFolder(s[key])
    if (picked) s[key] = picked
  } catch (e) {
    importError.value = `打开文件夹对话框失败：${String(e)}`
  }
}

async function browseFile(key: 'archivePath' | 'cookieFile') {
  try {
    const picked = await api.pickFile(s[key])
    if (picked) s[key] = picked
  } catch (e) {
    importError.value = `打开文件对话框失败：${String(e)}`
  }
}

/** 在资源管理器里定位（文件会被选中，目录会被打开）。 */
async function openDir(path: string) {
  const dir = path.trim()
  if (!dir) return
  try {
    await api.openFile(dir)
  } catch (e) {
    importError.value = `打开目录失败：${String(e)}`
  }
}

async function revealPath(path: string) {
  const p = path.trim()
  if (!p) return
  try {
    await api.revealFile(p)
  } catch (e) {
    importError.value = `定位失败：${String(e)}`
  }
}

/**
 * 目录字段的即时提示。
 *
 * 只做**形态**检查，不验存在性——后端没有「这个路径存不存在」的查询，
 * 而且网络盘/移动硬盘暂时不在线也不该拦着用户保存。
 * 这里拦的是最容易犯的错：填了个文件而不是目录、或者填了相对路径。
 */
function dirWarning(key: 'outputDir' | 'tempDir'): string | null {
  const v = s[key].trim()
  if (!v) return '留空会用默认目录'
  if (!/^[a-zA-Z]:[\\/]/.test(v) && !v.startsWith('\\\\') && !v.startsWith('/')) {
    return '看起来不是绝对路径，建议用「浏览…」选一个'
  }
  if (/[\\/][^\\/]*\.[a-zA-Z0-9]{1,5}$/.test(v)) {
    return '这看起来是一个文件而不是目录'
  }
  return null
}

/* ═════════════════ 文件名模板 ═════════════════
 *
 * yt-dlp 的输出模板字段有上百个，界面上只给一个空输入框等于让用户去翻文档。
 * 这里给三样东西：**可用字段清单**（可点击插入）、**常用模板**、**实时预览**。
 *
 * 两条实测确认的坑，必须显式警告：
 *  1. 模板里没有 `%(ext)s` → 下出来的文件**真的没有扩展名**（不是自动补上）。
 *  2. 字段名拼错 → yt-dlp 静默填 `NA`，不会报错。
 */

interface TplField {
  name: string
  desc: string
  /** 预览用的样例值。 */
  sample: string
}

const TEMPLATE_FIELD_GROUPS: { group: string; fields: TplField[] }[] = [
  {
    group: '基本信息',
    fields: [
      { name: 'title', desc: '视频标题', sample: '示例视频标题 Example Title' },
      { name: 'id', desc: '视频 ID', sample: 'dQw4w9WgXcQ' },
      { name: 'ext', desc: '扩展名（必填）', sample: 'mp4' },
      { name: 'duration_string', desc: '时长 12:34', sample: '10:13' },
      { name: 'duration', desc: '时长（秒）', sample: '613' },
      { name: 'resolution', desc: '分辨率', sample: '1920x1080' },
      { name: 'height', desc: '画面高度', sample: '1080' },
      { name: 'width', desc: '画面宽度', sample: '1920' },
      { name: 'fps', desc: '帧率', sample: '30' },
      { name: 'format_id', desc: '格式 ID', sample: '137' },
      { name: 'format_note', desc: '格式说明', sample: '1080p' },
      { name: 'filesize', desc: '文件大小（字节）', sample: '402653184' },
      { name: 'language', desc: '语言', sample: 'zh' },
    ],
  },
  {
    group: '上传者与时间',
    fields: [
      { name: 'uploader', desc: '上传者', sample: '示例频道' },
      { name: 'uploader_id', desc: '上传者 ID', sample: '@example' },
      { name: 'channel', desc: '频道名', sample: '示例频道' },
      { name: 'upload_date', desc: '上传日期 20260929', sample: '20260929' },
      { name: 'release_date', desc: '发布日期', sample: '20260929' },
      { name: 'release_year', desc: '发布年份', sample: '2026' },
      { name: 'timestamp', desc: 'Unix 时间戳', sample: '1790000000' },
    ],
  },
  {
    group: '播放列表',
    fields: [
      { name: 'playlist', desc: '播放列表名', sample: '示例合集' },
      { name: 'playlist_index', desc: '列表内序号', sample: '3' },
      { name: 'playlist_id', desc: '播放列表 ID', sample: 'PLexample' },
      { name: 'n_entries', desc: '总集数', sample: '42' },
    ],
  },
  {
    group: '站点与统计',
    fields: [
      { name: 'extractor', desc: '站点名', sample: 'Youtube' },
      { name: 'webpage_url', desc: '网页地址', sample: 'https://example.com/watch?v=x' },
      { name: 'view_count', desc: '播放量', sample: '1234567' },
      { name: 'like_count', desc: '点赞数', sample: '8901' },
      { name: 'comment_count', desc: '评论数', sample: '234' },
      { name: 'tags', desc: '标签（列表）', sample: 'music' },
    ],
  },
]

/** 平面化的字段表，供校验与预览查值。 */
const TEMPLATE_FIELDS: TplField[] = TEMPLATE_FIELD_GROUPS.flatMap((g) => g.fields)
const TEMPLATE_SAMPLE: Record<string, string> = Object.fromEntries(
  TEMPLATE_FIELDS.map((f) => [f.name, f.sample]),
)

const DEFAULT_FILENAME_TEMPLATE = '%(title).150B [%(id)s].%(ext)s'

const TEMPLATE_PRESETS = [
  { label: '默认', expr: DEFAULT_FILENAME_TEMPLATE },
  { label: '标题 + 日期', expr: '%(upload_date)s %(title).120B.%(ext)s' },
  { label: '上传者 + 标题', expr: '%(uploader)s - %(title).120B.%(ext)s' },
  { label: '列表序号 + 标题', expr: '%(playlist_index)02d - %(title).120B.%(ext)s' },
  { label: '列表名 / 序号', expr: '%(playlist)s/%(playlist_index)02d - %(title).120B.%(ext)s' },
  { label: '只用标题', expr: '%(title).150B.%(ext)s' },
]

/** 修饰符速查——这些不是字段，是写在字段后面的格式说明。 */
const TEMPLATE_MODIFIERS = [
  { code: '%(title).150B', desc: '按字节截断到 150（中文安全，防超长路径）' },
  { code: '%(title).50s', desc: '按字符截断到 50' },
  { code: '%(playlist_index)02d', desc: '数字补零到 2 位' },
  { code: '%(upload_date>%Y-%m-%d)s', desc: '把日期改成 2026-09-29 这种格式' },
]

const tplInput = ref<HTMLInputElement | null>(null)

/** 在光标处插入一个字段。没有焦点时追加到末尾。 */
function insertField(name: string) {
  const token = `%(${name})s`
  const el = tplInput.value
  if (!el) {
    s.filenameTemplate += token
    return
  }
  const start = el.selectionStart ?? s.filenameTemplate.length
  const end = el.selectionEnd ?? start
  s.filenameTemplate =
    s.filenameTemplate.slice(0, start) + token + s.filenameTemplate.slice(end)
  void nextTick(() => {
    el.focus()
    el.setSelectionRange(start + token.length, start + token.length)
  })
}

/** 按 UTF-8 字节截断——和 yt-dlp 的 `.NB` 语义一致（中文一个字 3 字节）。 */
function truncateBytes(str: string, n: number): string {
  const enc = new TextEncoder()
  if (enc.encode(str).length <= n) return str
  let out = ''
  let used = 0
  for (const ch of str) {
    const w = enc.encode(ch).length
    if (used + w > n) break
    out += ch
    used += w
  }
  return out
}

/**
 * 用样例值渲染模板，让用户看见「填进去会长什么样」。
 *
 * 只实现常见写法（`s`/`d`/`B`、`.NB`、`.Ns`、`0Nd`、`>日期格式`）——
 * yt-dlp 的模板语言比这大得多，不认识的一律原样保留，
 * 好过猜错了给出一个**看起来对但其实错**的预览。
 *
 * ⚠️ 格式说明那一段**不能图省事写成 `[^sSdDjJlLq]*`**：那样它会把后面的字面量
 * 一起吞掉，`%(title).150B [%(id)s]` 会被整体当成一个字段，
 * 预览变成 `(4K) …)s]`。必须只允许真正出现在格式说明里的字符（且不含空格）。
 */
function renderTemplate(tpl: string, sample: Record<string, string>): string {
  return tpl.replace(
    /%\(([^)>]+)(?:>([^)]*))?\)([-+#0-9.]*)([sSdDjJlLqB])/g,
    (whole, name: string, dateFmt: string | undefined, spec: string, conv: string) => {
      const raw = sample[name.trim()]
      if (raw === undefined) return whole // 未知字段：原样留着，由警告去说

      let v = raw
      if (dateFmt) {
        const m = /^(\d{4})(\d{2})(\d{2})$/.exec(raw)
        v = m ? dateFmt.replace('%Y', m[1]).replace('%m', m[2]).replace('%d', m[3]) : raw
      }
      // `.150B` = 按字节截断；`.50s` = 按字符截断；`02d` = 补零
      const num = /^\.(\d+)$/.exec(spec)
      if (conv === 'B' && num) v = truncateBytes(v, Number(num[1]))
      if (conv === 's' && num) v = [...v].slice(0, Number(num[1])).join('')
      const pad = /^0(\d+)$/.exec(spec)
      if (conv === 'd' && pad) v = v.padStart(Number(pad[1]), '0')
      return v
    },
  )
}

/**
 * 预览文本。有真实任务时用它的标题，预览才有说服力
 * （长标题截断的效果一眼就能看出来）。
 */
const templatePreview = computed(() => {
  const tpl = s.filenameTemplate.trim()
  if (!tpl) return '（留空会用默认模板）'
  const real = store.tasks.find((t) => t.title)?.title
  const sample = real ? { ...TEMPLATE_SAMPLE, title: real } : TEMPLATE_SAMPLE
  return renderTemplate(tpl, sample)
})

/** 模板里出现的字段名（去重）。 */
const templateFields = computed(() => {
  const names = new Set<string>()
  for (const m of s.filenameTemplate.matchAll(/%\(([^)>]+)/g)) names.add(m[1].trim())
  return [...names]
})

const templateWarning = computed(() => {
  const tpl = s.filenameTemplate.trim()
  if (!tpl) return '留空会用默认模板'
  if (!templateFields.value.includes('ext')) {
    return '模板里没有 %(ext)s —— 实测下出来的文件会真的没有扩展名，建议加上'
  }
  const known = new Set(TEMPLATE_FIELDS.map((f) => f.name))
  const unknown = templateFields.value.filter((n) => !known.has(n))
  if (unknown.length) {
    return `未知字段 ${unknown.map((n) => `%(${n})s`).join('、')} —— yt-dlp 不会报错，会静默填成 NA。是不是拼错了？`
  }
  return null
})
</script>

<template>
  <!--
    设置是**主区的一个页面**，不是右侧抽屉（DESIGN §9.2）。
    因此这里没有遮罩、没有 fixed 定位——宽度由外层 `.main` 决定。
  -->
  <div class="settings">
    <header class="page-head">
      <div class="head-text">
        <h1>设置</h1>
        <p>保存后只影响<strong>之后</strong>的探测与下载，已在跑的任务不受影响。</p>
      </div>
    </header>

    <nav class="tabs">
      <button :class="{ on: tab === 'general' }" @click="tab = 'general'">常规</button>
      <button :class="{ on: tab === 'format' }" @click="tab = 'format'">格式</button>
      <button :class="{ on: tab === 'embed' }" @click="tab = 'embed'">嵌入</button>
      <button :class="{ on: tab === 'download' }" @click="tab = 'download'">下载器</button>
      <button :class="{ on: tab === 'network' }" @click="tab = 'network'">网络与账号</button>
    </nav>

    <div class="body">
      <div class="body-inner">
        <!-- ───── 常规 ───── -->
        <section v-if="tab === 'general'">
          <div class="field">
            <span>输出目录</span>
            <div class="path-row">
              <input v-model="s.outputDir" class="mono" spellcheck="false" />
              <button class="btn sm" title="选择文件夹" @click="browseFolder('outputDir')">
                浏览…
              </button>
              <button
                class="btn sm"
                :disabled="!s.outputDir.trim()"
                title="在资源管理器中打开这个目录"
                @click="openDir(s.outputDir)"
              >
                打开
              </button>
            </div>
            <em v-if="dirWarning('outputDir')" class="path-warn">{{ dirWarning('outputDir') }}</em>
          </div>
          <div class="field">
            <span>临时目录<em>断点续传依赖它稳定不变</em></span>
            <div class="path-row">
              <input v-model="s.tempDir" class="mono" spellcheck="false" />
              <button class="btn sm" title="选择文件夹" @click="browseFolder('tempDir')">
                浏览…
              </button>
              <button
                class="btn sm"
                :disabled="!s.tempDir.trim()"
                title="在资源管理器中打开这个目录"
                @click="openDir(s.tempDir)"
              >
                打开
              </button>
            </div>
            <em v-if="dirWarning('tempDir')" class="path-warn">{{ dirWarning('tempDir') }}</em>
          </div>
          <div class="field">
            <span>文件名模板<em>决定下载下来的文件叫什么；下面有可用字段清单</em></span>
            <div class="path-row">
              <input
                ref="tplInput"
                v-model="s.filenameTemplate"
                class="mono"
                spellcheck="false"
                placeholder="%(title).150B [%(id)s].%(ext)s"
              />
              <button
                class="btn sm"
                :disabled="s.filenameTemplate === DEFAULT_FILENAME_TEMPLATE"
                title="恢复成默认模板"
                @click="s.filenameTemplate = DEFAULT_FILENAME_TEMPLATE"
              >
                恢复默认
              </button>
            </div>

            <!-- 实时预览：模板写对没有，看一眼就知道 -->
            <div class="tpl-preview">
              <span class="tp-label">预览</span>
              <code class="mono">{{ templatePreview }}</code>
            </div>
            <em v-if="templateWarning" class="path-warn">{{ templateWarning }}</em>

            <!-- 常用模板：点一下就能用，省得从零拼 -->
            <div class="tpl-presets">
              <button
                v-for="p in TEMPLATE_PRESETS"
                :key="p.label"
                class="tpl-preset"
                :class="{ on: s.filenameTemplate === p.expr }"
                :title="p.expr"
                @click="s.filenameTemplate = p.expr"
              >
                {{ p.label }}
              </button>
            </div>

            <!-- 可用字段：默认折叠，需要时展开 -->
            <details class="tpl-vars">
              <summary>可用变量（点一下插入到光标处）</summary>
              <div v-for="g in TEMPLATE_FIELD_GROUPS" :key="g.group" class="vg">
                <div class="vg-title">{{ g.group }}</div>
                <div class="vg-grid">
                  <button
                    v-for="f in g.fields"
                    :key="f.name"
                    class="var"
                    :title="`${f.desc}　样例：${f.sample}`"
                    @click="insertField(f.name)"
                  >
                    <code class="mono">{{ f.name }}</code>
                    <span>{{ f.desc }}</span>
                  </button>
                </div>
              </div>
              <div class="vg">
                <div class="vg-title">格式修饰符（写在字段后面）</div>
                <table class="mods">
                  <tr v-for="m in TEMPLATE_MODIFIERS" :key="m.code">
                    <td><code class="mono">{{ m.code }}</code></td>
                    <td>{{ m.desc }}</td>
                  </tr>
                </table>
              </div>
              <p class="note">
                字段名拼错时 yt-dlp <strong>不会报错</strong>，会静默填成 <code>NA</code>。
                完整清单见 yt-dlp 文档的「Output Template」一节。
              </p>
            </details>
          </div>
          <label class="field">
            <span>限速<em>留空为不限速，如 5M</em></span>
            <input v-model="s.limitRate" class="mono" placeholder="不限速" />
          </label>

          <h3>并发</h3>
          <p class="note">探测请求轻、可高；下载重、易触发限流，两者必须分开。</p>
          <label class="field inline">
            <span>探测并发</span>
            <input v-model.number="s.probeConcurrency" type="number" min="1" max="16" />
          </label>
          <label class="field inline">
            <span>下载并发</span>
            <input v-model.number="s.downloadConcurrency" type="number" min="1" max="8" />
          </label>
          <label class="field inline">
            <span>同一站点并发<em>比全局降并发更有效</em></span>
            <input v-model.number="s.perHostConcurrency" type="number" min="1" max="4" />
          </label>
        </section>

        <!-- ───── 格式 ───── -->
        <section v-else-if="tab === 'format'">
          <label class="field">
            <span>默认画质</span>
            <select v-model="s.preset">
              <option value="best">最佳画质</option>
              <option value="maxHeight">限制分辨率</option>
              <option value="audioOnly">仅音频</option>
            </select>
          </label>
          <label v-if="s.preset === 'maxHeight'" class="field inline">
            <span>分辨率上限</span>
            <select v-model.number="s.maxHeight">
              <option :value="2160">2160p (4K)</option>
              <option :value="1440">1440p</option>
              <option :value="1080">1080p</option>
              <option :value="720">720p</option>
              <option :value="480">480p</option>
            </select>
          </label>
          <label v-if="s.preset === 'audioOnly'" class="field inline">
            <span>音频格式</span>
            <select v-model="s.audioFormat">
              <option value="mp3">MP3</option>
              <option value="m4a">M4A</option>
              <option value="opus">Opus</option>
              <option value="flac">FLAC</option>
              <option value="wav">WAV</option>
            </select>
          </label>

          <label class="field">
            <span>输出容器</span>
            <select v-model="s.container">
              <option value="auto">自动（启用嵌入时用 MKV）</option>
              <option value="mp4">MP4</option>
              <option value="mkv">MKV</option>
              <option value="webm">WebM</option>
            </select>
          </label>
          <p class="note warn">
            启用缩略图嵌入时 WebM 会<strong>直接失败</strong>；MP4 的缩略图依赖三级回落且字幕只能用
            mov_text。MKV 是唯一全部可用的容器，因此自动模式会切到 MKV。
          </p>

          <h3>优先选择编码</h3>
          <p class="note">
            只是一种<strong>偏好</strong>：站点没有首选编码时会自动退到次选，
            不会因此下不到。留空表示不干预 yt-dlp 的默认挑法。
          </p>
          <div class="codec-grid">
            <label class="field inline">
              <span>视频</span>
              <select v-model="s.preferVcodec">
                <option v-for="c in codecChoices.video" :key="c.value" :value="c.value">
                  {{ c.label }}
                </option>
              </select>
            </label>
            <label class="field inline">
              <span>音频</span>
              <select v-model="s.preferAcodec">
                <option v-for="c in codecChoices.audio" :key="c.value" :value="c.value">
                  {{ c.label }}
                </option>
              </select>
            </label>
          </div>
          <p class="note">
            表达式：<code class="mono">{{ codecPreview }}</code>
          </p>
          <p v-if="codecCompatHint" class="note warn">
            {{ codecCompatHint }}
          </p>
        </section>

        <!-- ───── 嵌入 ───── -->
        <section v-else-if="tab === 'embed'">
          <label class="check">
            <input v-model="s.embed.subs" type="checkbox" />
            <span>嵌入字幕</span>
          </label>
          <template v-if="s.embed.subs">
            <label class="field">
              <span>字幕语言<em>逗号分隔，支持 all 与 - 排除，如 all,-live_chat</em></span>
              <input v-model="s.embed.subLangs" class="mono" />
            </label>
            <label class="check sub">
              <input v-model="s.embed.autoSubs" type="checkbox" />
              <span>包含自动生成字幕<em>需要 --write-auto-subs</em></span>
            </label>
            <label class="check sub">
              <input v-model="s.embed.keepSubFiles" type="checkbox" />
              <span>同时保留 .vtt 文件<em>不勾选则嵌入后删除</em></span>
            </label>
          </template>

          <label class="check">
            <input v-model="s.embed.thumbnail" type="checkbox" />
            <span>嵌入缩略图</span>
          </label>
          <label v-if="s.embed.thumbnail" class="check sub">
            <input v-model="s.embed.keepThumbnailFile" type="checkbox" />
            <span>同时保留缩略图文件</span>
          </label>

          <label class="check">
            <input v-model="s.embed.metadata" type="checkbox" />
            <span>嵌入元数据</span>
          </label>
          <template v-if="s.embed.metadata">
            <p class="note">
              --embed-metadata 默认还会嵌入章节与 infojson，这里已显式关闭，可按需开启。
            </p>
            <label class="check sub">
              <input v-model="s.embed.chapters" type="checkbox" />
              <span>同时嵌入章节</span>
            </label>
            <label class="check sub">
              <input v-model="s.embed.infoJson" type="checkbox" />
              <span>同时附加 info.json<em>仅 MKV/MKA 支持</em></span>
            </label>
          </template>

          <p class="note warn">
            注意：字幕嵌入失败时 yt-dlp 只打印警告、仍返回成功。若字幕没进去，请查看任务详情里的「后处理告警」。
          </p>
        </section>

        <!-- ───── 下载器 ───── -->
        <section v-else-if="tab === 'download'">
          <label class="check">
            <input v-model="s.aria2c" type="checkbox" />
            <span>使用 aria2c 多线程下载<em>高级选项，默认关闭</em></span>
          </label>
          <p class="note warn">
            aria2c 会让 yt-dlp 完全不上报进度（实测 0 条），进度改为解析 aria2c 自身的输出；
            且它对不支持 Range 的服务器会直接失败。失败时会自动回落原生下载。
          </p>

          <h3>下载归档</h3>
          <label class="check">
            <input v-model="s.archiveEnabled" type="checkbox" />
            <span>启用 download-archive<em>已下载过的视频将被跳过</em></span>
          </label>
          <div v-if="s.archiveEnabled" class="field">
            <span>归档文件路径<em>不存在会自动创建</em></span>
            <div class="path-row">
              <input v-model="s.archivePath" class="mono" spellcheck="false" />
              <button class="btn sm" title="选择已存在的归档文件" @click="browseFile('archivePath')">
                浏览…
              </button>
              <button
                class="btn sm"
                :disabled="!s.archivePath.trim()"
                title="在资源管理器中定位该文件"
                @click="revealPath(s.archivePath)"
              >
                定位
              </button>
            </div>
          </div>
          <p class="note">
            跳过的任务会显示为「已跳过」而非「已完成」——两者在 yt-dlp 里都返回退出码 0。
          </p>

          <h3>JS 运行时</h3>
          <p class="note warn">
            YouTube 的 n-sig 挑战需要 JS 运行时。yt-dlp<strong>不会自动启用</strong>已安装的
            ——不给的话会返回「需要重载页面」或只给预览图。留空即自动检测。
          </p>
          <div class="js-list">
            <span
              v-for="c in jsInfo?.candidates ?? []"
              :key="c.name"
              class="js-chip"
              :class="{ on: !!c.path }"
              :title="c.path ?? '未安装'"
            >
              {{ c.name }}{{ c.path ? ' ✓' : '' }}
            </span>
          </div>
          <label class="field">
            <span>指定运行时<em>逗号分隔；留空 = 自动检测</em></span>
            <input
              v-model="s.jsRuntime"
              class="mono"
              :placeholder="(jsInfo?.detected ?? []).join(',') || '（未检测到）'"
            />
          </label>

          <h3>aria2c（多线程下载器）</h3>
          <p class="note">
            随包分发的第三方程序，查找顺序与 yt-dlp 一致：
            <strong>随包副本 → AppData → PATH</strong>。
            它让一个文件分多段并行下载，速度通常明显更快。
          </p>
          <p v-if="ariaInfo?.found" class="sub-line">
            {{ ariaInfo.version }}<br />
            <code class="mono">{{ ellipsizePath(ariaInfo.path ?? '', 76) }}</code>
          </p>
          <p v-else class="path-warn">
            没有找到可用的 aria2c——勾了「多线程下载」也会自动回落到内置下载器，
            并在任务详情里留一条提示。
          </p>

          <h3>yt-dlp 版本</h3>
          <p class="note">
            换掉一个 <code>yt-dlp.exe</code> 就能修好全部站点问题，
            <strong>不需要重新发版应用</strong>。升级副本写在 AppData，
            不需要管理员权限，也不会被安装器的「修复」还原。
          </p>
          <div class="row-inline test-row">
            <button class="btn sm" :disabled="updateBusy" @click="checkUpdate">
              {{ updateBusy ? '处理中…' : '检查更新' }}
            </button>
            <button class="btn sm primary" :disabled="updateBusy" @click="runUpdate">
              立即更新
            </button>
            <span v-if="updateInfo" class="ver mono">
              {{ updateInfo.current || '未知' }} → {{ updateInfo.latest }}
            </span>
          </div>
          <p v-if="updateInfo && updateInfo.newer" class="note">
            有新版本可用（{{ updateInfo.current }} → {{ updateInfo.latest }}）。
          </p>
          <p v-else-if="updateInfo" class="note">已是最新版本（{{ updateInfo.current }}）。</p>
          <p v-if="updateMsg" class="res" :class="updateOk ? 'ok' : 'bad'">{{ updateMsg }}</p>
        </section>

        <!-- ───── 网络与账号 ───── -->
        <section v-else>
          <h3>代理</h3>

          <!--
            结构照着 Windows「设置 → 网络和 Internet → 代理」那一页来：
            三选一（不用 / 跟随系统 / 手动），手动的展开协议、主机、端口、绕过、认证。
          -->
          <div class="radios">
            <label class="radio">
              <input v-model="s.proxyMode" type="radio" value="none" />
              <span>不使用代理</span>
            </label>

            <label class="radio">
              <input v-model="s.proxyMode" type="radio" value="system" />
              <span>跟随系统代理</span>
              <button class="btn sm" type="button" @click="loadSystemProxy">
                {{ sysLoading ? '读取中…' : '读取系统设置' }}
              </button>
            </label>
            <div v-if="s.proxyMode === 'system'" class="sub-block">
              <p v-if="sysProxy && sysProxy.resolved" class="sub-line">
                系统代理 <code class="mono">{{ sysProxy.resolved }}</code>
                <template v-if="sysProxy.bypass">
                  <br />绕过 <code class="mono">{{ ellipsizePath(sysProxy.bypass, 76) }}</code>
                </template>
              </p>
              <p v-else class="sub-line warn-text">
                系统没有启用代理（注册表里 <code>ProxyEnable</code> 为 0）。
              </p>
              <!-- yt-dlp 不支持 PAC，必须如实说，否则用户会以为配了就能用 -->
              <p v-if="sysProxy?.usesPac" class="path-warn">
                检测到系统使用 PAC 自动配置（{{ sysProxy.autoConfigUrl }}）。
                yt-dlp 不支持 PAC，无法自动解析——请改用手动配置填写 PAC 解析出的地址。
              </p>
              <p v-else class="sub-line muted-text">
                每次探测/下载时重新读取，改了系统代理不用回来点一次。
              </p>
            </div>

            <label class="radio">
              <input v-model="s.proxyMode" type="radio" value="manual" />
              <span>手动配置</span>
            </label>
            <div v-if="s.proxyMode === 'manual'" class="sub-block">
              <div class="seg narrow">
                <button :class="{ on: s.proxyProtocol === 'http' }" @click="setProtocol('http')">
                  HTTP
                </button>
                <button
                  :class="{ on: s.proxyProtocol === 'socks5' }"
                  @click="setProtocol('socks5')"
                >
                  SOCKS5
                </button>
              </div>

              <div class="two-col">
                <label class="field">
                  <span>主机名</span>
                  <input v-model="s.proxyHost" class="mono" spellcheck="false" placeholder="127.0.0.1" />
                </label>
                <label class="field">
                  <span>端口</span>
                  <input v-model.number="s.proxyPort" type="number" min="1" max="65535" />
                </label>
              </div>

              <label class="check">
                <input v-model="s.proxyAuth" type="checkbox" />
                <span>代理身份验证<em>用户名密码会拼进代理地址</em></span>
              </label>
              <div v-if="s.proxyAuth" class="sub-block tight">
                <label class="field inline">
                  <span>用户名</span>
                  <input v-model="s.proxyUser" class="mono" spellcheck="false" />
                </label>
                <label class="field inline">
                  <span>密码</span>
                  <input v-model="s.proxyPassword" type="password" class="mono" spellcheck="false" />
                </label>
                <label class="check sub">
                  <input v-model="s.proxyRemember" type="checkbox" />
                  <span>记住密码<em>不勾则只保留在内存，重启后需要重填</em></span>
                </label>
              </div>

              <p class="sub-line">
                实际传给 yt-dlp：<code class="mono">{{ effectiveProxyLabel }}</code>
              </p>
            </div>
          </div>

          <div class="row-inline">
            <button
              class="btn sm"
              :disabled="s.proxyMode === 'none' || testingProxy"
              @click="testProxy"
            >
              {{ testingProxy ? '测试中…' : '检查连接' }}
            </button>
            <span class="ver">会真的发一次请求验证，不是只校验格式</span>
          </div>
          <p v-if="proxyProbe" class="res" :class="proxyProbe.ok ? 'ok' : 'bad'">
            {{ proxyProbe.summary }}
          </p>

          <h3>Cookie</h3>
          <div class="seg">
            <button :class="{ on: s.cookieMode === 'none' }" @click="s.cookieMode = 'none'">不用</button>
            <button :class="{ on: s.cookieMode === 'profile' }" @click="s.cookieMode = 'profile'">
              已导入 ({{ profiles.length }})
            </button>
            <button :class="{ on: s.cookieMode === 'file' }" @click="s.cookieMode = 'file'">
              指定文件
            </button>
            <button :class="{ on: s.cookieMode === 'browser' }" @click="s.cookieMode = 'browser'">
              从浏览器
            </button>
          </div>

          <!-- 已导入的 profile：多账号场景（B站账号 A / YouTube 账号 B） -->
          <template v-if="s.cookieMode === 'profile'">
            <p v-if="profiles.length === 0" class="note">
              还没有导入任何 cookies.txt。点下面的按钮选一个文件——格式会在导入时校验，
              有问题会直接告诉你第几行不对。
            </p>
            <div v-else class="pl-list">
              <label
                v-for="p in profiles"
                :key="p.id"
                class="pl-row"
                :class="{ on: s.cookieProfileId === p.id }"
              >
                <input v-model="s.cookieProfileId" type="radio" :value="p.id" />
                <span class="pl-name">{{ p.name }}</span>
                <span class="pl-meta">{{ p.cookieCount }} 条 · {{ p.origin }}</span>
                <button class="btn ghost sm" :disabled="testing" @click.prevent="testProfile(p.id)">
                  测试
                </button>
                <button class="btn ghost sm danger" @click.prevent="removeProfile(p.id)">
                  删除
                </button>
              </label>
            </div>

            <div class="row-inline test-row">
              <button class="btn sm" :disabled="importing" @click="pickFile">
                {{ importing ? '导入中…' : '导入 cookies.txt' }}
              </button>
            </div>

            <p
              v-for="(r, id) in profileTest"
              :key="id"
              class="res"
              :class="r.ok ? 'ok' : 'bad'"
            >
              {{ r.summary }}
            </p>
          </template>

          <template v-else-if="s.cookieMode === 'file'">
            <div class="field">
              <span>cookies.txt 路径<em>Netscape 格式</em></span>
              <div class="path-row">
                <input v-model="s.cookieFile" class="mono" spellcheck="false" />
                <button class="btn sm" title="选择 cookies.txt" @click="browseFile('cookieFile')">
                  浏览…
                </button>
                <button
                  class="btn sm"
                  :disabled="!s.cookieFile.trim()"
                  title="在资源管理器中定位该文件"
                  @click="revealPath(s.cookieFile)"
                >
                  定位
                </button>
              </div>
            </div>
          </template>

          <template v-else-if="s.cookieMode === 'browser'">
            <label class="field inline">
              <span>浏览器<em>装了的排在前面</em></span>
              <select :value="selBrowser" @change="onBrowserChange">
                <option
                  v-for="b in browsers"
                  :key="b.name"
                  :value="b.name"
                  :disabled="!b.installed"
                >
                  {{ b.label }}{{ b.installed ? '' : '（未安装）' }}
                </option>
              </select>
            </label>

            <!-- profile 选择：同一浏览器常有多套登录态 -->
            <label
              v-if="curBrowser?.supportsProfiles && (curBrowser?.profiles.length ?? 0) > 0"
              class="field inline"
            >
              <span>配置文件<em>按最近使用排序</em></span>
              <select v-model="selProfile" @change="syncToSpec">
                <option value="">（让 yt-dlp 自动挑）</option>
                <option v-for="p in curBrowser!.profiles" :key="p.id" :value="p.id">
                  {{ p.label }}
                  {{ p.id === curBrowser!.recommendedProfile ? ' · 最近用过' : '' }}
                </option>
              </select>
            </label>

            <p v-if="selProfileInfo" class="note">{{ selProfileInfo }}</p>

            <p v-if="curBrowser && !curBrowser.installed" class="note warn">
              本机没有检测到 {{ curBrowser.label }} 的 cookie 库。
            </p>
            <p
              v-else-if="curBrowser?.supportsProfiles && curBrowser.profiles.length === 0"
              class="note"
            >
              没找到 {{ curBrowser.label }} 的 cookie 库——可能还没用它登录过任何网站，
              或者装在了非默认位置。
            </p>

            <!--
              只在选中 Chromium 系时才提示。一直挂着那段 Windows 警告会把
              真正有用的信息淹没——用 Firefox 的人根本不需要看它。
              「是不是 Chromium 系」以后端下发的 chromium 字段为准，界面不猜名字。
            -->
            <p v-if="curBrowser?.chromium" class="note warn">
              Windows 上 Chromium 系（Chrome / Edge 等）把 cookie 库加密了，
              yt-dlp 目前解不开——<strong>关掉浏览器也没用</strong>。
              先点「测试能否读取」确认一下，读不到就换 Firefox。
            </p>

            <div class="row-inline test-row">
              <button class="btn sm" :disabled="testing" @click="testBrowser">
                {{ testing ? '测试中…' : '测试能否读取' }}
              </button>
            </div>
            <p v-if="browserTest" class="res" :class="browserTest.ok ? 'ok' : 'bad'">
              {{ browserTest.summary }}
            </p>
          </template>

          <p v-if="importError" class="res bad">{{ importError }}</p>

          <input
            ref="fileInput"
            class="hidden-file"
            type="file"
            accept=".txt,text/plain"
            @change="onFileChosen"
          />
        </section>
      </div>
    </div>

    <footer>
      <span v-if="saved" class="saved">已保存</span>
      <button class="btn" @click="emit('close')">返回任务</button>
      <button class="btn" @click="revert">还原改动</button>
      <button class="btn primary" @click="save">保存设置</button>
    </footer>
  </div>
</template>

<style scoped>
/* 主区页面布局：页头 / 标签栏 / 可滚动的表单区 / 底部操作条 */
.settings {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--bg);
}

.page-head {
  padding: 16px 32px 0;
}
.head-text h1 {
  margin: 0;
  font-size: var(--fs-xl);
  font-weight: 650;
  letter-spacing: -0.01em;
}
.head-text p {
  margin: 5px 0 0;
  font-size: var(--fs-xs);
  color: var(--text-mute);
}
.head-text p strong {
  color: var(--text-dim);
  font-weight: 600;
}

.tabs {
  display: flex;
  gap: 2px;
  margin: 14px 32px 0;
  padding: 3px;
  background: var(--surface-2);
  border-radius: var(--radius-sm);
  /* 设置项本身很窄，标签栏跟着缩到同一列宽，不然会显得头重脚轻 */
  max-width: 720px;
}
.tabs button {
  flex: 1;
  padding: 7px 10px;
  border-radius: var(--radius-xs);
  color: var(--text-mute);
  font-size: var(--fs-xs);
  font-weight: 500;
  transition: background 0.13s, color 0.13s, box-shadow 0.13s;
}
.tabs button:hover {
  color: var(--text-dim);
}
.tabs button.on {
  background: var(--surface);
  color: var(--text);
  font-weight: 600;
  box-shadow: var(--shadow-xs);
}

.body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 18px 32px 24px;
}
/* 表单列宽固定：设置项铺满 1200px 会让人读一行要横跨半个屏幕 */
.body-inner {
  max-width: 720px;
}
section {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
h3 {
  margin: 14px 0 0;
  font-size: var(--fs-xs);
  color: var(--text-mute);
  text-transform: uppercase;
  letter-spacing: 0.06em;
  font-weight: 650;
}
h3:first-child {
  margin-top: 0;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 5px;
}
.field.inline {
  flex-direction: row;
  align-items: center;
  justify-content: space-between;
}
.field > span {
  font-size: var(--fs-xs);
  color: var(--text-dim);
  display: flex;
  flex-direction: column;
}
.field em {
  font-style: normal;
  font-size: var(--fs-2xs);
  color: var(--text-mute);
}
.field input,
.field select {
  padding: 7px 11px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--surface);
  outline: none;
  min-width: 0;
  transition: border-color 0.13s, box-shadow 0.13s;
}
.field.inline input,
.field.inline select {
  width: 170px;
}
.field input:focus,
.field select:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-dim);
}

/* ── 代理：三选一 + 展开的子块（照着 Windows 那一页的结构） ── */
.radios {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.radio {
  display: flex;
  align-items: center;
  gap: 9px;
  padding: 7px 10px;
  border-radius: var(--radius-sm);
  cursor: pointer;
  font-size: var(--fs-xs);
  transition: background 0.13s;
}
.radio:hover {
  background: var(--surface-2);
}
.radio input {
  accent-color: var(--accent);
  flex-shrink: 0;
  /* 选中态靠 --accent 的点，不需要再放大 */
  width: 14px;
  height: 14px;
}
.radio > span {
  flex: 1;
}
/* 子块缩进，视觉上从属于上面那个单选项 */
.sub-block {
  margin: 2px 0 6px 24px;
  padding-left: 12px;
  border-left: 2px solid var(--border);
  display: flex;
  flex-direction: column;
  gap: 9px;
}
.sub-block.tight {
  gap: 6px;
}
.two-col {
  display: grid;
  grid-template-columns: 1fr 120px;
  gap: 8px;
}
/* 「优先选择编码」的两个下拉：等宽并排 */
.codec-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}
.sub-line {
  margin: 0;
  font-size: var(--fs-2xs);
  color: var(--text-mute);
  line-height: 1.6;
}
.sub-line code {
  background: var(--surface-3);
  padding: 1px 5px;
  border-radius: 4px;
  color: var(--text-dim);
}
.muted-text {
  color: var(--text-mute);
}
.warn-text {
  color: var(--warn);
}
.seg.narrow {
  max-width: 220px;
}
textarea.mono {
  font-family: 'Cascadia Mono', Consolas, monospace;
  font-size: var(--fs-2xs);
  padding: 7px 11px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--surface);
  outline: none;
  resize: vertical;
  min-height: 46px;
  transition: border-color 0.13s, box-shadow 0.13s;
}
textarea.mono:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-dim);
}

/* 路径字段：输入框 + 对话框按钮同一行；输入框吃掉剩余宽度 */
.path-row {
  display: flex;
  gap: 7px;
  align-items: center;
}
.path-row input {
  flex: 1;
  min-width: 0;
  padding: 7px 11px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--surface);
  outline: none;
  transition: border-color 0.13s, box-shadow 0.13s;
}
.path-row input:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-dim);
}
.path-row .btn {
  flex-shrink: 0;
}
.path-warn {
  font-style: normal;
  font-size: var(--fs-2xs);
  color: var(--warn);
}

/* ── 文件名模板 ── */
.tpl-preview {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 7px 11px;
  border-radius: var(--radius-sm);
  background: var(--surface-2);
  font-size: var(--fs-2xs);
  min-width: 0;
}
.tp-label {
  flex-shrink: 0;
  color: var(--text-mute);
}
.tpl-preview code {
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
}

.tpl-presets {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.tpl-preset {
  padding: 4px 10px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text-dim);
  font-size: var(--fs-2xs);
  transition: background 0.13s, border-color 0.13s, color 0.13s;
}
.tpl-preset:hover {
  background: var(--surface-2);
  color: var(--text);
}
.tpl-preset.on {
  border-color: var(--accent);
  background: var(--accent-dim);
  color: var(--accent-ink);
  font-weight: 600;
}

/* 变量清单：折叠起来，需要时再展开，不占版面 */
.tpl-vars {
  border: 1px solid var(--border-soft);
  border-radius: var(--radius-sm);
  background: var(--surface);
}
.tpl-vars summary {
  padding: 8px 12px;
  cursor: pointer;
  font-size: var(--fs-2xs);
  color: var(--text-dim);
  user-select: none;
}
.tpl-vars summary:hover {
  color: var(--text);
}
.tpl-vars[open] summary {
  border-bottom: 1px solid var(--border-soft);
  color: var(--text);
}
.vg {
  padding: 10px 12px 0;
}
.vg:last-of-type {
  padding-bottom: 4px;
}
.vg-title {
  font-size: var(--fs-2xs);
  font-weight: 650;
  color: var(--text-mute);
  margin-bottom: 6px;
  letter-spacing: 0.04em;
}
.vg-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 4px;
}
.var {
  display: flex;
  align-items: baseline;
  gap: 7px;
  padding: 4px 8px;
  border-radius: var(--radius-xs);
  border: 1px solid transparent;
  background: var(--surface-2);
  text-align: left;
  min-width: 0;
  transition: border-color 0.13s, background 0.13s;
}
.var:hover {
  border-color: var(--accent);
  background: var(--accent-dim);
}
.var code {
  flex-shrink: 0;
  font-size: var(--fs-2xs);
  color: var(--accent-ink);
}
.var span {
  font-size: var(--fs-2xs);
  color: var(--text-mute);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.mods {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--fs-2xs);
}
.mods td {
  padding: 3px 8px 3px 0;
  color: var(--text-mute);
  vertical-align: top;
}
.mods td:first-child {
  white-space: nowrap;
  width: 1%;
}
.mods code {
  color: var(--accent-ink);
}
.tpl-vars .note {
  margin: 10px 12px 12px;
  background: transparent;
  padding: 0;
  border-left: none;
}

.check {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 9px 12px;
  border-radius: var(--radius-sm);
  background: var(--surface);
  border: 1px solid var(--border);
  cursor: pointer;
  transition: border-color 0.13s, background 0.13s;
}
.check:hover {
  background: var(--surface-2);
}
.check.sub {
  margin-left: 18px;
  background: transparent;
  border-color: var(--border-soft);
}
.check input {
  margin-top: 2px;
  accent-color: var(--accent);
  flex-shrink: 0;
}
.check span {
  display: flex;
  flex-direction: column;
  font-size: var(--fs-xs);
}
.check em {
  font-style: normal;
  font-size: var(--fs-2xs);
  color: var(--text-mute);
  line-height: 1.45;
}

.note {
  margin: 0;
  font-size: var(--fs-2xs);
  color: var(--text-mute);
  line-height: 1.55;
  padding: 9px 12px;
  background: var(--surface-2);
  border-radius: var(--radius-sm);
  border-left: 2px solid var(--border);
}
.note.warn {
  border-left-color: var(--warn);
  color: var(--text-dim);
}
/* 模板里写的是 <strong>，不是 Markdown 的 **——后者会原样显示出来 */
.note strong {
  color: var(--text);
  font-weight: 600;
}

.row-inline {
  display: flex;
  gap: 7px;
  align-items: center;
}
.flex {
  flex: 1;
}
input.mono {
  font-family: 'Cascadia Mono', Consolas, monospace;
  font-size: var(--fs-xs);
}

.seg {
  display: flex;
  gap: 2px;
  background: var(--surface-2);
  padding: 3px;
  border-radius: var(--radius-sm);
}
.seg button {
  flex: 1;
  padding: 6px;
  border-radius: var(--radius-xs);
  font-size: var(--fs-xs);
  color: var(--text-mute);
  transition: background 0.13s, color 0.13s, box-shadow 0.13s;
}
.seg button.on {
  background: var(--surface);
  color: var(--text);
  font-weight: 600;
  box-shadow: var(--shadow-xs);
}

.res {
  margin: 0;
  font-size: var(--fs-xs);
  padding: 7px 11px;
  border-radius: var(--radius-sm);
}
.res.ok {
  background: rgba(15, 138, 69, 0.07);
  color: var(--ok);
}
.res.bad {
  background: rgba(217, 45, 32, 0.06);
  color: var(--err);
}
.test-row {
  margin-top: 4px;
}

/* cookie profile 列表 */
.pl-list {
  display: flex;
  flex-direction: column;
  gap: 5px;
}
.pl-row {
  display: flex;
  align-items: center;
  gap: 9px;
  padding: 8px 11px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--surface);
  cursor: pointer;
  font-size: var(--fs-xs);
  transition: border-color 0.13s, background 0.13s;
}
.pl-row:hover {
  border-color: #d7dce3;
  background: var(--surface-2);
}
.pl-row.on {
  border-color: var(--accent);
  background: var(--accent-dim);
}
.pl-row input {
  accent-color: var(--accent);
  flex-shrink: 0;
}
.pl-name {
  font-weight: 600;
  flex-shrink: 0;
}
.pl-meta {
  flex: 1;
  min-width: 0;
  color: var(--text-mute);
  font-size: var(--fs-2xs);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
/* 文件选择用原生 input，视觉上隐藏 */
.hidden-file {
  display: none;
}
.ver {
  font-size: var(--fs-2xs);
  color: var(--text-mute);
}
/* JS 运行时检测结果 */
.js-list {
  display: flex;
  flex-wrap: wrap;
  gap: 5px;
}
.js-chip {
  padding: 3px 10px;
  border-radius: 999px;
  font-size: var(--fs-2xs);
  background: var(--surface-2);
  border: 1px solid transparent;
  color: var(--text-mute);
}
.js-chip.on {
  background: rgba(15, 138, 69, 0.08);
  border-color: rgba(15, 138, 69, 0.24);
  color: var(--ok);
  font-weight: 600;
}
.note code {
  background: var(--surface-3);
  padding: 1px 5px;
  border-radius: 4px;
  color: var(--text-dim);
}

footer {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  padding: 14px 32px;
  border-top: 1px solid var(--border-soft);
  background: var(--bg-elev);
}
.saved {
  margin-right: auto;
  font-size: var(--fs-xs);
  color: var(--ok);
  font-weight: 600;
}
</style>
