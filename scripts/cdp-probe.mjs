/**
 * 开发期诊断脚本：启动无头 Edge，通过 CDP 抓取控制台错误与页面异常。
 *
 * 用法: node scripts/cdp-probe.mjs <url> [waitMs]
 * 依赖 Node 22 内置的 WebSocket，无需安装任何包。
 */
import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { setTimeout as sleep } from 'node:timers/promises'

const url = process.argv[2] ?? 'http://127.0.0.1:5183/'
const waitMs = Number(process.argv[3] ?? 4000)
const PORT = 9333

const EDGE = [
  'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
  'C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe',
].find((p) => existsSync(p))

if (!EDGE) {
  console.error('未找到 Edge')
  process.exit(1)
}

const edge = spawn(
  EDGE,
  [
    '--headless=new',
    '--disable-gpu',
    '--no-sandbox',
    `--remote-debugging-port=${PORT}`,
    '--user-data-dir=' + process.env.TEMP + '\\cdpprobe',
    'about:blank',
  ],
  { stdio: 'ignore' },
)

let ws
try {
  // 等 DevTools 端点就绪
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
  const page = targets?.find((t) => t.type === 'page')
  if (!page) throw new Error('未找到调试目标')

  ws = new WebSocket(page.webSocketDebuggerUrl)
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
      const text = (m.params.args ?? [])
        .map((a) => a.value ?? a.description ?? a.type)
        .join(' ')
      logs.push(`[${m.params.type}] ${text}`)
    } else if (m.method === 'Runtime.exceptionThrown') {
      const d = m.params.exceptionDetails
      logs.push(`[EXCEPTION] ${d.exception?.description ?? d.text}`)
    } else if (m.method === 'Log.entryAdded') {
      const e = m.params.entry
      if (e.level === 'error' || e.level === 'warning') {
        logs.push(`[${e.level}] ${e.text}`)
      }
    }
  })

  await send('Runtime.enable')
  await send('Log.enable')
  await send('Page.enable')
  await send('Page.navigate', { url })
  await sleep(waitMs)

  // 直接问 DOM 要结果，比截图可靠
  const probe = await send('Runtime.evaluate', {
    expression: `JSON.stringify({
      hasMask: !!document.querySelector('.mask'),
      hasDrawer: !!document.querySelector('.drawer'),
      hasModal: !!document.querySelector('.modal'),
      drawerText: (document.querySelector('.drawer')?.innerText ?? '').slice(0, 120),
      appHtmlLen: document.getElementById('app')?.innerHTML.length ?? 0
    })`,
    returnByValue: true,
  })

  console.log('URL        :', url)
  console.log('DOM 探测   :', probe?.result?.value)
  console.log('--- 控制台 ---')
  console.log(logs.length ? logs.join('\n') : '(无错误/警告)')
} finally {
  try {
    ws?.close()
  } catch {
    /* ignore */
  }
  edge.kill()
}
