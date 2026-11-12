import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import type { ProgressPatch } from '../ipc'
import type { Task } from '../types'
import { demoTasks } from '../mock'

/** 截住 store 订阅的三个事件通道，由测试手动推送。 */
const channels = vi.hoisted(() => ({
  change: null as ((tasks: Task[]) => void) | null,
  patch: null as ((t: Task) => void) | null,
  progress: null as ((p: ProgressPatch) => void) | null,
  failSubscribe: false,
}))

vi.mock('../ipc', () => ({
  api: {
    onChange: vi.fn(async (l: (tasks: Task[]) => void) => {
      if (channels.failSubscribe) throw new Error('missing core:event:allow-listen')
      channels.change = l
      return () => {}
    }),
    onPatch: vi.fn(async (l: (t: Task) => void) => {
      channels.patch = l
      return () => {}
    }),
    onProgress: vi.fn(async (l: (p: ProgressPatch) => void) => {
      channels.progress = l
      return () => {}
    }),
    getSettings: vi.fn(async () => ({ outputDir: 'D:\\v' })),
    listTasks: vi.fn(async () => []),
  },
}))

const { useTaskStore } = await import('./tasks')

function task(id: string, patch: Partial<Task> = {}): Task {
  return { ...structuredClone(demoTasks[0]), id, ...patch }
}

beforeEach(() => {
  setActivePinia(createPinia())
  channels.change = channels.patch = channels.progress = null
  channels.failSubscribe = false
})

describe('任务事件合并', () => {
  it('全量快照整体替换列表', async () => {
    const store = useTaskStore()
    await store.init()
    channels.change!([task('a'), task('b')])
    expect(store.tasks.map((t) => t.id)).toEqual(['a', 'b'])
  })

  it('单任务增量只替换对应的那一条', async () => {
    const store = useTaskStore()
    await store.init()
    channels.change!([task('a', { state: 'downloading' }), task('b')])
    const untouched = store.tasks[1]

    channels.patch!(task('a', { state: 'completed' }))

    expect(store.tasks[0].state).toBe('completed')
    expect(store.tasks[1]).toBe(untouched)
  })

  it('找不到 id 的增量被丢弃，不会复活已删除的任务', async () => {
    const store = useTaskStore()
    await store.init()
    channels.change!([task('a')])
    channels.patch!(task('gone'))
    channels.progress!({ id: 'gone', progress: { downloaded: 1, total: 2, speed: 1, eta: 1 }, usedAria2c: false })
    expect(store.tasks.map((t) => t.id)).toEqual(['a'])
  })

  it('进度增量只改进度字段', async () => {
    const store = useTaskStore()
    await store.init()
    channels.change!([task('a', { state: 'downloading', title: '原标题' })])

    channels.progress!({
      id: 'a',
      progress: { downloaded: 50, total: 100, speed: 10, eta: 5 },
      usedAria2c: true,
    })

    const t = store.tasks[0]
    expect(t.progress).toEqual({ downloaded: 50, total: 100, speed: 10, eta: 5 })
    expect(t.usedAria2c).toBe(true)
    expect(t.title).toBe('原标题')
    expect(t.state).toBe('downloading')
  })

  it('总速度只统计下载中的任务', async () => {
    const store = useTaskStore()
    await store.init()
    channels.change!([
      task('a', { state: 'downloading', progress: { downloaded: 0, total: null, speed: 100, eta: null } }),
      task('b', { state: 'paused', progress: { downloaded: 0, total: null, speed: 999, eta: null } }),
    ])
    expect(store.totalSpeed).toBe(100)
  })

  it('事件订阅失败时显式报错并降级为轮询', async () => {
    channels.failSubscribe = true
    const store = useTaskStore()
    await store.init()
    expect(store.polling).toBe(true)
    expect(store.lastError).toContain('订阅失败')
  })
})
