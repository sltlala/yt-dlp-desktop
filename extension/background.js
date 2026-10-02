// yt-dlp Desktop 一键推送（ROADMAP §F19）
//
// 右键菜单「用 yt-dlp 下载」→ 把当前标签页 URL POST 到本机应用端口。
// MV3 service worker，不能直接访问页面 DOM，所以用 activeTab + tabs API 拿 URL。

const PUSH_URL = 'http://127.0.0.1:19090/add'

// 安装/启动时注册右键菜单。
chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: 'ytdlp-push',
    title: '用 yt-dlp 下载',
    contexts: ['page', 'link', 'video', 'audio', 'selection'],
  })
})

chrome.runtime.onStartup?.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: 'ytdlp-push',
      title: '用 yt-dlp 下载',
      contexts: ['page', 'link', 'video', 'audio', 'selection'],
    })
  })
})

// 点击右键菜单。
chrome.contextMenus.onClicked.addListener((info, tab) => {
  // link/video/audio 等上下文里 info.linkUrl / info.srcUrl 更精确；
  // 否则退回当前标签页 URL。
  const url =
    info.linkUrl || info.srcUrl || info.pageUrl || (tab && tab.url) || ''

  if (!url) {
    notify('没拿到链接')
    return
  }
  push(url)
})

async function push(url) {
  try {
    const resp = await fetch(PUSH_URL, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ url }),
    })
    const data = await resp.json().catch(() => ({}))
    if (resp.ok && data.ok) {
      notify('已推送给 yt-dlp Desktop')
    } else {
      notify('推送失败：' + (data.error || '应用未运行或未启用接收端口'))
    }
  } catch (e) {
    notify('连不上本机应用（127.0.0.1:19090），请确认 yt-dlp Desktop 正在运行')
  }
}

function notify(message) {
  // service worker 里没有 window.alert；用通知 API（需 notifications 权限）
  // 或直接 console。这里用 chrome.notifications，若未授权则静默。
  try {
    chrome.notifications.create({
      type: 'basic',
      title: 'yt-dlp Desktop',
      message,
    })
  } catch (_) {
    console.log('[ytdlp-push]', message)
  }
}
