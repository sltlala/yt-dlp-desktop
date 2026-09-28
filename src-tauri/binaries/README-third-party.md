# 随包分发的第三方可执行文件

## yt-dlp

- **文件**：`yt-dlp-x86_64-pc-windows-msvc.exe`
- **来源**：<https://github.com/yt-dlp/yt-dlp/releases>（**必须是官方 onefile 版**，
  不是 winget / scoop 给的 onedir 形态——见 `README.md` 的说明）
- **许可证**：Unlicense（公有领域）
- **为什么要随包**：yt-dlp 因站点改版发版极频繁。作为独立 exe，
  **换一个文件就能修好全部站点问题**，宿主二进制一个字节都不用动。
  应用内的「检查更新」会把它原子替换到 `%APPDATA%\ytdlp-desktop\bin\`。

## aria2c

- **文件**：`aria2c-x86_64-pc-windows-msvc.exe`
- **版本**：1.37.0
- **来源**：<https://github.com/aria2/aria2/releases>
- **许可证**：**GPLv2**（原文见同目录 `aria2c-COPYING.txt`），
  并附 OpenSSL 链接例外（`aria2c-LICENSE.OpenSSL.txt`）。
  源码可从上面的仓库获取。

> ⚠️ **许可证与宿主应用不同。** 宿主应用与 yt-dlp 都不是 GPL，
> 但 aria2c 是。把多个独立程序**聚合**进同一个安装包，不会让宿主变成 GPL，
> 但分发时必须**一并提供它的许可证原文与源码出处**——所以这两个文本文件
> 通过 `bundle.resources` 随包分发，不能省。
>
> 同理，如果以后要换成非 GPL 的多线程下载器，直接替换这个文件即可，
> 应用侧只认「bundled → AppData → PATH」这个查找顺序。

### 怎么升级 aria2c

1. 从 releases 页下载 `aria2-<版本>-win-64bit-build1.zip`
2. 用其中的 `aria2c.exe` 覆盖 `aria2c-x86_64-pc-windows-msvc.exe`
3. 同时更新 `aria2c-COPYING.txt` 与 `aria2c-LICENSE.OpenSSL.txt`（如果变了）
4. 重新打包

**不需要改代码**：应用每次派发下载任务时都会跑一次 `aria2c --version` 验证候选，
拿不到版本就跳过它。

## 为什么 aria2c 走 PATH 而不是直接传路径

实测（详见 `DESIGN.md` §11.7）：

- `--downloader <绝对路径>` 指向**存在的**文件时被 yt-dlp **静默忽略**，
  它会不声不响地回落到内置下载器——最难排查的那种失败。
- `--downloader aria2c`（裸名）才会真正调用，靠的是 `PATH`。

所以宿主把**自带 aria2c 所在目录前置到子进程的 `PATH`**，仍然传裸名
`--downloader aria2c`。这样既能保证用的是我们自带那份，又不踩上面那个坑。
