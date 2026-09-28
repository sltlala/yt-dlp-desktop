<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api } from '../ipc'
import type { FormatOption, FormatPreset } from '../types'
import { fmtBytes } from '../utils'

const props = defineProps<{
  expression: string
  url: string
  /** 任务探测时已经拿到的格式表。有值就直接用，不再启动一次 yt-dlp。 */
  formats?: FormatOption[]
  /** 当前表达式是不是用户手动指定的（决定要不要显示「恢复默认」）。 */
  overridden?: boolean
}>()
const emit = defineEmits<{ close: []; apply: [expression: string] }>()

const mode = ref<'preset' | 'advanced'>('preset')
const formats = ref<FormatOption[]>([])
const picked = ref<string | null>(null)
const loading = ref(false)

onMounted(async () => {
  // 预设的表达式**由后端按当前编码偏好算好**（DESIGN §3.3）。
  // 原先这张表硬编码在这里，于是设置里选了「优先 H.264」之后，
  // 从选择器里点「1080p」又会把偏好丢掉——`-f` 被抄成了两份。
  try {
    presets.value = await api.formatPresets()
  } catch {
    // 取不到就退回不带偏好的默认——**不能让「预设」页签变成一片空白**，
    // 那看起来像功能坏了。（这些值与 `preset_expression` 的无偏好输出一致。）
    presets.value = [
      { label: '最佳画质', expr: 'bv*+ba/b', note: '自动挑最优视频轨与音频轨' },
      { label: '1080p', expr: 'bv*[height<=1080]+ba/b[height<=1080]/b', note: '不超过 1920×1080' },
      { label: '720p', expr: 'bv*[height<=720]+ba/b[height<=720]/b', note: '省流量' },
      { label: '仅音频 MP3', expr: 'ba/b', note: '提取音频并转码' },
    ]
  }

  if (props.formats?.length) {
    formats.value = props.formats
    return
  }
  loading.value = true
  try {
    formats.value = await api.probeFormats(props.url)
  } finally {
    loading.value = false
  }
})

const presets = ref<FormatPreset[]>([])

/* ───────────────── 音视频分开时怎么选 ─────────────────
 *
 * YouTube 之类的 DASH 站点会把**视频轨和音频轨分开列**，单点一条视频轨是没有
 * 声音的——这就是「有时候音频和视频会分开」的来源。三类要分清楚：
 *
 *   视频+音频  已封装在一起，选了就能用
 *   仅视频     必须再配一条音频轨，界面自动配 `+ba`
 *   仅音频     就是一条音轨
 *
 * 早期版本对所有「有视频编码」的行都追加 `+ba`，于是选了 `18`（本来就带音频）
 * 会变成 `18+ba/18`。翻译规则现在在核心层（`format_expression_for`）并有单测。
 */

type Kind = 'muxed' | 'video' | 'audio'

/**
 * yt-dlp 用 `"none"` 字符串表示「没有这一路」，`null` 也表示没有。
 *
 * ⚠️ 这里的判定必须与 Rust 侧 `ytdlp_core::probe::format_kind` **逐字一致**，
 * 否则同一行在前后端会得到不同的类型与表达式（DESIGN §3 反复强调的分叉问题）。
 */
function has(codec: string | null | undefined): boolean {
  return !!codec && codec !== 'none'
}

function kindOf(f: FormatOption): Kind {
  const v = has(f.vcodec)
  const a = has(f.acodec)
  if (v && a) return 'muxed'
  if (v) return 'video'
  if (a) return 'audio'
  // 两路编码都缺 = generic 提取器给直链文件的情形：那是一个**完整可用的文件**，
  // 当已封装处理（表达式就是它自己），不能配 `+ba` 也不能当成纯音轨。
  return 'muxed'
}

const KIND_LABEL: Record<Kind, string> = {
  muxed: '视频+音频',
  video: '仅视频',
  audio: '仅音频',
}

const KIND_HINT: Record<Kind, string> = {
  muxed: '已封装在一起，选了就能直接用',
  video: '没有声音，会自动搭配一条最佳音频轨',
  audio: '一条音轨，没有画面',
}

/** 分辨率高度，用于排序；音频轨没有高度。 */
function heightOf(f: FormatOption): number {
  const m = /(\d+)\s*[x×]\s*(\d+)/.exec(f.resolution)
  if (m) return Number(m[2])
  const h = /^(\d+)p$/.exec(f.resolution)
  return h ? Number(h[1]) : 0
}

/**
 * 排序：能出画面的在前，按分辨率从高到低；同分辨率下「视频+音频」在「仅视频」
 * 之前（更省事的选择先出现）。音轨排在最后，按码率从高到低。
 */
const sorted = computed(() =>
  [...formats.value].sort((a, b) => {
    const ka = kindOf(a)
    const kb = kindOf(b)
    const rank = (k: Kind) => (k === 'audio' ? 1 : 0)
    if (rank(ka) !== rank(kb)) return rank(ka) - rank(kb)

    if (ka === 'audio' && kb === 'audio') return (b.tbr ?? 0) - (a.tbr ?? 0)

    const ha = heightOf(a)
    const hb = heightOf(b)
    if (ha !== hb) return hb - ha
    if (ka !== kb) return ka === 'muxed' ? -1 : 1
    return (b.tbr ?? 0) - (a.tbr ?? 0)
  }),
)

const pickedFormat = computed(() =>
  picked.value ? (formats.value.find((x) => x.formatId === picked.value) ?? null) : null,
)

/**
 * `+ba` 里的 `ba` 由 yt-dlp 挑，这里只是把**预计会配上**的那条显示给用户。
 * 选音轨的规则与 yt-dlp 的 `bestaudio` 一致：优先码率高、容器能合得进去。
 */
const bestAudio = computed(() =>
  formats.value
    .filter((f) => kindOf(f) === 'audio')
    .sort((a, b) => (b.tbr ?? 0) - (a.tbr ?? 0))[0] ?? null,
)

/** 选中的行反解回表达式——**绝不存 format_id**（DESIGN §3）。 */
const derived = computed(() => {
  const f = pickedFormat.value
  if (!f) return ''
  if (kindOf(f) === 'video') return `${f.formatId}+ba/${f.formatId}`
  return f.formatId
})

/** 最终会写回任务的表达式：高级模式用选中的行，否则用当前预设。 */
const effectiveExpr = computed(() =>
  mode.value === 'advanced' ? derived.value : props.expression,
)
</script>

<template>
  <div class="mask" @click.self="emit('close')">
    <div class="modal">
      <header>
        <h2>选择下载格式</h2>
        <button class="btn ghost sm" @click="emit('close')">✕</button>
      </header>

      <div class="tabs">
        <button :class="{ on: mode === 'preset' }" @click="mode = 'preset'">预设</button>
        <button :class="{ on: mode === 'advanced' }" @click="mode = 'advanced'">
          高级（完整格式表）
        </button>
      </div>

      <div class="body">
        <!-- ── 预设 ── -->
        <div v-if="mode === 'preset'" class="presets">
          <button
            v-for="p in presets"
            :key="p.label"
            class="preset"
            :class="{ on: expression === p.expr }"
            @click="emit('apply', p.expr)"
          >
            <div class="p-label">{{ p.label }}</div>
            <div class="p-note">{{ p.note }}</div>
            <code class="mono">{{ p.expr }}</code>
          </button>
        </div>

        <!-- ── 高级 ── -->
        <div v-else-if="loading" class="loading">正在读取格式列表…</div>
        <div v-else-if="formats.length === 0" class="loading">
          没有可用的格式信息（探测可能失败或该链接不支持）
        </div>
        <template v-else>
          <!--
            这个提示是必须的：DASH 站点把音视频分开列，用户看到两列里各有一半是
            「—」会直接懵住（「这样怎么选择」）。
          -->
          <p class="kind-note">
            这个站点的视频轨和音频轨是<strong>分开</strong>的，所以列表里既有「仅视频」
            也有「仅音频」。<strong>选「仅视频」那一行就行</strong>——会自动配上一条最佳
            音频轨（<code class="mono">+ba</code>），不用自己再挑一遍。
          </p>

          <table class="fmt">
            <thead>
              <tr>
                <th></th>
                <th>ID</th>
                <th>类型</th>
                <th>容器</th>
                <th>分辨率</th>
                <th>帧率</th>
                <th>编码</th>
                <th class="r">大小</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="f in sorted"
                :key="f.formatId"
                :class="[kindOf(f), { on: picked === f.formatId }]"
                :title="KIND_HINT[kindOf(f)]"
                @click="picked = f.formatId"
              >
                <td><input type="radio" :checked="picked === f.formatId" /></td>
                <td class="mono">{{ f.formatId }}</td>
                <td>
                  <span class="kind" :class="kindOf(f)">{{ KIND_LABEL[kindOf(f)] }}</span>
                </td>
                <td>{{ f.ext }}</td>
                <td>{{ f.resolution }}</td>
                <td>{{ f.fps ?? '—' }}</td>
                <td class="mono dim">
                  {{ has(f.vcodec) ? f.vcodec : f.acodec }}
                </td>
                <td class="r">{{ fmtBytes(f.filesize) }}</td>
              </tr>
            </tbody>
          </table>
        </template>
      </div>

      <footer>
        <div class="expr">
          <span class="lbl">将保存为表达式</span>
          <code class="mono">{{ effectiveExpr || '请选择一行' }}</code>
          <!-- 选了仅视频轨时，明确告诉用户音频从哪来 -->
          <span v-if="mode === 'advanced' && pickedFormat && kindOf(pickedFormat) === 'video'" class="pair">
            自动搭配最佳音频轨<template v-if="bestAudio">
              （{{ bestAudio.formatId }} · {{ bestAudio.ext }} ·
              {{ bestAudio.tbr ? Math.round(bestAudio.tbr) + 'k' : '—' }}）</template
            >
          </span>
        </div>
        <button class="btn" @click="emit('close')">取消</button>
        <button
          v-if="overridden"
          class="btn"
          title="改回跟随「设置 → 格式」里的预设"
          @click="emit('apply', '')"
        >
          恢复默认
        </button>
        <button
          class="btn primary"
          :disabled="!effectiveExpr"
          :title="
            mode === 'advanced' && derived
              ? '按新格式重新下载这个任务'
              : '按这个预设重新下载这个任务'
          "
          @click="emit('apply', effectiveExpr)"
        >
          按此格式重新下载
        </button>
      </footer>

      <p class="foot-note">
        保存的是 <code>-f</code> 表达式本身，而不是 format_id 列表——这样任务永远可以重放，
        也不需要在数据库里保存 info.json。
      </p>
    </div>
  </div>
</template>

<style scoped>
.mask {
  position: fixed;
  inset: 0;
  background: rgba(16, 24, 40, 0.28);
  display: grid;
  place-items: center;
  z-index: 60;
  backdrop-filter: blur(3px);
}
.modal {
  width: 760px;
  max-width: 94vw;
  max-height: 88vh;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-lg);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 15px 18px;
  border-bottom: 1px solid var(--border-soft);
}
header h2 {
  margin: 0;
  font-size: var(--fs-lg);
  font-weight: 650;
  letter-spacing: -0.01em;
}

.tabs {
  display: flex;
  gap: 2px;
  margin: 12px 16px 0;
  padding: 3px;
  background: var(--surface-2);
  border-radius: var(--radius-sm);
}
.tabs button {
  flex: 1;
  padding: 6px 12px;
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
  overflow: auto;
  padding: 14px 18px;
}

.presets {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
}

/* 音视频分开站点的说明横幅 */
.kind-note {
  margin: 0 0 12px;
  padding: 10px 12px;
  font-size: var(--fs-xs);
  line-height: 1.6;
  color: var(--text-dim);
  background: var(--surface-2);
  border-radius: var(--radius-sm);
  border-left: 2px solid var(--accent);
}
.kind-note strong {
  color: var(--text);
}
.kind-note code {
  background: var(--surface-3);
  padding: 1px 5px;
  border-radius: 4px;
}

/* 类型标记：三类要一眼分得开，否则用户不知道该点哪一行 */
.kind {
  display: inline-block;
  padding: 1px 8px;
  border-radius: 999px;
  font-size: var(--fs-2xs);
  font-weight: 600;
  white-space: nowrap;
}
.kind.muxed {
  background: rgba(15, 138, 69, 0.1);
  color: var(--ok);
}
.kind.video {
  background: var(--accent-dim);
  color: var(--accent-ink);
}
.kind.audio {
  background: rgba(102, 112, 133, 0.1);
  color: var(--skip);
}

.loading {
  padding: 30px;
  text-align: center;
  color: var(--text-mute);
  font-size: var(--fs-xs);
}
.preset {
  text-align: left;
  padding: 12px 14px;
  border-radius: var(--radius-sm);
  background: var(--surface);
  border: 1px solid var(--border);
  display: flex;
  flex-direction: column;
  gap: 3px;
  transition: border-color 0.13s, background 0.13s, box-shadow 0.13s;
}
.preset:hover {
  border-color: #d7dce3;
  background: var(--surface-2);
}
.preset.on {
  border-color: var(--accent);
  background: var(--accent-dim);
  box-shadow: 0 0 0 3px var(--accent-dim);
}
.p-label {
  font-weight: 650;
  font-size: var(--fs-xs);
}
.p-note {
  font-size: var(--fs-2xs);
  color: var(--text-mute);
}
.preset code {
  margin-top: 4px;
  font-size: var(--fs-2xs);
  color: var(--text-mute);
  background: var(--surface-2);
  padding: 3px 7px;
  border-radius: var(--radius-xs);
  overflow: hidden;
  text-overflow: ellipsis;
}

table.fmt {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--fs-xs);
}
table.fmt th {
  text-align: left;
  padding: 7px 8px;
  color: var(--text-mute);
  font-weight: 600;
  font-size: var(--fs-2xs);
  border-bottom: 1px solid var(--border);
  position: sticky;
  top: 0;
  background: var(--surface);
}
table.fmt th.r,
table.fmt td.r {
  text-align: right;
}
table.fmt td {
  padding: 7px 8px;
  border-bottom: 1px solid var(--border-soft);
  color: var(--text-dim);
}
table.fmt tbody tr {
  cursor: pointer;
}
table.fmt tbody tr:hover {
  background: var(--surface-2);
}
table.fmt tbody tr.on {
  background: var(--accent-dim);
}
/* 仅视频轨要配音频，行本身给一点提示色，扫一眼就知道这类不能单独用 */
table.fmt tbody tr.audio td {
  color: var(--text-mute);
}
td.mono {
  font-family: 'Cascadia Mono', Consolas, monospace;
  font-size: var(--fs-2xs);
}
td.dim {
  color: var(--text-mute);
}
input[type='radio'] {
  accent-color: var(--accent);
}

footer {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 13px 18px 6px;
  border-top: 1px solid var(--border-soft);
}
.expr {
  margin-right: auto;
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
/* 表达式 + 搭配说明在同一行，窄了也不换行挤掉按钮 */
.expr .pair {
  display: block;
}
.lbl {
  font-size: var(--fs-2xs);
  color: var(--text-mute);
}
.expr code {
  font-size: var(--fs-xs);
  color: var(--accent-ink);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
/* 选了仅视频轨时，把「音频从哪来」写清楚 */
.pair {
  font-size: var(--fs-2xs);
  color: var(--text-mute);
}
.foot-note {
  margin: 0;
  padding: 0 18px 15px;
  font-size: var(--fs-2xs);
  color: var(--text-mute);
  line-height: 1.55;
}
</style>
