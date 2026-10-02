# yt-dlp Desktop 浏览器扩展（一键推送）

配合桌面端「浏览器扩展一键推送」功能使用：在浏览器里右键「用 yt-dlp 下载」，
把当前页链接推送给本机正在运行的 yt-dlp Desktop。

## 安装

1. 打开 Chrome/Edge 的扩展管理页：
   - Chrome：地址栏输入 `chrome://extensions`
   - Edge：地址栏输入 `edge://extensions`
2. 打开右上角「开发者模式」。
3. 点「加载已解压的扩展程序」，选择**本目录**（`extension/`）。

## 使用前提

- 桌面端「设置 → 网络与账号 → 浏览器扩展一键推送」勾选了「启用本机接收端口」（默认开）。
- 桌面端正在运行。扩展把链接 POST 到 `http://127.0.0.1:19090/add`。

## 权限说明

- `contextMenus`：右键菜单。
- `activeTab` / `tabs`：读取当前标签页的 URL。
- `notifications`：推送结果提示。
- `http://127.0.0.1:19090/*`：只访问本机回环端口，不访问任何外网。

## 手动验证（不用扩展也能测接收端点）

桌面端运行时，在 PowerShell 里执行：

```powershell
Invoke-RestMethod -Method Post -Uri http://127.0.0.1:19090/add `
  -ContentType 'application/json' -Body '{"url":"https://www.youtube.com/watch?v=dQw4w9WgXcQ"}'
```

应返回 `ok: true`，且桌面端任务列表里出现这条任务。
