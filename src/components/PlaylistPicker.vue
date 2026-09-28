<script setup lang="ts">
import { computed, ref } from 'vue'
import type { PlaylistEntry } from '../types'
import { useTaskStore } from '../stores/tasks'
import { fmtDuration } from '../utils'

const props = defineProps<{ taskId: string; entries: PlaylistEntry[] }>()
const emit = defineEmits<{ close: [] }>()
const store = useTaskStore()

const selected = ref<Set<number>>(new Set(props.entries.map((_, i) => i)))
const filter = ref('')
const firstN = ref<number | null>(null)
const busy = ref(false)

const shown = computed(() => {
  const q = filter.value.trim().toLowerCase()
  const list = props.entries.map((e, i) => ({ e, i }))
  if (!q) return list
  return list.filter(({ e }) => e.title.toLowerCase().includes(q))
})

const allSelected = computed(() => selected.value.size === props.entries.length)

function toggle(i: number) {
  const s = new Set(selected.value)
  s.has(i) ? s.delete(i) : s.add(i)
  selected.value = s
}

function selectAll() {
  selected.value = new Set(props.entries.map((_, i) => i))
}

function selectNone() {
  selected.value = new Set()
}

function invert() {
  const s = new Set<number>()
  props.entries.forEach((_, i) => {
    if (!selected.value.has(i)) s.add(i)
  })
  selected.value = s
}

/** 「只下前 N 集」——合集动辄几百集，逐个勾选不现实。 */
function applyFirstN() {
  const n = firstN.value
  if (!n || n <= 0) return
  selected.value = new Set(props.entries.slice(0, n).map((_, i) => i))
}

const totalDuration = computed(() =>
  [...selected.value].reduce((s, i) => s + (props.entries[i]?.duration ?? 0), 0),
)

async function confirm() {
  if (selected.value.size === 0 || busy.value) return
  busy.value = true
  try {
    await store.startPlaylist(props.taskId, [...selected.value])
    emit('close')
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="mask" @click.self="emit('close')">
    <div class="modal">
      <header>
        <div>
          <h2>选择要下载的集数</h2>
          <p class="sub">共 {{ entries.length }} 集 · 已选 {{ selected.size }} 集 · 合计约 {{ fmtDuration(totalDuration) }}</p>
        </div>
        <button class="btn ghost sm" @click="emit('close')">✕</button>
      </header>

      <div class="toolbar">
        <input v-model="filter" class="search" type="search" placeholder="筛选标题…" />
        <button class="btn sm" @click="selectAll">全选</button>
        <button class="btn sm" @click="selectNone">全不选</button>
        <button class="btn sm" @click="invert">反选</button>
        <span class="spacer" />
        <input v-model.number="firstN" class="num" type="number" min="1" placeholder="N" />
        <button class="btn sm" @click="applyFirstN">只选前 N 集</button>
      </div>

      <div class="list">
        <label
          v-for="{ e, i } in shown"
          :key="i"
          class="item"
          :class="{ on: selected.has(i) }"
        >
          <input type="checkbox" :checked="selected.has(i)" @change="toggle(i)" />
          <span class="idx">{{ i + 1 }}</span>
          <span class="title" :title="e.title">{{ e.title }}</span>
          <span class="dur">{{ e.duration ? fmtDuration(e.duration) : '—' }}</span>
        </label>
        <div v-if="shown.length === 0" class="none">没有匹配的条目</div>
      </div>

      <footer>
        <span class="hint">
          选集通过 <code>--playlist-items</code> 交给 yt-dlp，不会为每集单独建任务
        </span>
        <button class="btn" @click="emit('close')">取消</button>
        <button class="btn primary" :disabled="selected.size === 0 || busy" @click="confirm">
          开始下载{{ allSelected ? '全部' : `（${selected.size} 集）` }}
        </button>
      </footer>
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
  z-index: 70;
  backdrop-filter: blur(3px);
}
.modal {
  width: 820px;
  max-width: 94vw;
  height: 78vh;
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
  align-items: flex-start;
  justify-content: space-between;
  padding: 15px 18px 13px;
  border-bottom: 1px solid var(--border-soft);
}
h2 {
  margin: 0;
  font-size: var(--fs-lg);
  font-weight: 650;
  letter-spacing: -0.01em;
}
.sub {
  margin: 3px 0 0;
  font-size: var(--fs-xs);
  color: var(--text-mute);
}

.toolbar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 10px 18px;
  border-bottom: 1px solid var(--border-soft);
}
.search {
  flex: 1;
  min-width: 0;
  padding: 6px 11px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--surface);
  outline: none;
  transition: border-color 0.13s, box-shadow 0.13s;
}
.search:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-dim);
}
.num {
  width: 66px;
  padding: 6px 9px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--surface);
  outline: none;
}
.num:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-dim);
}
.spacer {
  flex: 1;
}

.list {
  flex: 1;
  overflow-y: auto;
  padding: 8px 12px;
}
.item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 7px 9px;
  border-radius: var(--radius-xs);
  cursor: pointer;
  font-size: var(--fs-xs);
  transition: background 0.12s;
}
.item:hover {
  background: var(--surface-2);
}
.item.on {
  background: var(--accent-dim);
}
.item input {
  accent-color: var(--accent);
  flex-shrink: 0;
}
.idx {
  width: 34px;
  text-align: right;
  color: var(--text-mute);
  font-variant-numeric: tabular-nums;
  font-size: var(--fs-2xs);
  flex-shrink: 0;
}
.title {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.dur {
  color: var(--text-mute);
  font-size: var(--fs-2xs);
  font-variant-numeric: tabular-nums;
  flex-shrink: 0;
}
.none {
  padding: 24px;
  text-align: center;
  color: var(--text-mute);
  font-size: var(--fs-xs);
}

footer {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 13px 18px;
  border-top: 1px solid var(--border-soft);
  background: var(--bg-elev);
}
.hint {
  flex: 1;
  font-size: var(--fs-2xs);
  color: var(--text-mute);
}
.hint code {
  background: var(--surface-3);
  padding: 1px 5px;
  border-radius: 4px;
  color: var(--text-dim);
}
</style>
