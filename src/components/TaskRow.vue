<script setup lang="ts">
import { computed } from 'vue'
import type { Task } from '../types'
import { POST_PROCESS_LABEL, TASK_STATE_LABEL } from '../types'
import { useTaskStore } from '../stores/tasks'
import { fmtBytes, fmtDuration, fmtEta, fmtSpeed, shortenPath } from '../utils'
import TaskDetail from './TaskDetail.vue'

const props = defineProps<{ task: Task }>()
const store = useTaskStore()

const t = computed(() => props.task)
const expanded = computed(() => store.expanded === t.value.id)
const selected = computed(() => store.selected.has(t.value.id))

/**
 * 进度比例。**总大小未知时返回 null** —— 界面必须切「不确定态」，
 * 而不是显示 0%（HANDOFF §3.2）。
 */
const fraction = computed(() => {
  const { downloaded, total } = t.value.progress
  if (total === null || total === 0 || downloaded === null) return null
  return Math.max(0, Math.min(1, downloaded / total))
})

const indeterminate = computed(
  () => t.value.state === 'downloading' && fraction.value === null,
)

/** 后处理阶段：进度已满但仍在工作，必须给出明确文案（DESIGN §5.4）。 */
const postLabel = computed(() =>
  t.value.postProcess ? POST_PROCESS_LABEL[t.value.postProcess] : TASK_STATE_LABEL.postprocessing,
)

const stateLabel = computed(() => TASK_STATE_LABEL[t.value.state])

/** `skipped` 绝不能显示成「已完成」（DESIGN §13.1）。 */
const chipClass = computed(() => t.value.state)

const canPause = computed(() =>
  ['downloading', 'postprocessing', 'pending', 'queued', 'probing'].includes(t.value.state),
)
const canResume = computed(() => ['paused', 'failed'].includes(t.value.state))

/**
 * 多选模式下点行体 = 勾选，而不是展开详情。
 *
 * 否则用户要「在列表里选」时得先精确点到左边那个小方框，
 * 点标题只会把详情展开——那就不是「直接在列表上选」了。
 */
function onRowClick(e: MouseEvent) {
  if (store.selectMode) {
    store.toggleSelect(t.value.id)
    return
  }
  /*
   * 单击展开 / 双击打开文件，这两件事天然冲突：双击会先触发两次 click。
   *
   * 用 `event.detail` 区分——它就是浏览器自己的连击计数（且遵循系统的
   * 双击间隔设置），比另写一个 setTimeout 阈值可靠：
   *   detail === 1 → 是单击，切换展开
   *   detail >= 2 → 是双击的第二下，**吞掉**，交给 dblclick 去开文件
   *
   * 这样单击是**零延迟**的（不用「等 200ms 看有没有第二下」那套，
   * 那会给最主要的交互平白加上延迟），双击也不会出现「展开又收起」的闪烁。
   */
  if (e.detail > 1) return
  store.toggleExpand(t.value.id)
}

/** 双击行体 = 用系统默认程序打开成品文件。 */
function onRowDblClick() {
  if (store.selectMode) return
  const p = t.value.filepath
  // 没有成品文件（没下完 / 记录已被删）时什么都不做，不要弹一个无意义的错误
  if (!p) return
  void store.openFile(p)
}

/** 有成品文件才提示「双击打开」，否则会教用户做一件做不到的事。 */
const canOpen = computed(() => !!t.value.filepath)
</script>

<template>
  <div
    class="row"
    :class="{ expanded, selected, failed: t.state === 'failed', selectable: store.selectMode }"
  >
    <!-- 双击监听放在这一行上，而不是整个 .row：
         否则双击展开区里的内容（比如路径）也会触发「打开文件」 -->
    <div
      class="main-line"
      :class="{ openable: canOpen && !store.selectMode }"
      :title="canOpen && !store.selectMode ? '单击展开详情 · 双击打开文件' : undefined"
      @dblclick="onRowDblClick"
    >
      <!-- 批量选择 -->
      <input
        v-if="store.selectMode"
        class="cb"
        type="checkbox"
        :checked="selected"
        @change="store.toggleSelect(t.id)"
      />

      <!-- 缩略图 -->
      <div class="thumb" @click="onRowClick">
        <img v-if="t.thumbnail" :src="t.thumbnail" alt="" />
        <div v-else class="thumb-ph">🎬</div>
        <span v-if="t.durationSec" class="dur">{{ fmtDuration(t.durationSec) }}</span>
      </div>

      <!-- 主信息 -->
      <div class="info" @click="onRowClick">
        <div class="line1">
          <!-- 探测失败时 title 为空，回落到 URL，否则整行看起来是坏的 -->
          <span class="title" :title="t.title || t.url">{{ t.title || t.url }}</span>
          <span v-if="t.extractor" class="src" :class="t.extractor.toLowerCase()">
            {{ t.extractor }}
          </span>
        </div>

        <!-- 进度条 -->
        <div class="bar" :class="{ ind: indeterminate }">
          <div
            v-if="!indeterminate && fraction !== null"
            class="fill"
            :class="{
              completed: t.state === 'completed',
              post: t.state === 'postprocessing',
            }"
            :style="{ width: (fraction * 100).toFixed(1) + '%' }"
          />
          <div v-else-if="indeterminate" class="stripes" />
        </div>

        <div class="line3">
          <!-- 后处理优先显示，避免用户以为卡死 -->
          <template v-if="t.state === 'postprocessing'">
            <span class="post-text">{{ postLabel }}…</span>
          </template>
          <template v-else-if="t.state === 'skipped'">
            <span class="skip-text">
              {{ t.skipReason === 'archive' ? '已在下载归档中，已跳过' : '文件已存在，已跳过' }}
            </span>
          </template>
          <template v-else-if="t.state === 'failed'">
            <span class="err-text" :title="t.error ?? ''">{{ t.error }}</span>
          </template>
          <template v-else-if="t.state === 'completed'">
            <span class="muted">{{ shortenPath(t.filepath ?? '', 1) }}</span>
          </template>
          <template v-else-if="t.state === 'paused'">
            <span class="muted"
              >已暂停 · {{ fmtBytes(t.progress.downloaded) }} / {{ fmtBytes(t.progress.total) }}</span
            >
          </template>
          <!-- 播放列表探测完成，等用户勾选（DESIGN §9） -->
          <template v-else-if="t.state === 'selecting'">
            <span class="select-text">
              探测到 {{ t.playlistEntries?.length ?? 0 }} 集，展开后可选择要下载哪几集
            </span>
          </template>
          <!-- 排队/探测中：显示调度器给的提示，避免看起来像卡住 -->
          <template v-else-if="t.state === 'probing' || t.state === 'queued' || t.state === 'pending'">
            <span class="muted">{{ t.queueHint ?? TASK_STATE_LABEL[t.state] }}</span>
          </template>
          <template v-else>
            <span class="muted">
              {{ fmtBytes(t.progress.downloaded) }} / {{ fmtBytes(t.progress.total) }}
            </span>
            <span class="muted" v-if="fraction !== null">{{ (fraction * 100).toFixed(1) }}%</span>
            <span class="speed" v-if="t.progress.speed">{{ fmtSpeed(t.progress.speed) }}</span>
            <span class="muted" v-if="t.progress.eta !== null">{{ fmtEta(t.progress.eta) }}</span>
          </template>
        </div>
      </div>

      <!-- 右侧状态与操作 -->
      <div class="right">
        <span class="chip" :class="chipClass">
          <span class="dot" />
          {{ stateLabel }}
        </span>

        <div class="actions" @click.stop>
          <!-- 多选时藏掉行内操作：此刻点行体是「勾选」，留着 ▼ 会让人以为还能展开 -->
          <template v-if="!store.selectMode">
            <button v-if="canPause" class="btn ghost sm" title="暂停" @click="store.pause(t.id)">
              ⏸
            </button>
            <button v-if="canResume" class="btn ghost sm" title="继续" @click="store.resume(t.id)">
              ▶
            </button>
            <button
              class="btn ghost sm"
              :title="expanded ? '收起' : '展开详情'"
              @click="store.toggleExpand(t.id)"
            >
              {{ expanded ? '▲' : '▼' }}
            </button>
          </template>
        </div>
      </div>
    </div>

    <TaskDetail v-if="expanded" :task="t" />
  </div>
</template>

<style scoped>
.row {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-xs);
  /* 让展开区的浅色底被卡片圆角裁掉，不必自己再算一次圆角 */
  overflow: hidden;
  transition: border-color 0.14s, box-shadow 0.14s, background 0.14s;
}
.row:hover {
  border-color: #d7dce3;
  box-shadow: var(--shadow-sm);
}
.row.expanded {
  border-color: #d2d8e0;
  box-shadow: var(--shadow-sm);
}
.row.selected {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-dim);
}
.row.failed {
  border-color: rgba(217, 45, 32, 0.28);
}

/* 多选模式：整行就是勾选热区，悬停给底色让人知道这里能点 */
.row.selectable {
  cursor: pointer;
}
.row.selectable:hover {
  background: var(--surface-2);
}

.main-line {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  min-height: var(--row-h);
}
/* 有成品文件时才提示可以双击打开 */
.main-line.openable {
  cursor: default;
}

.cb {
  width: 15px;
  height: 15px;
  accent-color: var(--accent);
  flex-shrink: 0;
}

.thumb {
  position: relative;
  width: 96px;
  height: 54px;
  flex-shrink: 0;
  border-radius: var(--radius-sm);
  overflow: hidden;
  background: var(--surface-2);
  border: 1px solid var(--border-soft);
  cursor: pointer;
}
.thumb img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
.thumb-ph {
  display: grid;
  place-items: center;
  height: 100%;
  /* 图标字号不进制：emoji 没有笔画粗细，半像素不影响清晰度 */
  font-size: 22px;
  opacity: 0.45;
}
.dur {
  position: absolute;
  right: 4px;
  bottom: 4px;
  padding: 1px 5px;
  border-radius: 4px;
  background: rgba(16, 24, 40, 0.78);
  font-size: var(--fs-2xs);
  font-variant-numeric: tabular-nums;
  color: #fff;
  backdrop-filter: blur(2px);
}

.info {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
  cursor: pointer;
}

.line1 {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}
.title {
  font-size: var(--fs-md);
  font-weight: 550;
  letter-spacing: -0.005em;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.src {
  flex-shrink: 0;
  font-size: var(--fs-2xs);
  padding: 1px 7px;
  border-radius: 999px;
  background: var(--surface-2);
  color: var(--text-mute);
  font-weight: 600;
}
/* 站点标识色在亮底上要压暗，否则一律发飘 */
.src.bilibili {
  background: rgba(251, 114, 153, 0.12);
  color: #d94f7c;
}
.src.youtube {
  background: rgba(217, 45, 32, 0.09);
  color: #cc2b1f;
}
.src.vimeo {
  background: rgba(26, 183, 234, 0.14);
  color: #0b7fa6;
}

/* ── 进度条 ── */
.bar {
  position: relative;
  height: 4px;
  border-radius: 999px;
  background: var(--surface-3);
  overflow: hidden;
}
.fill {
  height: 100%;
  border-radius: 999px;
  background: linear-gradient(90deg, #5b8cf8, var(--accent));
  transition: width 0.45s ease;
}
/* 已完成用绿色；琥珀色**只**留给后处理，否则完成态会被误读成警告 */
.fill.completed {
  background: linear-gradient(90deg, #23a55a, var(--ok));
}
.fill.post {
  background: linear-gradient(90deg, #e0a03a, var(--warn));
}
/* 不确定态：滚动条纹，而不是假装 0% */
.bar.ind {
  background: repeating-linear-gradient(115deg, #eaecf0 0 10px, #d8dde4 10px 20px);
  background-size: 200% 100%;
  animation: slide 1.1s linear infinite;
}
@keyframes slide {
  from {
    background-position: 0 0;
  }
  to {
    background-position: -40px 0;
  }
}

.line3 {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: var(--fs-xs);
  min-height: 16px;
  overflow: hidden;
}
.muted {
  color: var(--text-mute);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.speed {
  color: var(--accent);
  font-variant-numeric: tabular-nums;
  font-weight: 600;
}
.post-text {
  color: var(--warn);
  font-weight: 600;
}
.post-text::after {
  content: '';
  display: inline-block;
  width: 5px;
  animation: blink 1.1s steps(2, start) infinite;
}
@keyframes blink {
  to {
    visibility: hidden;
  }
}
.skip-text {
  color: var(--skip);
}
.select-text {
  color: var(--accent-ink);
  font-weight: 600;
}
.err-text {
  color: var(--err);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* ── 右侧 ── */
.right {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 8px;
  flex-shrink: 0;
}
.actions {
  display: flex;
  gap: 1px;
  opacity: 0;
  transition: opacity 0.14s;
}
.row:hover .actions,
.row.expanded .actions {
  opacity: 1;
}
.actions .btn {
  padding: 3px 7px;
  font-size: var(--fs-xs);
}
</style>
