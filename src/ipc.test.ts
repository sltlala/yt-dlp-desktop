import { afterEach, describe, expect, it, vi } from 'vitest'
import type { ProgressPatch } from './ipc'
import type { Task } from './types'

afterEach(() => {
  vi.useRealTimers()
  vi.resetModules()
})

describe('演示后端', () => {
  it('进度走 progress 通道，不再每次推全量快照', async () => {
    vi.useFakeTimers()
    const { api } = await import('./ipc')
    const snapshots: Task[][] = []
    const progress: ProgressPatch[] = []
    await api.onChange((t) => snapshots.push(t))
    await api.onProgress((p) => progress.push(p))
    await api.onPatch(() => {})

    vi.advanceTimersByTime(2000)

    // 只有订阅时的首帧
    expect(snapshots).toHaveLength(1)
    expect(progress.length).toBeGreaterThan(0)
    const downloading = snapshots[0].filter((t) => t.state === 'downloading').map((t) => t.id)
    expect(downloading).toContain(progress[0].id)
  })

  it('总大小未知的任务只推进已下载字节，不编造总大小', async () => {
    vi.useFakeTimers()
    const { api } = await import('./ipc')
    const progress: ProgressPatch[] = []
    let first: Task[] = []
    await api.onChange((t) => (first = t))
    await api.onProgress((p) => progress.push(p))

    vi.advanceTimersByTime(1000)

    const unknown = first.filter((t) => t.state === 'downloading' && t.progress.total === null)
    for (const t of unknown) {
      const mine = progress.filter((p) => p.id === t.id)
      expect(mine.length).toBeGreaterThan(0)
      expect(mine.every((p) => p.progress.total === null)).toBe(true)
    }
  })
})
