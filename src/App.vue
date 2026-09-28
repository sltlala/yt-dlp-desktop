<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { useTaskStore, type Filter } from './stores/tasks'
import AddUrlBar from './components/AddUrlBar.vue'
import TaskRow from './components/TaskRow.vue'
import SettingsPanel from './components/SettingsPanel.vue'
import { useVirtualList } from './composables/useVirtualList'
import { fmtSpeed, shortenPath } from './utils'
import { isTauri } from './ipc'

const store = useTaskStore()
const query = ref('')

/**
 * 开发期快照辅助：`?panel=settings|detail|formats` 可直接打开对应视图，
 * 便于无人值守截图与视觉回归。
 */
const panel = new URLSearchParams(window.location.search).get('panel')

/**
 * 设置是**主区的一个页面**，不是右侧抽屉（见 DESIGN §9.2）。
 *
 * 之前做成抽屉时，抽屉盖住的那半边窗口完全浪费，而且「设置」和任务列表
 * 是同一层级的导航目标，用两种不同的交互承载同一层级的东西会让人困惑。
 */
const view = ref<'tasks' | 'settings'>(panel === 'settings' ? 'settings' : 'tasks')

function showTasks(f: Filter) {
  store.filter = f
  view.value = 'tasks'
}

/* ───────────────── 多选（DESIGN §9.3）─────────────────
 *
 * 多选控件放在**列表正上方**，而不是侧栏底部：
 * 它作用于列表里的东西，就该跟列表在一起；侧栏是导航，不是操作区。
 * 全选/反选只覆盖**当前可见**的项（`shownKeys` 已按筛选与搜索过滤过）。
 */
function enterSelect() {
  store.clearSelection()
  store.selectMode = true
  store.expanded = null
}

/** 删除所选只是**移除记录**，不动磁盘上的成品文件（DESIGN §13）。 */
async function removeSelected() {
  if (store.selected.size === 0) return
  await store.removeMany()
}

/**
 * 换筛选条件时清空选择。
 *
 * 否则会出现「已选 3 项」但列表里一个勾都看不见——被筛掉的项仍然会被删掉，
 * 这正是 DESIGN §13 想避免的那种「用户不知道自己要删什么」。
 */
watch(
  () => store.filter,
  () => {
    if (store.selectMode) store.clearSelected()
  },
)
watch(view, () => {
  if (store.selectMode) store.clearSelection()
})

let statsTimer: number | null = null

onMounted(async () => {
  await store.init()
  if (panel === 'detail' || panel === 'formats' || panel === 'playlist') {
    store.expanded = store.tasks[0]?.id ?? null
  }
  // 调度器状态是辅助信息，用低频轮询即可，不必走事件通道。
  await store.refreshStats()
  statsTimer = window.setInterval(() => store.refreshStats(), 1500)
})

onUnmounted(() => {
  if (statsTimer !== null) window.clearInterval(statsTimer)
})

const filters: { key: Filter; label: string }[] = [
  { key: 'all', label: '全部任务' },
  { key: 'active', label: '进行中' },
  { key: 'completed', label: '已完成' },
  { key: 'failed', label: '失败' },
]

const shown = computed(() => {
  const q = query.value.trim().toLowerCase()
  if (!q) return store.visible
  return store.visible.filter(
    (t) => t.title.toLowerCase().includes(q) || t.url.toLowerCase().includes(q),
  )
})

const postLabel = computed(() => {
  const n = store.postProcessing.length
  if (n === 0) return null
  return `${n} 个任务正在后处理`
})

/**
 * 侧栏的输出目录标签。
 *
 * 侧栏只有 200px 宽，长路径只显示**尾部**（`…\父目录\目录`）比显示开头有用。
 * 截断放在 JS 里做，不用 `direction: rtl`——那个会把结尾的 `\` 挪到最前面
 * （见 `.stat .v.path` 的注释）。完整路径仍在 `title` 里。
 */
const outputDirLabel = computed(() => {
  const d = store.settings?.outputDir?.trim()
  if (!d) return '—'
  return shortenPath(d.replace(/[\\/]+$/, ''), 2)
})

/* ───────────────── 虚拟滚动（DESIGN §9）─────────────────
 *
 * 任务可能上百条，全量渲染会让 DOM 直接爆炸。这里只渲染视口内的项，
 * 高度靠实测修正（展开详情后行高会变，固定行高的做法会整列错位）。
 */
const listWrap = ref<HTMLElement | null>(null)
const shownKeys = computed(() => shown.value.map((t) => t.id))
const taskMap = computed(() => new Map(shown.value.map((t) => [t.id, t])))
const taskByKey = (key: string) => taskMap.value.get(key)!

const {
  items: virtualItems,
  totalHeight,
  refresh: refreshVirtual,
} = useVirtualList(listWrap, shownKeys)

// 列表内容变化（筛选/增删）后要重新测量
watch(shownKeys, () => refreshVirtual())
// 展开/收起会改变行高；容器的 ResizeObserver 捕捉不到内部变化，得显式触发
watch(
  () => store.expanded,
  () => nextTick(() => refreshVirtual()),
)

/** 探测池 / 下载池各自的排队情况——两个池是独立的，必须分开显示（DESIGN §4）。 */
const queueLabel = computed(() => {
  const s = store.stats
  if (!s) return null
  const parts: string[] = []
  if (s.probing) parts.push(`探测中 ${s.probing}`)
  if (s.probeQueued) parts.push(`待探测 ${s.probeQueued}`)
  if (s.downloadQueued) parts.push(`待下载 ${s.downloadQueued}`)
  return parts.length ? parts.join(' · ') : null
})

const hostLimitLabel = computed(() => {
  const s = store.stats
  if (!s) return null
  return `下载 ${s.downloading}/${s.downloadLimit} · 同站 ${s.perHostLimit}`
})

/** 事件通道不可用时的「重试」：重载会重新走一遍订阅。 */
function reload() {
  window.location.reload()
}
</script>

<template>
  <div class="app">
    <!-- ─────────── 侧栏 ─────────── -->
    <aside class="sidebar">
      <div class="brand">
        <div class="logo">yt</div>
        <div class="brand-text">
          <strong>yt-dlp 下载器</strong>
          <span>yt-dlp 2026.07.04</span>
        </div>
      </div>

      <nav class="nav">
        <button
          v-for="f in filters"
          :key="f.key"
          class="nav-item"
          :class="{ active: view === 'tasks' && store.filter === f.key }"
          @click="showTasks(f.key)"
        >
          <span>{{ f.label }}</span>
          <span class="count">{{ store.counts[f.key] }}</span>
        </button>

        <div class="nav-sep" />

        <button
          class="nav-item"
          :class="{ active: view === 'settings' }"
          @click="view = 'settings'"
        >
          <span>设置</span>
        </button>
      </nav>

      <div class="side-stats">
        <div class="stat">
          <span class="k">下载并发</span>
          <span class="v">{{ store.activeCount }} / {{ store.settings?.downloadConcurrency ?? '—' }}</span>
        </div>
        <div class="stat">
          <span class="k">总速度</span>
          <span class="v accent">{{ fmtSpeed(store.totalSpeed) }}</span>
        </div>
        <div class="stat">
          <span class="k">输出目录</span>
          <span class="v path" :title="store.settings?.outputDir ?? ''">{{ outputDirLabel }}</span>
        </div>
      </div>
    </aside>

    <!-- ─────────── 主区 ─────────── -->
    <main class="main">
      <!-- 设置是主区的一个页面，不是浮层 -->
      <SettingsPanel
        v-if="view === 'settings' && store.settings"
        @close="view = 'tasks'"
      />

      <template v-else>
        <header class="topbar">
          <div class="title-row">
            <h1>下载任务</h1>
            <span v-if="!isTauri()" class="demo-badge" title="当前运行在浏览器中，数据来自演示后端">
              演示模式
            </span>
          </div>
          <div class="top-actions">
            <input v-model="query" class="search" type="search" placeholder="搜索标题或链接…" />
            <button
              v-if="!store.selectMode"
              class="btn sm"
              :disabled="shown.length === 0"
              title="多选后可以一次性移除多条记录"
              @click="enterSelect"
            >
              多选
            </button>
          </div>
        </header>

        <!-- 多选工具条：占据添加栏的位置，避免两排控件打架 -->
        <div v-if="store.selectMode" class="selbar">
          <span class="sel-count">
            已选 <strong>{{ store.selected.size }}</strong> 项
            <em v-if="store.selected.size === 0">· 点列表里的任意一行即可勾选</em>
          </span>
          <span class="sel-spacer" />
          <button class="btn sm" @click="store.selectAll(shownKeys)">全选</button>
          <button class="btn sm" @click="store.invertSelection(shownKeys)">反选</button>
          <button
            class="btn sm"
            :disabled="store.selected.size === 0"
            @click="store.clearSelected()"
          >
            清空
          </button>
          <button
            class="btn sm danger"
            :disabled="store.selected.size === 0"
            title="只从列表移除记录并清理临时文件；不删除已下载的成品文件"
            @click="removeSelected"
          >
            移除所选记录 ({{ store.selected.size }})
          </button>
          <button class="btn sm" @click="store.clearSelection()">完成</button>
        </div>

        <AddUrlBar v-else />

        <!-- 错误必须显式呈现：静默的 Promise rejection 曾让功能完全无反应 -->
        <div v-if="store.lastError" class="err-banner">
          <span class="eb-icon">!</span>
          <span class="eb-text">{{ store.lastError }}</span>
          <button
            v-if="store.polling"
            class="btn ghost sm"
            title="重新订阅事件通道"
            @click="reload()"
          >
            重试
          </button>
          <button class="btn ghost sm" @click="store.lastError = null">✕</button>
        </div>

        <div ref="listWrap" class="list-wrap">
          <div v-if="shown.length === 0" class="empty">
            <div class="empty-icon">⬇</div>
            <div>没有任务</div>
            <div class="empty-sub">把视频链接粘贴到上面的输入框，或直接拖入窗口</div>
          </div>

          <!-- 虚拟滚动：只渲染视口内的项，用一个撑高的占位层维持滚动条 -->
          <div v-else class="list vlist" :style="{ height: totalHeight + 'px' }">
            <div
              v-for="it in virtualItems"
              :key="it.key"
              class="vitem"
              :data-vkey="it.key"
              :style="{ transform: `translateY(${it.offset}px)` }"
            >
              <TaskRow :task="taskByKey(it.key)" />
            </div>
          </div>
        </div>

        <footer class="statusbar">
          <div class="sb-left">
            <span v-if="postLabel" class="sb-post">{{ postLabel }}</span>
            <span v-else-if="queueLabel" class="sb-queue">{{ queueLabel }}</span>
            <span v-else class="sb-idle">空闲</span>
          </div>
          <div class="sb-right">
            <span v-if="hostLimitLabel" class="sb-host">{{ hostLimitLabel }}</span>
            <span v-if="hostLimitLabel" class="sep">·</span>
            <span>共 {{ store.counts.all }} 个任务</span>
            <span class="sep">·</span>
            <span>进行中 {{ store.counts.active }}</span>
            <span class="sep">·</span>
            <span>已完成 {{ store.counts.completed }}</span>
            <span v-if="store.counts.failed" class="sep">·</span>
            <span v-if="store.counts.failed" class="err">失败 {{ store.counts.failed }}</span>
          </div>
        </footer>
      </template>
    </main>
  </div>
</template>

<style scoped>
.app {
  display: grid;
  grid-template-columns: 224px 1fr;
  /* 必须显式约束行高：只给 columns 时隐式行是 auto，
     `.main` 会被内容撑开，`.list-wrap` 的 overflow-y 就形同虚设
     （任务一多整页会跟着变长，虚拟滚动也拿不到真实视口高度）。 */
  grid-template-rows: minmax(0, 1fr);
  height: 100%;
  overflow: hidden;
}

/* ── 侧栏 ── */
.sidebar {
  display: flex;
  flex-direction: column;
  background: var(--bg-elev);
  border-right: 1px solid var(--border-soft);
  padding: 16px 14px;
  gap: 18px;
}

.brand {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 2px 4px;
}
.logo {
  width: 34px;
  height: 34px;
  border-radius: 10px;
  background: linear-gradient(140deg, #5b8cf8, #3d6ef5 55%, #6d5cf0);
  display: grid;
  place-items: center;
  font-weight: 700;
  font-size: var(--fs-md);
  color: #fff;
  letter-spacing: -0.02em;
  box-shadow: 0 3px 8px rgba(61, 110, 245, 0.32);
  flex-shrink: 0;
}
.brand-text {
  display: flex;
  flex-direction: column;
  line-height: 1.3;
  min-width: 0;
}
.brand-text strong {
  font-size: var(--fs-md);
  font-weight: 650;
}
.brand-text span {
  font-size: var(--fs-2xs);
  color: var(--text-mute);
}

.nav {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.nav-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 11px;
  border-radius: var(--radius-sm);
  color: var(--text-dim);
  font-weight: 500;
  transition: background 0.12s, color 0.12s;
}
.nav-item:hover {
  background: var(--surface-2);
  color: var(--text);
}
.nav-item.active {
  background: var(--surface);
  color: var(--accent-ink);
  font-weight: 600;
  box-shadow: var(--shadow-xs);
}
.count {
  font-size: var(--fs-xs);
  color: var(--text-mute);
  font-variant-numeric: tabular-nums;
}
.nav-item.active .count {
  color: var(--accent);
}

/* 筛选与设置是两组不同的导航目标，用一条分隔线断开 */
.nav-sep {
  height: 1px;
  margin: 9px 10px;
  background: var(--border);
}

.side-stats {
  margin-top: auto;
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px;
  background: var(--surface);
  border: 1px solid var(--border-soft);
  border-radius: var(--radius);
  box-shadow: var(--shadow-xs);
}
.stat {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  font-size: var(--fs-xs);
}
.stat .k {
  color: var(--text-mute);
  flex-shrink: 0;
}
.stat .v {
  color: var(--text-dim);
  font-variant-numeric: tabular-nums;
  text-align: right;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.stat .v.accent {
  color: var(--accent);
  font-weight: 650;
}
.stat .v.path {
  font-size: var(--fs-2xs);
  /*
   * 这里**不能**用 `direction: rtl` 做「从左边截断」那一套。
   * 它会把路径里的中性字符重排：`E:\下载\视频\` 在 RTL 段落里，结尾那个 `\`
   * 被 bidi 算法挪到了最前面，界面上显示成 `\E:\下载\视频`——看着像路径本身坏了。
   * 现在改成在 JS 里用 `shortenPath()` 截，结果可预测，也不再依赖 bidi 行为。
   */
}

/* 多选工具条：与添加栏同一位置、同一外形，切换时不跳版 */
.selbar {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 0 22px 14px;
  padding: 7px 8px 7px 13px;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-xs);
}
.sel-count {
  font-size: var(--fs-xs);
  color: var(--text-dim);
}
.sel-count strong {
  color: var(--accent-ink);
  font-weight: 650;
  font-variant-numeric: tabular-nums;
}
.sel-count em {
  font-style: normal;
  font-size: var(--fs-2xs);
  color: var(--text-mute);
}
.sel-spacer {
  flex: 1;
}

/* ── 主区 ── */
.main {
  display: flex;
  flex-direction: column;
  min-width: 0;
  background: var(--bg);
  /* flex 子项默认 min-height:auto，会被内容撑破，导致内部滚动失效 */
  min-height: 0;
}

.topbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
  padding: 16px 22px 14px;
}
.title-row {
  display: flex;
  align-items: center;
  gap: 9px;
}
.top-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}
.topbar h1 {
  margin: 0;
  font-size: var(--fs-xl);
  font-weight: 650;
  letter-spacing: -0.01em;
}
.demo-badge {
  font-size: var(--fs-2xs);
  padding: 2px 8px;
  border-radius: 999px;
  background: rgba(179, 105, 10, 0.1);
  color: var(--warn);
  font-weight: 600;
}
.search {
  width: 240px;
  padding: 8px 13px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--surface-2);
  outline: none;
  transition: border-color 0.13s, background 0.13s, box-shadow 0.13s;
}
.search:focus {
  border-color: var(--accent);
  background: var(--surface);
  box-shadow: 0 0 0 3px var(--accent-dim);
}
.search::placeholder {
  color: var(--text-mute);
}

.list-wrap {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overflow-x: hidden;
  padding: 4px 22px 22px;
}

.err-banner {
  display: flex;
  align-items: center;
  gap: 9px;
  margin: 0 22px 12px;
  padding: 9px 12px;
  border-radius: var(--radius-sm);
  background: rgba(217, 45, 32, 0.05);
  border: 1px solid rgba(217, 45, 32, 0.22);
  font-size: var(--fs-xs);
  color: var(--text-dim);
}
.eb-icon {
  flex-shrink: 0;
  width: 17px;
  height: 17px;
  border-radius: 50%;
  background: var(--err);
  color: #fff;
  display: grid;
  place-items: center;
  font-weight: 700;
  font-size: var(--fs-2xs);
}
.eb-text {
  flex: 1;
  min-width: 0;
  overflow-wrap: anywhere;
}
.list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

/* 虚拟列表：外层撑出总高度以维持滚动条，项用 transform 定位。
   用 transform 而不是 top，避免每帧触发布局。 */
.list.vlist {
  position: relative;
  display: block;
}
.vitem {
  position: absolute;
  top: 0;
  left: 0;
  right: 0;
  /* 行间距做进项自身高度里，这样实测高度就已经包含了间隔 */
  padding-bottom: 8px;
  will-change: transform;
}

.empty-icon {
  width: 52px;
  height: 52px;
  border-radius: 16px;
  background: var(--surface-2);
  display: grid;
  place-items: center;
  font-size: 26px;
  opacity: 0.75;
}
.empty-sub {
  font-size: var(--fs-xs);
  opacity: 0.8;
}

/* ── 状态栏 ── */
.statusbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 22px;
  border-top: 1px solid var(--border-soft);
  background: var(--bg-elev);
  font-size: var(--fs-xs);
  color: var(--text-mute);
}
.sb-post {
  color: var(--warn);
  font-weight: 600;
}
.sb-queue {
  color: var(--accent);
  font-weight: 600;
}
.sb-host {
  color: var(--text-mute);
}
.sb-right {
  display: flex;
  gap: 7px;
}
.sep {
  opacity: 0.5;
}
.err {
  color: var(--err);
}
</style>
