/*
 * 文件名模板。
 *
 * yt-dlp 的输出模板字段有上百个，界面上只给一个空输入框等于让用户去翻文档。
 * 这里给三样东西：**可用字段清单**（可点击插入）、**常用模板**、**实时预览**。
 *
 * 两条实测确认的坑，必须显式警告：
 *  1. 模板里没有 `%(ext)s` → 下出来的文件**真的没有扩展名**（不是自动补上）。
 *  2. 字段名拼错 → yt-dlp 静默填 `NA`，不会报错。
 */

export interface TplField {
  name: string
  desc: string
  /** 预览用的样例值。 */
  sample: string
}

export const TEMPLATE_FIELD_GROUPS: { group: string; fields: TplField[] }[] = [
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
export const TEMPLATE_FIELDS: TplField[] = TEMPLATE_FIELD_GROUPS.flatMap((g) => g.fields)
export const TEMPLATE_SAMPLE: Record<string, string> = Object.fromEntries(
  TEMPLATE_FIELDS.map((f) => [f.name, f.sample]),
)

export const DEFAULT_FILENAME_TEMPLATE = '%(title).150B [%(id)s].%(ext)s'

export const TEMPLATE_PRESETS = [
  { label: '默认', expr: DEFAULT_FILENAME_TEMPLATE },
  { label: '标题 + 日期', expr: '%(upload_date)s %(title).120B.%(ext)s' },
  { label: '上传者 + 标题', expr: '%(uploader)s - %(title).120B.%(ext)s' },
  { label: '列表序号 + 标题', expr: '%(playlist_index)02d - %(title).120B.%(ext)s' },
  { label: '列表名 / 序号', expr: '%(playlist)s/%(playlist_index)02d - %(title).120B.%(ext)s' },
  { label: '只用标题', expr: '%(title).150B.%(ext)s' },
]

/**
 * 按站点 / 上传者分子目录。每个都带 `|兜底值`：字段缺失时 yt-dlp 会填成 `NA`，
 * 不兜底就会冒出一个叫 NA 的目录。
 */
export const FOLDER_PRESETS = [
  { label: '按站点', expr: `%(extractor)s/${DEFAULT_FILENAME_TEMPLATE}` },
  { label: '按上传者', expr: `%(uploader,channel|未知作者).80B/${DEFAULT_FILENAME_TEMPLATE}` },
  { label: '按上传月份', expr: `%(upload_date>%Y-%m|未知日期)s/${DEFAULT_FILENAME_TEMPLATE}` },
  {
    label: '站点 / 上传者',
    expr: `%(extractor)s/%(uploader,channel|未知作者).80B/${DEFAULT_FILENAME_TEMPLATE}`,
  },
]

/**
 * 拆开字段括号里的内容：`uploader,channel>%Y|兜底` → 备选字段、日期格式、兜底值。
 */
export function parseField(inner: string): { names: string[]; dateFmt?: string; fallback?: string } {
  const bar = inner.indexOf('|')
  const fallback = bar >= 0 ? inner.slice(bar + 1) : undefined
  const head = bar >= 0 ? inner.slice(0, bar) : inner
  const gt = head.indexOf('>')
  const dateFmt = gt >= 0 ? head.slice(gt + 1) : undefined
  const names = (gt >= 0 ? head.slice(0, gt) : head).split(',').map((n) => n.trim()).filter(Boolean)
  return { names, dateFmt, fallback }
}

/** 修饰符速查——这些不是字段，是写在字段后面的格式说明。 */
export const TEMPLATE_MODIFIERS = [
  { code: '%(title).150B', desc: '按字节截断到 150（中文安全，防超长路径）' },
  { code: '%(title).50s', desc: '按字符截断到 50' },
  { code: '%(playlist_index)02d', desc: '数字补零到 2 位' },
  { code: '%(upload_date>%Y-%m-%d)s', desc: '把日期改成 2026-09-29 这种格式' },
]

/** 按 UTF-8 字节截断——和 yt-dlp 的 `.NB` 语义一致（中文一个字 3 字节）。 */
export function truncateBytes(str: string, n: number): string {
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
export function renderTemplate(tpl: string, sample: Record<string, string>): string {
  return tpl.replace(
    /%\(([^)]+)\)([-+#0-9.]*)([sSdDjJlLqB])/g,
    (whole, inner: string, spec: string, conv: string) => {
      const { names, dateFmt, fallback } = parseField(inner)
      const raw = names.map((n) => sample[n]).find((v) => v !== undefined) ?? fallback
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

/** 模板里出现的字段名（去重）。 */
export function fieldsOf(template: string): string[] {
  const names = new Set<string>()
  for (const m of template.matchAll(/%\(([^)]+)\)/g)) {
    for (const n of parseField(m[1]).names) names.add(n)
  }
  return [...names]
}
