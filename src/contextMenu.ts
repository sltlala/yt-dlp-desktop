/**
 * 右键菜单：**只给我们自己的菜单**。
 *
 * Tauri 用的 WebView2 默认弹 Edge 的浏览器菜单。在桌面应用里这既不像原生，
 * 也有实际危害：「刷新」会丢掉没保存的设置改动，「检查」会把 DevTools 开在界面上。
 *
 * ## 一条踩出来的结论：不要「放行原生菜单」这条中间路线
 *
 * 第一版规则是「不是输入框、也没有选中文字时才压掉」，想着「选中文字时留原生菜单
 * 好让用户复制」。结果选中链接再右键，弹出来的是**整份**浏览器菜单
 * （表情符号 / 导入密码 / 书写方向 / 更多工具 / 检查）。
 * 第二版把输入框也放行（为了粘贴），用户在「添加任务」输入框上又碰到了同样的问题。
 *
 * **放行原生菜单等于放行全部浏览器入口**，没有中间态。所以现在一律压掉，
 * 需要什么就自己实现什么。
 *
 * ## 各项是怎么实现的
 *
 * | 动作 | 做法 |
 * |---|---|
 * | 复制 | `navigator.clipboard.writeText`，失败回落隐藏 textarea + `execCommand('copy')` |
 * | 剪切 | 先复制，再 `execCommand('delete')` |
 * | 粘贴 | **走后端**读剪贴板（`api.readClipboard()`），再 `execCommand('insertText')` |
 * | 全选 | input/textarea 用 `.select()`，其余用 `execCommand('selectAll')` |
 *
 * 「粘贴」是唯一前端做不了的：`navigator.clipboard.readText()` 在 WebView2 里会卡在
 * 权限弹窗上，`execCommand('paste')` 恒为 false（原因见 `src-tauri/src/clipboard.rs`）。
 *
 * 用 `execCommand('insertText')` 而不是直接改 `.value`：前者会**触发 input 事件**
 * （Vue 的 v-model 靠它同步），也进撤销栈；直接赋值两样都没有。
 */
import { api } from './ipc'

/** 一个菜单项。 */
type Item = { label: string; hint?: string; run: () => void | Promise<void> }

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
    // 老办法只对**当前选区**生效，所以先造一个选区
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
    // 用 mousedown 而不是 click：click 之前菜单可能已被外部点击关掉。
    // preventDefault 还能让焦点留在原来的输入框上——粘贴要靠它。
    b.addEventListener('mousedown', (ev) => {
      ev.preventDefault()
      ev.stopPropagation()
      closeMenu()
      void it.run()
    })
    el.appendChild(b)
  }

  // 先挂上去量尺寸再夹进视口，否则贴边时会被裁掉
  el.style.left = '-9999px'
  el.style.top = '-9999px'
  document.body.appendChild(el)
  menuEl = el
  const r = el.getBoundingClientRect()
  el.style.left = `${Math.max(4, Math.min(x, window.innerWidth - r.width - 4))}px`
  el.style.top = `${Math.max(4, Math.min(y, window.innerHeight - r.height - 4))}px`
}

function selectedText(): string {
  const sel = window.getSelection()
  if (!sel || sel.isCollapsed) return ''
  return sel.toString()
}

/** 可编辑字段里当前选中的片段（用于判断「剪切/复制」能不能点）。 */
function editableSelection(el: HTMLElement): string {
  if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
    const { selectionStart: a, selectionEnd: b } = el
    return a !== null && b !== null && b > a ? el.value.slice(a, b) : ''
  }
  return selectedText()
}

async function doPaste(el: HTMLElement) {
  el.focus()
  let text = ''
  try {
    text = await api.readClipboard()
  } catch {
    return
  }
  if (!text) return
  // insertText 会触发 input 事件（v-model 依赖它）并进撤销栈
  document.execCommand('insertText', false, text)
}

function doSelectAll(el: HTMLElement) {
  el.focus()
  if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
    el.select()
  } else {
    document.execCommand('selectAll')
  }
}

function itemsFor(target: HTMLElement | null): Item[] {
  const editable = target?.closest?.(
    'input, textarea, [contenteditable="true"]',
  ) as HTMLElement | null
  const sel = selectedText()

  if (editable) {
    const own = editableSelection(editable)
    const items: Item[] = []
    if (own) {
      items.push({
        label: '剪切',
        hint: 'Ctrl+X',
        run: async () => {
          if (await copyText(own)) document.execCommand('delete')
        },
      })
      items.push({ label: '复制', hint: 'Ctrl+C', run: () => void copyText(own) })
    }
    items.push({ label: '粘贴', hint: 'Ctrl+V', run: () => doPaste(editable) })
    items.push({ label: '全选', hint: 'Ctrl+A', run: () => doSelectAll(editable) })
    return items
  }

  // 非输入框：只有选中了文字才有得可做
  if (sel.trim()) {
    return [{ label: '复制', hint: 'Ctrl+C', run: () => void copyText(sel) }]
  }
  return []
}

export function installContextMenuGuard(): void {
  document.addEventListener(
    'contextmenu',
    (e) => {
      // 一律不让浏览器弹自己的菜单
      e.preventDefault()
      e.stopPropagation()

      const items = itemsFor(e.target as HTMLElement | null)
      if (items.length) showMenu(e.clientX, e.clientY, items)
      else closeMenu()
    },
    // 捕获阶段：免得被某个组件的 stopPropagation 挡掉
    true,
  )

  // 点别处 / 滚动 / 按 Esc / 失焦都要收起来。
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
