/** 展示层格式化工具。 */

/**
 * 深拷贝可序列化数据。
 *
 * **不能用 `structuredClone`**：Pinia 的 state 是 Vue 的 `reactive` Proxy，
 * 而 `structuredClone` 对 Proxy 会抛 `DataCloneError`。
 * `Settings` 全是纯 JSON 数据，走 JSON 往返即可。
 */
export function deepClone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v)) as T
}

export function fmtBytes(n: number | null | undefined): string {
  if (n === null || n === undefined) return '—'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let v = n
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v < 10 && i > 0 ? v.toFixed(1) : Math.round(v)} ${units[i]}`
}

export function fmtSpeed(n: number | null | undefined): string {
  if (!n) return '—'
  return `${fmtBytes(n)}/s`
}

export function fmtDuration(sec: number | null | undefined): string {
  if (sec === null || sec === undefined) return '—'
  const s = Math.max(0, Math.round(sec))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const ss = s % 60
  const pad = (x: number) => String(x).padStart(2, '0')
  return h > 0 ? `${h}:${pad(m)}:${pad(ss)}` : `${m}:${pad(ss)}`
}

export function fmtEta(sec: number | null | undefined): string {
  if (sec === null || sec === undefined) return '—'
  if (sec <= 0) return '即将完成'
  return `剩余 ${fmtDuration(sec)}`
}

export function fmtTime(ts: number | null): string {
  if (!ts) return '—'
  const d = new Date(ts)
  const pad = (x: number) => String(x).padStart(2, '0')
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}

/** 把路径截断成 `…\父目录\文件名`，避免撑破行宽。 */
export function shortenPath(p: string, keep = 2): string {
  const parts = p.split(/[\\/]/).filter(Boolean)
  if (parts.length <= keep + 1) return p
  return `…\\${parts.slice(-keep).join('\\')}`
}

/**
 * 单个字符占几个「显示格」。
 *
 * 中日韩字符在等宽字体里占两格。按**字符数**截断会让一行中文比一行英文宽一倍，
 * 截出来长短不一；按显示宽度算才稳定。
 */
function charWidth(ch: string): number {
  const c = ch.codePointAt(0) ?? 0
  const wide =
    (c >= 0x1100 && c <= 0x115f) || // 韩文字母
    (c >= 0x2e80 && c <= 0x303e) || // 部首、标点
    (c >= 0x3041 && c <= 0x33ff) || // 假名、注音、CJK 符号
    (c >= 0x3400 && c <= 0x4dbf) || // CJK 扩展 A
    (c >= 0x4e00 && c <= 0x9fff) || // CJK 基本区
    (c >= 0xa000 && c <= 0xa4cf) ||
    (c >= 0xac00 && c <= 0xd7a3) || // 谚文音节
    (c >= 0xf900 && c <= 0xfaff) ||
    (c >= 0xfe30 && c <= 0xfe6f) ||
    (c >= 0xff00 && c <= 0xff60) || // 全角
    (c >= 0xffe0 && c <= 0xffe6) ||
    (c >= 0x1f300 && c <= 0x1faff) // emoji
  return wide ? 2 : 1
}

function displayWidth(s: string): number {
  let w = 0
  for (const ch of s) w += charWidth(ch)
  return w
}

/**
 * 中间截断，保留头尾：`(4K) 🖤 검스…[tW34TyACBIQ].mp4`。
 *
 * **为什么不用 CSS 的 `text-overflow: ellipsis`**：它砍的是结尾，而下载下来的
 * 文件名恰恰是「结尾」（扩展名、`[视频 id]`）最能说明这是什么文件。
 * 全砍掉之后 `E:\下载\视频\(4K) 🖤 검스 VS 살스…` 既看不出格式也看不出是哪一集。
 */
export function ellipsizeMiddle(s: string, maxWidth = 84): string {
  if (displayWidth(s) <= maxWidth) return s

  const budget = maxWidth - 1 // 省略号自己占一格
  const headBudget = Math.floor(budget / 2)
  const tailBudget = budget - headBudget

  let head = ''
  let used = 0
  for (const ch of s) {
    const w = charWidth(ch)
    if (used + w > headBudget) break
    head += ch
    used += w
  }

  const chars = [...s]
  let tail = ''
  used = 0
  for (let i = chars.length - 1; i >= 0; i--) {
    const w = charWidth(chars[i])
    if (used + w > tailBudget) break
    tail = chars[i] + tail
    used += w
  }

  return `${head}…${tail}`
}

/**
 * 路径的展示形式：目录只留尾部，文件名做中间截断。
 *
 * 比 `shortenPath` 更适合**窄栏位**——`shortenPath` 只动目录，
 * 文件名过长时还是会被 CSS 从右边砍掉，扩展名就看不见了。
 */
export function ellipsizePath(p: string, maxWidth = 84): string {
  // 放得下就原样显示，别无故把盘符换成省略号
  if (displayWidth(p) <= maxWidth) return p

  const parts = p.split(/[\\/]/).filter(Boolean)
  const name = parts.pop() ?? p
  const dir = parts.length ? '…\\' : ''
  return dir + ellipsizeMiddle(name, maxWidth - displayWidth(dir))
}

export function basename(p: string): string {
  const parts = p.split(/[\\/]/).filter(Boolean)
  return parts[parts.length - 1] ?? p
}

/**
 * 给缩略图 URL 追加 cache-bust 查询串，强制 WebView2 重下。
 *
 * WebView2（Chromium）会按 URL 缓存图片；缩略图 bug 修掉后，旧的空白/403
 * 结果可能还躺在 `EBWebView\Default\Cache` 里。事件刷新时 URL 不变，浏览器
 * 认为「没变」就不重取，所以必须换 URL。实测 B站 CDN 对 `?_t=…` 照常 200。
 *
 * ⚠️ `data:` 内联图不能加查询串，原样返回。
 */
export function cacheBust(url: string | null | undefined, version: number): string | null {
  if (!url) return null
  if (url.startsWith('data:')) return url
  const sep = url.includes('?') ? '&' : '?'
  return `${url}${sep}_refresh=${version}`
}
