/**
 * 直接用 CDP 截图（而不是 PrintWindow）。
 *
 * 为什么不用 `scripts/capture-window.ps1`：WebView2 是合成层，`PrintWindow`
 * 抓的是窗口表面——刚还原最小化窗口、或正赶上重绘时，会抓到**上一帧的残影**，
 * 表现出来就是文字重影/发虚，看着像渲染 bug。CDP 的 `Page.captureScreenshot`
 * 由渲染引擎直接出图，没有这个问题，而且拿到的是精确的 CSS 像素。
 *
 * 用法:
 *   node scripts/cdp-shot.mjs <port> <out.png> [fullPage]
 *
 * 需要应用带 `--remote-debugging-port=<port>` 启动。
 */
import { writeFileSync, mkdirSync } from 'node:fs'
import { dirname } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'

const PORT = Number(process.argv[2] ?? 9334)
const OUT = process.argv[3]
const FULL = process.argv[4] === 'full'

if (!OUT) {
  console.error('用法: node scripts/cdp-shot.mjs <port> <out.png> [fullPage]')
  process.exit(1)
}

let page = null
for (let i = 0; i < 40; i++) {
  try {
    const r = await fetch(`http://127.0.0.1:${PORT}/json/list`)
    const targets = await r.json()
    // ⚠️ 只认 http(s)：page 目标里混着 `devtools://`（右键「检查」开的）和
    // `edge://permission-request-dialog/`（权限弹窗）这些浏览器自己的窗口，
    // 截出来就不是应用界面了。
    const pages = targets.filter((t) => t.type === 'page')
    page = pages.find((t) => /^https?:/.test(t.url)) ?? pages[0] ?? null
    if (page) break
  } catch {
    /* 还没起来 */
  }
  await sleep(250)
}
if (!page) {
  console.error(`端口 ${PORT} 上未找到调试目标`)
  process.exit(1)
}

const ws = new WebSocket(page.webSocketDebuggerUrl)
await new Promise((res, rej) => {
  ws.onopen = res
  ws.onerror = rej
})

let id = 0
const send = (method, params = {}) =>
  new Promise((res, rej) => {
    const myId = ++id
    const onMsg = (ev) => {
      const m = JSON.parse(ev.data)
      if (m.id !== myId) return
      ws.removeEventListener('message', onMsg)
      if (m.error) rej(new Error(`${method}: ${m.error.message}`))
      else res(m.result)
    }
    ws.addEventListener('message', onMsg)
    ws.send(JSON.stringify({ id: myId, method, params }))
  })

const r = await send('Page.captureScreenshot', {
  format: 'png',
  captureBeyondViewport: FULL,
  fromSurface: true,
})

mkdirSync(dirname(OUT), { recursive: true })
writeFileSync(OUT, Buffer.from(r.data, 'base64'))
console.log(`已保存 ${OUT}  (${Buffer.from(r.data, 'base64').length} bytes)`)
ws.close()
