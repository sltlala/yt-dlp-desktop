<script setup lang="ts">
import { ref } from 'vue'
import { useTaskStore } from '../stores/tasks'

const store = useTaskStore()
const url = ref('')
const busy = ref(false)
const dragOver = ref(false)
const feedback = ref<{ ok: boolean; text: string } | null>(null)

/** 把一段可能含多行的文本批量加进去。 */
async function addBulk(text: string) {
  const urls = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean)
  if (urls.length === 0) return
  busy.value = true
  try {
    const res = await store.addUrls(urls)
    const skipped = urls.length - res.added - res.failed.length
    const parts: string[] = []
    if (res.added > 0) parts.push(`已添加 ${res.added} 条`)
    if (skipped > 0) parts.push(`去重跳过 ${skipped} 条`)
    if (res.failed.length > 0) {
      parts.push(`无效 ${res.failed.length} 条`)
      console.warn('[addBulk] 无效链接', res.failed)
    }
    feedback.value = { ok: res.failed.length === 0, text: parts.join('，') || '未添加任何任务' }
    if (res.added > 0) url.value = ''
    window.setTimeout(() => (feedback.value = null), 5000)
  } finally {
    busy.value = false
  }
}

async function submit() {
  if (!url.value.trim() || busy.value) return
  await addBulk(url.value)
}

/** 多行粘贴：整段走批量添加（一条请求，避免 N 次往返）。 */
async function onPaste(e: ClipboardEvent) {
  const text = e.clipboardData?.getData('text') ?? ''
  const lines = text.split(/\r?\n/).filter((l) => l.trim())
  if (lines.length > 1) {
    e.preventDefault()
    await addBulk(text)
  }
}

/** 拖入 .txt / 多链接：用 HTML5 File API 读内容（WebView2 支持，无需后端命令）。 */
async function onDrop(e: DragEvent) {
  dragOver.value = false
  const file = e.dataTransfer?.files?.[0]
  if (file) {
    // 只认文本文件；.txt / .md / .log 等常见纯文本。太大的文件（>2MB）拒绝。
    if (file.size > 2 * 1024 * 1024) {
      feedback.value = { ok: false, text: '文件太大（>2MB），请只拖文本链接列表' }
      return
    }
    const text = await file.text()
    await addBulk(text)
  } else {
    // 没有文件（拖的是文本选区）：浏览器默认会把文本放进 dataTransfer。
    const text = e.dataTransfer?.getData('text') ?? ''
    if (text.trim()) await addBulk(text)
  }
}
</script>

<template>
  <div
    class="addbar"
    :class="{ 'drag-over': dragOver }"
    @dragover.prevent="dragOver = true"
    @dragleave.prevent="dragOver = false"
    @drop.prevent="onDrop"
  >
    <span class="paste-icon">🔗</span>
    <input
      v-model="url"
      class="url-input"
      type="text"
      placeholder="粘贴视频或播放列表链接；支持一次粘多个（每行一个），也可拖入 .txt"
      spellcheck="false"
      @keydown.enter="submit"
      @paste="onPaste"
    />
    <button class="btn primary" :disabled="!url.trim() || busy" @click="submit">
      {{ busy ? '添加中…' : '添加任务' }}
    </button>
  </div>
  <p v-if="feedback" class="bulk-feedback" :class="feedback.ok ? 'ok' : 'bad'">
    {{ feedback.text }}
  </p>
</template>

<style scoped>
.addbar {
  display: flex;
  align-items: center;
  gap: 9px;
  margin: 0 22px 14px;
  padding: 7px 8px 7px 13px;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-xs);
  transition: border-color 0.14s, box-shadow 0.14s;
}
.addbar:focus-within {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-dim);
}
.addbar.drag-over {
  border-color: var(--accent);
  background: var(--accent-dim);
}
.paste-icon {
  font-size: var(--fs-md);
  opacity: 0.5;
}
.url-input {
  flex: 1;
  min-width: 0;
  padding: 6px 0;
  border: none;
  background: transparent;
  outline: none;
}
.url-input::placeholder {
  color: var(--text-mute);
}
.bulk-feedback {
  margin: -6px 22px 14px;
  font-size: var(--fs-sm);
}
.bulk-feedback.ok {
  color: var(--ok, #2e9e5b);
}
.bulk-feedback.bad {
  color: var(--danger, #d64545);
}
</style>
