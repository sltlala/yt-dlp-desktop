<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { api } from '../ipc'
import { useTaskStore } from '../stores/tasks'
import type { Subscription } from '../types'
import { fmtTime } from '../utils'

const store = useTaskStore()
const subs = ref<Subscription[]>([])
const url = ref('')
const adding = ref(false)
const checking = ref(false)
const error = ref<string | null>(null)
const notice = ref<string | null>(null)

const INTERVALS = [
  { value: 1, label: '每小时' },
  { value: 6, label: '每 6 小时' },
  { value: 24, label: '每天' },
  { value: 0, label: '不自动检查' },
]

let un: (() => void) | null = null
onMounted(async () => {
  subs.value = await api.listSubscriptions()
  un = await api.onSubscriptions((s) => (subs.value = s))
})
onUnmounted(() => un?.())

async function add() {
  const u = url.value.trim()
  if (!u || adding.value) return
  adding.value = true
  error.value = null
  notice.value = null
  try {
    const s = await api.addSubscription(u)
    if (!subs.value.some((x) => x.id === s.id)) subs.value.push(s)
    url.value = ''
    notice.value = `已订阅「${s.title}」，以后发布的新视频会自动加入下载`
  } catch (e) {
    error.value = String(e)
  } finally {
    adding.value = false
  }
}

async function checkNow(id?: string) {
  checking.value = true
  error.value = null
  notice.value = null
  try {
    const n = await api.checkSubscriptions(id)
    notice.value = n > 0 ? `发现 ${n} 个新视频，已加入下载` : '没有新视频'
    subs.value = await api.listSubscriptions()
  } catch (e) {
    error.value = String(e)
  } finally {
    checking.value = false
  }
}

async function toggle(s: Subscription) {
  await api.setSubscriptionEnabled(s.id, !s.enabled)
  s.enabled = !s.enabled
}

async function remove(s: Subscription) {
  await api.removeSubscription(s.id)
  subs.value = subs.value.filter((x) => x.id !== s.id)
}

async function setCheckInterval(hours: number) {
  if (!store.settings) return
  await store.saveSettings({ ...store.settings, subscriptionIntervalHours: hours })
}

function status(s: Subscription): string {
  if (!s.enabled) return '已暂停'
  if (s.lastError) return s.lastError
  const when = s.lastChecked ? fmtTime(s.lastChecked * 1000) : '尚未检查'
  return s.lastNew > 0 ? `${when} 检查，新增 ${s.lastNew} 个` : `${when} 检查`
}
</script>

<template>
  <div class="subs">
    <header class="page-head">
      <h1>订阅</h1>
      <p>订阅频道或播放列表后，定期检查新视频并自动加入下载。已有的视频不会下载。</p>
    </header>

    <div class="body">
      <form class="add-row" @submit.prevent="add">
        <input
          v-model="url"
          placeholder="频道或播放列表链接，例如 https://www.youtube.com/@xxx/videos"
          spellcheck="false"
          :disabled="adding"
        />
        <button class="btn primary" type="submit" :disabled="adding || !url.trim()">
          {{ adding ? '正在读取…' : '订阅' }}
        </button>
      </form>

      <div class="toolbar">
        <label>
          自动检查
          <select
            :value="store.settings?.subscriptionIntervalHours ?? 6"
            @change="setCheckInterval(Number(($event.target as HTMLSelectElement).value))"
          >
            <option v-for="i in INTERVALS" :key="i.value" :value="i.value">{{ i.label }}</option>
          </select>
        </label>
        <span class="spacer" />
        <button class="btn sm" :disabled="checking || subs.length === 0" @click="checkNow()">
          {{ checking ? '检查中…' : '立即检查全部' }}
        </button>
      </div>

      <p v-if="error" class="msg err">{{ error }}</p>
      <p v-else-if="notice" class="msg">{{ notice }}</p>

      <div v-if="subs.length === 0" class="empty">还没有订阅</div>
      <ul v-else class="list">
        <li v-for="s in subs" :key="s.id" :class="{ off: !s.enabled }">
          <div class="info">
            <div class="title">{{ s.title }}</div>
            <div class="url" :title="s.url">{{ s.url }}</div>
            <div class="status" :class="{ err: s.enabled && s.lastError }">{{ status(s) }}</div>
          </div>
          <div class="actions">
            <button class="btn sm ghost" :disabled="checking || !s.enabled" @click="checkNow(s.id)">
              检查
            </button>
            <button class="btn sm ghost" @click="toggle(s)">{{ s.enabled ? '暂停' : '恢复' }}</button>
            <button class="btn sm ghost danger" @click="remove(s)">取消订阅</button>
          </div>
        </li>
      </ul>
    </div>
  </div>
</template>

<style scoped>
.subs {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--bg);
}
.page-head {
  padding: 16px 32px 0;
}
.page-head h1 {
  margin: 0;
  font-size: var(--fs-xl);
  font-weight: 650;
  letter-spacing: -0.01em;
}
.page-head p {
  margin: 5px 0 0;
  font-size: var(--fs-xs);
  color: var(--text-mute);
}
.body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 18px 32px 24px;
  max-width: 820px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.add-row {
  display: flex;
  gap: 8px;
}
.add-row input,
.toolbar select {
  padding: 7px 11px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--surface);
  outline: none;
}
.add-row input {
  flex: 1;
  min-width: 0;
}
.add-row input:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-dim);
}
.toolbar {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: var(--fs-xs);
  color: var(--text-dim);
}
.toolbar label {
  display: flex;
  align-items: center;
  gap: 8px;
}
.spacer {
  flex: 1;
}
.msg {
  margin: 0;
  font-size: var(--fs-xs);
  color: var(--text-dim);
}
.msg.err,
.status.err {
  color: var(--err);
}
.empty {
  padding: 40px 0;
  text-align: center;
  color: var(--text-mute);
  font-size: var(--fs-md);
}
.list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.list li {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 14px;
  border: 1px solid var(--border-soft);
  border-radius: var(--radius);
  background: var(--surface);
}
.list li.off {
  opacity: 0.6;
}
.info {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.title {
  font-weight: 600;
}
.url,
.status {
  font-size: var(--fs-xs);
  color: var(--text-mute);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.actions {
  display: flex;
  gap: 4px;
  flex-shrink: 0;
}
</style>
