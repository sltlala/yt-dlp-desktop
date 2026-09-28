/**
 * 变高虚拟滚动。
 *
 * 为什么不能用固定行高的实现：任务行在**展开详情**后高度会变化很多
 * （详情里有格式表、告警、删除区），而同一时刻只允许一行展开。
 * 固定行高的做法会导致展开后整列错位。
 *
 * 策略：先用估算高度排布，渲染后**实测**每个可见项的真实高度并更新偏移表。
 * 高度一旦测到就缓存下来（按 key），滚动时不会反复跳动。
 */

import { computed, nextTick, onBeforeUnmount, onMounted, ref, type Ref } from 'vue'

export interface VirtualItem {
  key: string
  index: number
  /** 该项距列表顶部的像素偏移。 */
  offset: number
}

export function useVirtualList(
  /** 滚动容器。 */
  container: Ref<HTMLElement | null>,
  /** 当前列表的 key 序列（顺序即展示顺序）。 */
  keys: Ref<string[]>,
  /** 未测量前的估算行高。 */
  estimate = 76,
  /** 视口外多渲染几项，减少快速滚动时的白屏。 */
  overscan = 5,
) {
  const scrollTop = ref(0)
  const viewport = ref(0)
  /** 已实测的高度，按 key 缓存。 */
  const heights = ref<Record<string, number>>({})
  /** 高度表变更计数，用于触发重算。 */
  const revision = ref(0)

  const heightOf = (key: string) => heights.value[key] ?? estimate

  /** 前缀和：offsets[i] 是第 i 项的顶部偏移。 */
  const offsets = computed(() => {
    void revision.value
    const out: number[] = new Array(keys.value.length)
    let acc = 0
    for (let i = 0; i < keys.value.length; i++) {
      out[i] = acc
      acc += heightOf(keys.value[i])
    }
    return out
  })

  const totalHeight = computed(() => {
    const o = offsets.value
    if (o.length === 0) return 0
    const last = keys.value.length - 1
    return o[last] + heightOf(keys.value[last])
  })

  /**
   * 二分找第一个「底部超过 scrollTop」的项。
   *
   * 用二分而不是线性扫描：任务上千时线性扫描每次滚动都是 O(n)。
   */
  function firstVisible(offset: number): number {
    const o = offsets.value
    let lo = 0
    let hi = o.length
    while (lo < hi) {
      const mid = (lo + hi) >> 1
      if (o[mid] + heightOf(keys.value[mid]) < offset) lo = mid + 1
      else hi = mid
    }
    return lo
  }

  /** 当前该渲染的区间。 */
  const range = computed(() => {
    void revision.value
    const n = keys.value.length
    if (n === 0) return { start: 0, end: 0 }
    const top = scrollTop.value
    const bottom = top + (viewport.value || 600)
    const start = Math.max(0, firstVisible(top - overscan * estimate) - overscan)
    let end = start
    // 从 start 往后走到超出视口底部为止
    while (end < n && offsets.value[end] < bottom + overscan * estimate) end++
    return { start, end: Math.min(n, Math.max(end, start + 1)) }
  })

  const items = computed<VirtualItem[]>(() => {
    const { start, end } = range.value
    const out: VirtualItem[] = []
    for (let i = start; i < end; i++) {
      out.push({ key: keys.value[i], index: i, offset: offsets.value[i] })
    }
    return out
  })

  /** 实测已渲染项的高度，有变化就更新偏移表。 */
  async function measure() {
    await nextTick()
    const el = container.value
    if (!el) return
    const nodes = el.querySelectorAll<HTMLElement>('[data-vkey]')
    let changed = false
    const next = { ...heights.value }
    nodes.forEach((node) => {
      const key = node.dataset.vkey
      if (!key) return
      const h = node.offsetHeight
      if (h > 0 && next[key] !== h) {
        next[key] = h
        changed = true
      }
    })
    if (changed) {
      heights.value = next
      revision.value++
    }
  }

  function readScroll() {
    const el = container.value
    if (!el) return
    scrollTop.value = el.scrollTop
    viewport.value = el.clientHeight
  }

  // 滚动用 rAF 节流：直接绑 scroll 会在高速滚动时疯狂触发 measure
  let raf = 0
  function onScroll() {
    if (raf) return
    raf = requestAnimationFrame(() => {
      raf = 0
      readScroll()
      void measure()
    })
  }

  let ro: ResizeObserver | null = null

  onMounted(() => {
    const el = container.value
    if (!el) return
    readScroll()
    el.addEventListener('scroll', onScroll, { passive: true })
    ro = new ResizeObserver(() => {
      readScroll()
      void measure()
    })
    ro.observe(el)
    void measure()
  })

  onBeforeUnmount(() => {
    if (raf) cancelAnimationFrame(raf)
    container.value?.removeEventListener('scroll', onScroll)
    ro?.disconnect()
  })

  // 列表变化（新增/删除/筛选）后需要重新测量
  const refresh = () => {
    readScroll()
    void measure()
  }

  return { items, totalHeight, measure, refresh, scrollTop }
}
