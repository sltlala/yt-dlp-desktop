/**
 * 附着到**已存在**的调试目标（如 Tauri 的 WebView2），抓控制台并执行表达式。
 *
 * 与 cdp-probe.mjs 的区别：那个自己启动 Edge，这个只附着不启动。
 *
 * 用法:
 *   node scripts/cdp-attach.mjs <port> [evalExpression]
 *
 * 启动应用时需带：
 *   WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<port>
 */
import { setTimeout as sleep } from 'node:timers/promises'
import { readFileSync } from 'node:fs'

const PORT = Number(process.argv[2] ?? 9334)
// 表达式可以直接给，也可以 `@文件路径`（避免 shell 引号转义问题）
let EXPR = process.argv[3] ?? ''
if (EXPR.startsWith('@')) {
  EXPR = readFileSync(EXPR.slice(1), 'utf8')
}
EXPR = EXPR.trim()

let targets = null
for (let i = 0; i < 40; i++) {
  try {
    const r = await fetch(`http://127.0.0.1:${PORT}/json/list`)
    targets = await r.json()
    if (targets.some((t) => t.type === 'page')) break
  } catch {
    /* 还没起来 */
  }
  await sleep(250)
}

// ⚠️ 必须优先挑**应用页面**。CDP 的 page 目标里混着一堆浏览器自己的窗口：
//   - `devtools://`                        —— 页面里右键「检查」开出来的
//   - `edge://permission-request-dialog/`  —— 剪贴板之类的权限弹窗
// 直接取第一个就会接到它们身上，报的错完全指不到原因（表达式语法明明是对的）。
// 判据用「是不是 http(s)」最省事，以后再多出别的内部页也不怕。
const pages = targets?.filter((t) => t.type === 'page') ?? []
const page = pages.find((t) => /^https?:/.test(t.url)) ?? pages[0]
if (!page) {
  console.error(`端口 ${PORT} 上未找到调试目标`)
  process.exit(1)
}
console.log('目标:', page.title, '|', page.url)

const ws = new WebSocket(page.webSocketDebuggerUrl)
await new Promise((res, rej) => {
  ws.onopen = res
  ws.onerror = rej
})

let id = 0
const send = (method, params = {}) =>
  new Promise((res) => {
    const myId = ++id
    const onMsg = (ev) => {
      const m = JSON.parse(ev.data)
      if (m.id === myId) {
        ws.removeEventListener('message', onMsg)
        res(m.result)
      }
    }
    ws.addEventListener('message', onMsg)
    ws.send(JSON.stringify({ id: myId, method, params }))
  })

const logs = []
ws.addEventListener('message', (ev) => {
  const m = JSON.parse(ev.data)
  if (m.method === 'Runtime.consoleAPICalled') {
    logs.push(
      `[${m.params.type}] ` +
        (m.params.args ?? []).map((a) => a.value ?? a.description ?? a.type).join(' '),
    )
  } else if (m.method === 'Runtime.exceptionThrown') {
    const d = m.params.exceptionDetails
    logs.push(`[EXCEPTION] ${d.exception?.description ?? d.text}`)
  } else if (m.method === 'Log.entryAdded') {
    const e = m.params.entry
    if (e.level === 'error') logs.push(`[${e.level}] ${e.text}`)
  }
})

await send('Runtime.enable')
await send('Log.enable')

if (EXPR) {
  const r = await send('Runtime.evaluate', {
    expression: EXPR,
    awaitPromise: true,
    returnByValue: true,
  })
  console.log('--- 求值结果 ---')
  if (r?.exceptionDetails) {
    console.log('抛错:', r.exceptionDetails.exception?.description ?? r.exceptionDetails.text)
  } else {
    console.log(JSON.stringify(r?.result?.value, null, 2))
  }
}

await sleep(600)
console.log('--- 控制台 ---')
console.log(logs.length ? logs.join('\n') : '(无错误)')
ws.close()
