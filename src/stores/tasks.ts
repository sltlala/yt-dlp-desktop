import { defineStore } from 'pinia'
import { api, type SchedulerStats } from '../ipc'
import { ACTIVE_STATES, type Settings, type Task } from '../types'

export type Filter = 'all' | 'active' | 'completed' | 'failed'

/** 事件通道失败时的降级轮询句柄。 */
let pollTimer: number | null = null

/**
 * 悬浮刷新按钮图标的最短转圈时长。
 *
 * 本地 IPC 刷新几毫秒就完成，若立刻复位 `refreshing`，旋转动画的类加了
 * 又删，浏览器一帧都来不及画——用户看到的是「没转」。这里保证至少转完一圈。
 */
const REFRESH_MIN_SPIN_MS = 900

/**
 * 转一圈的周期，必须与 App.vue 里 `fab-spin` 动画的 duration（`0.9s`）一致。
 *
 * 停圈时机要对齐到**整圈边界**（360° 与 0° 视觉上同一处），否则图标会
 * 「多转一点再复位」——上一版就是 0.8s/圈 撞上 900ms 时长 = 1.125 圈，
 * 多出的 0.125 圈（45°）在复位时产生可见跳变。
 */
const REFRESH_SPIN_PERIOD_MS = 900

export const useTaskStore = defineStore('tasks', {
  state: () => ({
    tasks: [] as Task[],
    settings: null as Settings | null,
    filter: 'all' as Filter,
    selected: new Set<string>(),
    expanded: null as string | null,
    /** 多选模式：避免误触批量删除。 */
    selectMode: false,
    ready: false,
    /**
     * 最近一次失败原因。
     *
     * **不能让错误静默**：事件订阅失败曾导致「添加任务毫无反应」，
     * 因为 Promise  rejection 没人接，界面上什么都看不出来。
     */
    lastError: null as string | null,
    /** 是否已降级为轮询（事件通道不可用时的兜底）。 */
    polling: false,
    /** 调度器队列快照（探测中 / 排队中）。 */
    stats: null as SchedulerStats | null,
    /**
     * 缩略图缓存版本号：每次手动刷新自增一次，喂给 `<img>` 的 cache-bust 查询串。
     *
     * WebView2 会缓存图片（`EBWebView\Default\Cache`）。修过缩略图 bug 之后，
     * 旧的空白/403 结果可能还在缓存里——光靠事件刷新 URL 不变，浏览器不会重下，
     * 必须换 URL 才能强制重取。
     */
    thumbVersion: 0,
    /** 手动刷新进行中——悬浮按钮据此转圈并防抖。 */
    refreshing: false,
  }),

  getters: {
    visible(state): Task[] {
      switch (state.filter) {
        case 'active':
          return state.tasks.filter((t) => ACTIVE_STATES.includes(t.state))
        case 'completed':
          return state.tasks.filter((t) => t.state === 'completed' || t.state === 'skipped')
        case 'failed':
          return state.tasks.filter((t) => t.state === 'failed')
        default:
          return state.tasks
      }
    },

    counts(state) {
      const c = { all: state.tasks.length, active: 0, completed: 0, failed: 0 }
      for (const t of state.tasks) {
        if (ACTIVE_STATES.includes(t.state)) c.active++
        if (t.state === 'completed' || t.state === 'skipped') c.completed++
        if (t.state === 'failed') c.failed++
      }
      return c
    },

    /**
     * 总速度只统计正在下载的任务。
     *
     * 注意：进度条「不确定态」的任务（total 未知）也要计入速度，
     * 否则用户会以为它卡死了。
     */
    totalSpeed(state): number {
      return state.tasks
        .filter((t) => t.state === 'downloading')
        .reduce((s, t) => s + (t.progress.speed ?? 0), 0)
    },

    activeCount(state): number {
      return state.tasks.filter((t) => ['downloading', 'postprocessing'].includes(t.state)).length
    },

    /** 后处理中的任务 —— 用于底部状态栏明确提示「正在合并」，避免被当成卡死。 */
    postProcessing(state): Task[] {
      return state.tasks.filter((t) => t.state === 'postprocessing')
    },
  },

  actions: {
    async init() {
      if (this.ready) return

      try {
        await api.onChange((tasks) => {
          this.tasks = tasks
        })
      } catch (e) {
        // 订阅失败必须显式暴露并降级，否则任务列表永远不刷新。
        console.error('[onChange] 事件订阅失败', e)
        this.lastError = `任务事件订阅失败，已降级为轮询：${String(e)}`
        this.startPolling()
      }

      try {
        this.settings = await api.getSettings()
      } catch (e) {
        console.error('[getSettings] 失败', e)
        this.lastError = `读取设置失败：${String(e)}`
      }

      this.ready = true
    },

    /** 事件通道不可用时的兜底：定时全量拉取。 */
    startPolling() {
      if (pollTimer !== null) return
      this.polling = true
      pollTimer = window.setInterval(async () => {
        try {
          const tasks = await api.listTasks()
          this.tasks = tasks
        } catch {
          /* 轮询失败静默重试，不刷屏 */
        }
      }, 1000)
    },

    async addUrl(url: string) {
      const u = url.trim()
      if (!u) return
      try {
        this.lastError = null
        const t = await api.addUrl(u)
        // 乐观更新：即便事件通道出问题，用户也能立刻看到任务出现。
        if (t && t.id && !this.tasks.some((x) => x.id === t.id)) {
          this.tasks = [t, ...this.tasks]
        }
      } catch (e) {
        console.error('[addUrl] 失败', e)
        this.lastError = `添加任务失败：${String(e)}`
      }
    },

    toggleSelect(id: string) {
      const s = new Set(this.selected)
      s.has(id) ? s.delete(id) : s.add(id)
      this.selected = s
    },

    /** 全选**当前可见**的项。筛选后「全选」应当只覆盖看得见的那批。 */
    selectAll(ids: string[]) {
      this.selected = new Set(ids)
    },

    /** 反选：同样只在可见范围内翻转，不会动到被筛掉的选中项。 */
    invertSelection(ids: string[]) {
      const next = new Set<string>()
      for (const id of ids) {
        if (!this.selected.has(id)) next.add(id)
      }
      this.selected = next
    },

    clearSelected() {
      this.selected = new Set()
    },

    clearSelection() {
      this.selected = new Set()
      this.selectMode = false
    },

    toggleExpand(id: string) {
      this.expanded = this.expanded === id ? null : id
    },

    /**
     * 删除动作刻意拆成三个独立方法。
     *
     * 「移除记录」「从归档移除」「删除文件」是三种不同意图，合成一个按钮
     * 会导致：删了记录但没删归档 → 重新添加同一链接被静默跳过，
     * 用户会以为程序坏了（DESIGN §13）。
     */
    async removeRecord(id: string) {
      await api.removeRecord(id)
    },

    /**
     * 设定任务的 `-f` 表达式并重新下载。
     *
     * 后端会立刻按新格式重跑，所以这里**不**做乐观更新——
     * 状态由后端推进（pending → probing → downloading），跟着事件走即可。
     */
    async setFormat(id: string, expression: string) {
      await api.setFormat(id, expression)
    },
    async removeMany() {
      await api.removeMany([...this.selected])
      this.clearSelection()
    },
    async removeFromArchive(ids: string[]) {
      await api.removeFromArchive(ids)
    },
    async deleteFile(id: string) {
      await api.deleteFile(id)
    },

    /**
     * 用系统默认程序打开成品文件。
     *
     * 失败（文件被移走/删掉）必须**显式报出来**——双击没反应是最难排查的失败方式。
     */
    async openFile(path: string) {
      try {
        this.lastError = null
        await api.openFile(path)
      } catch (e) {
        this.lastError = `打开文件失败：${String(e)}`
      }
    },

    async revealFile(path: string) {
      try {
        this.lastError = null
        await api.revealFile(path)
      } catch (e) {
        this.lastError = `定位文件失败：${String(e)}`
      }
    },

    async pause(id: string) {
      await api.pause(id)
    },
    async resume(id: string) {
      await api.resume(id)
    },
    async retry(id: string) {
      await api.retry(id)
    },

    /** 播放列表勾选后开始下载。 */
    async startPlaylist(id: string, indices: number[]) {
      try {
        this.lastError = null
        await api.startPlaylist(id, indices)
      } catch (e) {
        console.error('[startPlaylist] 失败', e)
        this.lastError = `开始下载失败：${String(e)}`
      }
    },

    /** 调度器队列快照，用于状态栏。 */
    async refreshStats() {
      try {
        this.stats = await api.schedulerStats()
      } catch {
        /* 状态栏是辅助信息，失败不打扰用户 */
      }
    },

    /**
     * 手动刷新：重新拉取任务全量快照（详情、进度、缩略图 URL），并推进
     * 缩略图缓存版本，强制浏览器重下可能已过期的图片。
     *
     * 与 `init` 不同——`init` 只在事件通道失效时降级轮询；这里是用户主动
     * 「我要现在看到最新状态」，所以无条件全量拉一次，失败要显式报出来。
     */
    async refresh() {
      if (this.refreshing) return
      this.refreshing = true
      this.lastError = null
      const started = Date.now()
      try {
        const [tasks, settings] = await Promise.all([api.listTasks(), api.getSettings()])
        this.tasks = tasks
        this.settings = settings
        // 推进版本号，让 <img> 的 cache-bust 查询串变化 → 浏览器重取缩略图
        this.thumbVersion++
        await this.refreshStats()
      } catch (e) {
        console.error('[refresh] 失败', e)
        this.lastError = `刷新失败：${String(e)}`
      }
      // 本地 IPC 只要几毫秒，若立刻复位 refreshing，`.spinning` 类加了又被删，
      // 浏览器根本没机会画一帧 → 用户只看到「没转圈」。这里兜底至少转一圈，
      // 并把时长对齐到整圈边界，让图标停在 360°（= 0°，不可见跳变）。
      const elapsed = Date.now() - started
      const remain = REFRESH_MIN_SPIN_MS - elapsed
      if (remain > 0) {
        await new Promise((r) => setTimeout(r, remain))
      }
      // 万一真实刷新慢到跨过整圈，也补到下一个整圈边界，避免「多转一点再复位」
      const done = Date.now() - started
      const over = done % REFRESH_SPIN_PERIOD_MS
      if (over > 0) {
        await new Promise((r) => setTimeout(r, REFRESH_SPIN_PERIOD_MS - over))
      }
      this.refreshing = false
    },

    async saveSettings(s: Settings) {
      await api.saveSettings(s)
      this.settings = s
    },
  },
})
