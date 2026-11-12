import { inject, provide, ref, type InjectionKey, type Ref } from 'vue'
import { api } from '../../ipc'
import type { Settings } from '../../types'

/**
 * 设置页各分页共用的东西：正在编辑的那份设置，和几处都要用的文件对话框。
 *
 * 分页改的是**同一个** `s`，「保存 / 还原改动」在外层面板统一做。
 */
export interface SettingsContext {
  s: Settings
  /** 对话框、读列表之类操作的报错，显示在「账号」页底部。 */
  importError: Ref<string | null>
  browseFile: (key: 'archivePath' | 'cookieFile') => Promise<void>
  revealPath: (path: string) => Promise<void>
}

const KEY: InjectionKey<SettingsContext> = Symbol('settings')

export function provideSettingsContext(s: Settings): SettingsContext {
  const importError = ref<string | null>(null)

  /* 路径字段：对话框 + 手填 两条路都要有。
   * 对话框返回 `null` 表示用户取消，此时**什么都不做**（不能把字段清空）。 */
  async function browseFile(key: 'archivePath' | 'cookieFile') {
    try {
      const picked = await api.pickFile(s[key])
      if (picked) s[key] = picked
    } catch (e) {
      importError.value = `打开文件对话框失败：${String(e)}`
    }
  }

  async function revealPath(path: string) {
    const p = path.trim()
    if (!p) return
    try {
      await api.revealFile(p)
    } catch (e) {
      importError.value = `定位失败：${String(e)}`
    }
  }

  const ctx: SettingsContext = { s, importError, browseFile, revealPath }
  provide(KEY, ctx)
  return ctx
}

export function useSettingsContext(): SettingsContext {
  const ctx = inject(KEY)
  if (!ctx) throw new Error('设置分页必须放在 SettingsPanel 里')
  return ctx
}
