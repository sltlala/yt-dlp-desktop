<script setup lang="ts">
import { ref } from 'vue'
import { useTaskStore } from '../stores/tasks'

const store = useTaskStore()
const url = ref('')
const busy = ref(false)

async function submit() {
  if (!url.value.trim() || busy.value) return
  busy.value = true
  try {
    await store.addUrl(url.value)
    url.value = ''
  } finally {
    busy.value = false
  }
}

/** 多行粘贴：一次加多个链接。 */
async function onPaste(e: ClipboardEvent) {
  const text = e.clipboardData?.getData('text') ?? ''
  const lines = text.split(/\s+/).filter((l) => /^https?:\/\//i.test(l))
  if (lines.length > 1) {
    e.preventDefault()
    for (const l of lines) await store.addUrl(l)
    url.value = ''
  }
}
</script>

<template>
  <div class="addbar">
    <span class="paste-icon">🔗</span>
    <input
      v-model="url"
      class="url-input"
      type="text"
      placeholder="粘贴视频或播放列表链接，支持一次粘贴多个（每行一个）"
      spellcheck="false"
      @keydown.enter="submit"
      @paste="onPaste"
    />
    <button class="btn primary" :disabled="!url.trim() || busy" @click="submit">添加任务</button>
  </div>
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
</style>
