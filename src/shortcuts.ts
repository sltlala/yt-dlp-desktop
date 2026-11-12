/**
 * 任务列表的全局快捷键：只决定「按下的键对应哪个动作」，执行留给 App.vue。
 *
 * 抽成纯函数是为了能单测——「输入框里按空格不能暂停任务」这种规则
 * 写错了就是打字时任务被暂停，很难靠手点发现。
 */

export type ShortcutAction = 'paste' | 'togglePause' | 'remove' | 'selectAll' | 'escape'

export interface KeyInput {
  key: string
  ctrlKey: boolean
  metaKey: boolean
  altKey: boolean
  shiftKey: boolean
  target: EventTarget | null
}

/** 焦点在可编辑元素里时，键盘归它，快捷键一律不拦截。 */
export function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  if (target.isContentEditable) return true
  return ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName)
}

/** 开着弹窗时只认 Esc，别在弹窗背后操作列表。 */
export function shortcutFor(e: KeyInput, dialogOpen: boolean): ShortcutAction | null {
  if (e.key === 'Escape') return isEditable(e.target) ? null : 'escape'
  if (dialogOpen || isEditable(e.target) || e.altKey) return null

  const mod = e.ctrlKey || e.metaKey
  const key = e.key.toLowerCase()
  if (mod && !e.shiftKey && key === 'v') return 'paste'
  if (mod && !e.shiftKey && key === 'a') return 'selectAll'
  if (mod || e.shiftKey) return null
  if (e.key === ' ') return 'togglePause'
  if (e.key === 'Delete') return 'remove'
  return null
}

export const SHORTCUT_HELP = [
  ['Ctrl+V', '粘贴链接并添加任务'],
  ['空格', '暂停 / 继续展开的任务（多选时作用于所选）'],
  ['Delete', '移除展开的任务记录（多选时移除所选），不删文件'],
  ['Ctrl+A', '全选当前列表'],
  ['Esc', '退出多选 / 收起详情'],
] as const
