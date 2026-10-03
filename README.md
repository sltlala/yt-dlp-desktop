# yt-dlp 桌面下载工具

基于 **yt-dlp.exe（子进程）** 的桌面下载器。宿主用 **Tauri 2（Rust）**，界面用 **Vue 3 + TypeScript**。

## 功能

- 下载视频 / 仅音频 / 字幕 / 封面，可调画质、容器与编码偏好
- aria2c 多线程下载、按章节切分、下载完成后执行自定义命令
- 断点续传、下载归档（自动跳过已下载）
- 代理：跟随系统 / 手动配置 / 按站点分流，支持绕过 Cloudflare 拦截
- Cookie 多账号（导入文件 / 从浏览器读取）
- 剪贴板监听、浏览器扩展一键推送、完成 / 失败系统通知
- 亮色 / 暗色主题

## 下载

Windows 安装包见 [Releases](https://github.com/sltlala/yt-dlp-desktop/releases)。

> 安装包暂无代码签名，Windows 会提示「未知发布者」，点「仍要运行」即可。

## 从源码构建

```bash
npm install              # 装前端依赖
npm run tauri:dev        # 开发运行
npm run tauri:build      # 打包（产出 NSIS 安装包）
cargo test -p ytdlp-core # 核心层单测
```

架构决策与 yt-dlp CLI 协议契约见 [`DESIGN.md`](DESIGN.md) / [`HANDOFF.md`](HANDOFF.md)。

## 许可证

本仓库代码采用 **GPL-3.0**（见 [`LICENSE`](LICENSE)）。

随包分发的第三方程序各自独立：

| 程序 | 许可证 |
|---|---|
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | Unlicense（公有领域） |
| [aria2](https://github.com/aria2/aria2) | GPL-2.0（许可证原文随安装包分发） |

详见 [`src-tauri/binaries/README-third-party.md`](src-tauri/binaries/README-third-party.md)。
