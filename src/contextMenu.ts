/**
 * 右键菜单：**只给我们自己的菜单**。
 *
 * Tauri 用的 WebView2 默认弹 Edge 的浏览器菜单。在桌面应用里这既不像原生，
 * 也有实际危害：「刷新」会丢掉没保存的设置改动，「检查」会把 DevTools 开在界面上。
 * 就算只是选中一段文字再右键，弹出来的也是整份浏览器菜单
 * （表情符号 / 导入密码 / 书写方向 / 更多工具 / 检查）——用户看到的就是这个。
 *
 * ## 为什么不做「选中文字就放行原生菜单」
 *
 * 第一版就是这么写的：输入框与「有选中文字」时都放行。结果是选中链接再右键，
 * 浏览器菜单原样回来——用户的反馈正是「怎么还有导入密码」。**放行原生菜单等于
 * 放行全部浏览器入口**，没有中间态。
 *
 * ## 粘贴为什么只能靠原生菜单
 *
 * 剪贴板**读**在 Chromium 里要 `clipboard-read` 权限。实测 WebView2 里
 * `navigator.clipboard.readText()` 会**直接挂住**（等一个没人能回答的授权），
 * `document.execCommand('paste')` 更是恒返回 false。所以「粘贴」这一项我们做不了，
 * 只能把原生菜单留在输入框上——那里正是粘贴最常用的地方（添加链接）。
 * 其它地方一律换成我们自己的菜单。
 *
 * 写剪贴板则没问题（不需要权限，只要用户手势），`复制` 由我们自己做。
 */

/** 一个菜单项。`sep` 表示分隔线。 */
type Item = { label: string; hint?: string; run: () => void } | 'sep'

/** 触发过一次自定义菜单的标记，便于测试与排查。 */
let menuEl: HTMLDivElement | null = null

function closeMenu() {
  menuEl?.remove()
  menuEl = null
}

/** 写剪贴板：优先现代接口，失败回落到隐藏 textarea + execCommand。 */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    // 老办法对**当前选区**生效，所以要先造一个选区
    const ta = document.createElement('textarea')
    ta.value = text
    ta.setAttribute('readonly', '')
    ta.style.cssText = 'position:fixed;top:-1000px;opacity:0'
    document.body.appendChild(ta)
    ta.select()
    try {
      return document.execCommand('copy')
    } catch {
      return false
    } finally {
      ta.remove()
    }
  }
}

function showMenu(x: number, y: number, items: Item[]) {
  closeMenu()
  const el = document.createElement('div')
  el.className = 'ctx-menu'
  el.setAttribute('role', 'menu')

  for (const it of items) {
    if (it === 'sep') {
      const hr = document.createElement('div')
      hr.className = 'ctx-sep'
      el.appendChild(hr)
      continue
    }
    const b = document.createElement('button')
    b.type = 'button'
    b.className = 'ctx-item'
    b.setAttribute('role', 'menuitem')
    const span = document.createElement('span')
    span.textContent = it.label
    b.appendChild(span)
    if (it.hint) {
      const em = document.createElement('em')
      em.textContent = it.hint
      b.appendChild(em)
    }
    // 用 mousedown 而不是 click：click 之前菜单可能已被外部点击关掉
    b.addEventListener('mousedown', (ev) => {
      ev.preventDefault()
      ev.stopPropagation()
      closeMenu()
      it.run()
    })
    el.appendChild(b)
  }

  // 先挂上去量一次尺寸，再夹到视口内——否则贴边时会被裁掉
  el.style.left = '-9999px'
  el.style.top = '-9999px'
  document.body.appendChild(el)
  menuEl = el
  const r = el.getBoundingClientRect()
  const left = Math.min(x, window.innerWidth - r.width - 4)
  const top = Math.min(y, window.innerHeight - r.height - 4)
  el.style.left = `${Math.max(4, left)}px`
  el.style.top = `${Math.max(4, top)}px`
}

/** 当前选中的文字（没有选中则为空串）。 */
function selectedText(): string {
  const sel = window.getSelection()
  if (!sel || sel.isCollapsed) return ''
  return sel.toString()
}

export function installContextMenuGuard(): void {
  document.addEventListener(
    'contextmenu',
    (e) => {
      const el = e.target as HTMLElement | null
      // 输入框放行：粘贴只能靠原生菜单（见文件头说明）
      if (el?.closest?.('input, textarea, [contenteditable="true"]')) return

      // 其余一律不弹浏览器菜单
      e.preventDefault()
      e.stopPropagation()

      const text = selectedText()
      const items: Item[] = []
      if (text.trim()) {
        items.push({ label: '复制', hint: 'Ctrl+C', run: () => void copyText(text) })
      }
      if (items.length) showMenu(e.clientX, e.clientY, items)
      else closeMenu()
    },
    // 捕获阶段：免得被某个组件的 stopPropagation 挡掉
    true,
  )

  // 点别处 / 滚动 / 按 Esc / 窗口失焦都要收起来。
  // ⚠️ 点在菜单**自己身上**时不能关：document 的捕获监听比菜单项的监听先跑，
  // 这里要是先把节点摘了，菜单项就再也点不到了。
  document.addEventListener(
    'mousedown',
    (e) => {
      const t = e.target
      if (menuEl && t instanceof Node && menuEl.contains(t)) return
      closeMenu()
    },
    true,
  )
  document.addEventListener('wheel', closeMenu, { capture: true, passive: true })
  window.addEventListener('blur', closeMenu)
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') closeMenu()
  })
}
