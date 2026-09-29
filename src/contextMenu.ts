/**
 * 右键菜单：**默认只该有我们自己给的东西**。
 *
 * Tauri 用的 WebView2 默认会弹出 Edge 的菜单（返回 / 刷新 / 另存为 / 打印 /
 * 更多工具 / 检查）。在一个桌面应用里这既不像原生，也危险：
 * 「刷新」会把未保存的设置改动丢掉，「检查」会把 DevTools 开在界面上。
 *
 * 但不能一刀切全禁：
 * - 输入框里右键要能**粘贴**（设置页、添加链接都要用）；
 * - 选中文字后右键要能**复制**（复制链接、文件名）。
 *
 * 所以只在「既不是可编辑字段、也没有选中文字」时压掉默认菜单。
 */
export function installContextMenuGuard(): void {
  document.addEventListener(
    'contextmenu',
    (e) => {
      const el = e.target as HTMLElement | null
      const editable = el?.closest?.('input, textarea, [contenteditable="true"]')
      const selection = window.getSelection()
      const hasSelection = !!selection && !selection.isCollapsed

      if (editable || hasSelection) return
      e.preventDefault()
    },
    // 捕获阶段：免得某个组件自己 stopPropagation 把这条挡掉
    true,
  )
}
