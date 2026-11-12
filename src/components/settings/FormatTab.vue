<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { api } from '../../ipc'
import type { CodecChoices } from '../../types'
import { deepClone } from '../../utils'
import { useSettingsContext } from './context'

const { s } = useSettingsContext()

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

onMounted(() => void loadCodecChoices())
</script>

<template>
  <section>
    <div class="two-col">
      <label class="field">
        <span>默认画质</span>
        <select v-model="s.preset">
          <option value="best">最佳画质</option>
          <option value="maxHeight">限制分辨率</option>
          <option value="audioOnly">仅音频</option>
          <option value="subsOnly">仅字幕</option>
          <option value="thumbnailOnly">仅封面</option>
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
    </div>
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

    <p v-if="s.preset === 'subsOnly'" class="note">
      只下载字幕文件（不下载视频本体）。字幕语言在本页下方「嵌入 → 字幕语言」里设置
      （默认 {{ s.embed.subLangs }}）。
    </p>
    <p v-if="s.preset === 'thumbnailOnly'" class="note">
      只下载封面缩略图（不下载视频本体）。
    </p>

    <!-- 只在**真的会失败**的组合下提示，平时不占版面 -->
    <p v-if="s.container === 'webm' && s.embed.thumbnail" class="note warn">
      WebM 装不下缩略图，这样组合会<strong>直接失败</strong>——请改用 MKV 或「自动」。
    </p>
    <p v-else-if="s.container === 'mp4' && s.embed.subs" class="note warn">
      MP4 的字幕只能用 mov_text；想要完整字幕样式请改用 MKV。
    </p>

    <h3>优先选择编码</h3>
    <p class="note">
      只是一种<strong>偏好</strong>：站点没有首选编码时会自动退到次选，不会因此下不到。
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

    <h3>嵌入</h3>
    <div class="check-row">
      <label class="check">
        <input v-model="s.embed.subs" type="checkbox" />
        <span>嵌入字幕</span>
      </label>
      <label class="check">
        <input v-model="s.embed.thumbnail" type="checkbox" />
        <span>嵌入缩略图</span>
      </label>
      <label class="check">
        <input v-model="s.embed.metadata" type="checkbox" />
        <span>嵌入元数据</span>
      </label>
    </div>
    <template v-if="s.embed.subs">
      <label class="field inline wide">
        <span>字幕语言</span>
        <input
          v-model="s.embed.subLangs"
          class="mono"
          spellcheck="false"
          placeholder="all,-live_chat,-danmaku（逗号分隔，支持 - 排除）"
        />
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

    <label v-if="s.embed.thumbnail" class="check sub">
      <input v-model="s.embed.keepThumbnailFile" type="checkbox" />
      <span>同时保留缩略图文件</span>
    </label>

    <template v-if="s.embed.metadata">
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
</template>
