<script setup lang="ts">
import { computed, ref } from 'vue'
import type { Task } from '../types'
import { useTaskStore } from '../stores/tasks'
import { ellipsizePath, fmtBytes, fmtTime, shortenPath } from '../utils'
import FormatPicker from './FormatPicker.vue'
import PlaylistPicker from './PlaylistPicker.vue'

const props = defineProps<{ task: Task }>()
const store = useTaskStore()
// 开发期快照辅助，与 App.vue 的 ?panel= 约定一致。
const showFormats = ref(
  new URLSearchParams(window.location.search).get('panel') === 'formats',
)
const showPlaylist = ref(
  new URLSearchParams(window.location.search).get('panel') === 'playlist',
)

const t = computed(() => props.task)

/**
 * 应用选定的 `-f` 表达式。
 *
 * 后端会立刻按新格式重下（`set_task_format`），所以这里只负责关窗——
 * 之后的状态变化走事件通道，不做乐观更新，免得和后端打架。
 */
async function applyFormat(expression: string) {
  showFormats.value = false
  await store.setFormat(t.value.id, expression)
}

/**
 * 路径行要显示**尾部**。
 *
 * CSS 的 `text-overflow: ellipsis` 砍的是结尾，长文件名下正好把扩展名和
 * 关键的辨识部分砍掉（`E:\下载\视频\(4K) 🖤 검스 VS 살스…` 看不出是什么文件）。
 * 所以显示值走 `shortenPath()`，原始值仍放在 `title` 与 `v` 里。
 */
const rows = computed(() => {
  const out = t.value.outputDir.replace(/[\\/]+$/, '')
  const file = t.value.filepath ?? ''
  return [
    { k: '链接', v: t.value.url, mono: true },
    {
      k: '格式表达式',
      v: t.value.formatExpression,
      mono: true,
      hint: t.value.formatOverride
        ? '手动指定 · 可在「选择格式」里恢复默认'
        : '跟随设置里的预设',
    },
    { k: '容器', v: t.value.container.toUpperCase(), mono: true },
    { k: '输出目录', v: t.value.outputDir, display: shortenPath(out, 2), mono: true },
    {
      k: '最终文件',
      v: file || '—',
      display: file ? ellipsizePath(file, 84) : '—',
      hint: file ? '双击行可打开' : undefined,
      mono: true,
    },
    {
      k: '预估大小',
      v: t.value.sizeEstimate ? fmtBytes(t.value.sizeEstimate) : '—',
      hint: '估算值；合并、嵌入后成品通常更大',
    },
    {
      k: '实际大小',
      v: t.value.sizeActual ? fmtBytes(t.value.sizeActual) : '—',
      // 有值时不再补一句「成品文件的实际大小」——标题已经说了，纯重复
      hint: t.value.sizeActual ? undefined : '下载完成后才有',
    },
    {
      k: '进度来源',
      v: t.value.usedAria2c ? 'aria2c 自身输出（精度略低）' : 'yt-dlp',
      hint: t.value.usedAria2c ? '用 aria2c 时 yt-dlp 不上报进度' : undefined,
    },
    { k: '添加时间', v: fmtTime(t.value.addedAt) },
    { k: '完成时间', v: fmtTime(t.value.finishedAt) },
  ]
})
</script>

<template>
  <div class="detail">
    <!-- 嵌入告警：--embed-subs 失败是静默的，必须显式呈现（DESIGN §14.3） -->
    <div v-if="t.warnings.length" class="warn-box">
      <div class="warn-title">后处理告警</div>
      <ul>
        <li v-for="(w, i) in t.warnings" :key="i">{{ w }}</li>
      </ul>
    </div>

    <div v-if="t.error" class="err-box">
      <div class="err-title">失败原因</div>
      <div>{{ t.error }}</div>
    </div>

    <!-- 播放列表：探测完成，等用户勾选（DESIGN §9） -->
    <div v-if="t.state === 'selecting'" class="playlist-box">
      <div class="pl-head">
        这是一个播放列表，共 <strong>{{ t.playlistEntries?.length ?? 0 }}</strong> 集
      </div>
      <ol class="pl-preview">
        <li v-for="(e, i) in (t.playlistEntries ?? []).slice(0, 4)" :key="i">
          {{ e.title }}
        </li>
      </ol>
      <div v-if="(t.playlistEntries?.length ?? 0) > 4" class="pl-more">
        …还有 {{ (t.playlistEntries?.length ?? 0) - 4 }} 集
      </div>
      <button class="btn primary sm" @click="showPlaylist = true">选择要下载的集数</button>
    </div>

    <dl class="kv">
      <template v-for="r in rows" :key="r.k">
        <dt>{{ r.k }}</dt>
        <dd :class="{ mono: r.mono }" :title="r.v">
          <span>{{ r.display ?? r.v }}</span>
          <span v-if="r.hint" class="hint">{{ r.hint }}</span>
        </dd>
      </template>
    </dl>

    <!-- ───────── 操作区 ───────── -->
    <div class="ops">
      <div class="op-group">
        <button
          class="btn sm"
          :disabled="!t.filepath"
          title="用系统默认程序打开成品文件（也可以直接双击任务行）"
          @click="t.filepath && store.openFile(t.filepath)"
        >
          打开文件
        </button>
        <button
          class="btn sm"
          :disabled="!t.filepath"
          title="在资源管理器中选中该文件"
          @click="t.filepath && store.revealFile(t.filepath)"
        >
          打开所在文件夹
        </button>
        <button class="btn sm" @click="showFormats = true">选择格式</button>
        <button class="btn sm" @click="store.retry(t.id)">重新下载</button>
      </div>

      <!-- 三个删除动作刻意分开，语义互不相同（DESIGN §13） -->
      <div class="danger-zone">
        <div class="dz-label">删除选项</div>
        <div class="dz-items">
          <button
            class="btn sm"
            title="从列表移除该记录，并清理临时文件与 .part 碎片；不删除已下载的成品文件"
            @click="store.removeRecord(t.id)"
          >
            移除记录
          </button>
          <button
            class="btn sm"
            title="从 download-archive 移除，使该视频可以被重新下载"
            @click="store.removeFromArchive([t.id])"
          >
            从归档移除
          </button>
          <button
            class="btn sm danger"
            :disabled="!t.filepath"
            title="删除磁盘上的成品文件。此操作不可撤销"
            @click="store.deleteFile(t.id)"
          >
            删除文件
          </button>
        </div>
        <p class="dz-note">
          仅「移除记录」不足以重新下载：成品文件仍在磁盘时，yt-dlp 会直接跳过并返回成功。
        </p>
      </div>
    </div>

    <FormatPicker
      v-if="showFormats"
      :expression="t.formatExpression"
      :url="t.url"
      :formats="t.formats ?? []"
      :overridden="!!t.formatOverride"
      @close="showFormats = false"
      @apply="applyFormat"
    />

    <PlaylistPicker
      v-if="showPlaylist && t.playlistEntries?.length"
      :task-id="t.id"
      :entries="t.playlistEntries"
      @close="showPlaylist = false"
    />
  </div>
</template>

<style scoped>
/* 展开区是「卡片内部的嵌套面板」：整条铺满卡片宽度（由 .row 的
 * overflow:hidden 裁掉圆角），但内容仍对齐到缩略图右侧那一列。 */
.detail {
  border-top: 1px solid var(--border-soft);
  background: var(--bg-elev);
  padding: 13px 14px 15px 122px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.warn-box {
  background: rgba(179, 105, 10, 0.055);
  border: 1px solid rgba(179, 105, 10, 0.2);
  border-radius: var(--radius-sm);
  padding: 9px 12px;
  font-size: var(--fs-xs);
}
.warn-title {
  color: var(--warn);
  font-weight: 650;
  margin-bottom: 4px;
}
.warn-box ul {
  margin: 0;
  padding-left: 16px;
  color: var(--text-dim);
}

.err-box {
  background: rgba(217, 45, 32, 0.05);
  border: 1px solid rgba(217, 45, 32, 0.2);
  border-radius: var(--radius-sm);
  padding: 9px 12px;
  font-size: var(--fs-xs);
}
.err-title {
  color: var(--err);
  font-weight: 650;
  margin-bottom: 4px;
}

.playlist-box {
  background: rgba(109, 63, 224, 0.05);
  border: 1px solid rgba(109, 63, 224, 0.2);
  border-radius: var(--radius-sm);
  padding: 11px 13px;
  font-size: var(--fs-xs);
  display: flex;
  flex-direction: column;
  gap: 7px;
  align-items: flex-start;
}
.pl-head strong {
  color: #6d3fe0;
}
.pl-preview {
  margin: 0;
  padding-left: 18px;
  color: var(--text-dim);
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.pl-preview li {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 100%;
}
.pl-more {
  color: var(--text-mute);
  font-size: var(--fs-2xs);
}

.kv {
  display: grid;
  /* 标签列按 --fs-md 下最长的标签（「格式表达式」）留量：
     92px 是按原来的 13.5px 正文算的，字号涨了会挤到值那一列 */
  grid-template-columns: 104px 1fr;
  gap: 6px 12px;
  margin: 0;
  font-size: var(--fs-xs);
}
.kv dt {
  color: var(--text-mute);
}
.kv dd {
  margin: 0;
  color: var(--text-dim);
  display: flex;
  align-items: baseline;
  gap: 8px;
  min-width: 0;
}
.kv dd > span:first-child {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.hint {
  flex-shrink: 0;
  font-size: var(--fs-2xs);
  color: var(--text-mute);
  opacity: 0.8;
}

.ops {
  display: flex;
  flex-direction: column;
  gap: 11px;
  padding-top: 4px;
  border-top: 1px dashed var(--border-soft);
}
.op-group {
  display: flex;
  gap: 7px;
}

.danger-zone {
  background: rgba(217, 45, 32, 0.028);
  border: 1px solid rgba(217, 45, 32, 0.14);
  border-radius: var(--radius-sm);
  padding: 10px 12px;
}
.dz-label {
  font-size: var(--fs-2xs);
  color: var(--text-mute);
  margin-bottom: 7px;
  font-weight: 600;
}
.dz-items {
  display: flex;
  gap: 7px;
}
.dz-note {
  margin: 8px 0 0;
  font-size: var(--fs-2xs);
  color: var(--text-mute);
  line-height: 1.45;
}
</style>
