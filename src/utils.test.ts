import { describe, expect, it } from 'vitest'
import {
  cacheBust,
  ellipsizeMiddle,
  ellipsizePath,
  fmtBytes,
  fmtDuration,
  fmtEta,
  fmtSpeed,
  shortenPath,
  splitError,
} from './utils'

describe('fmtBytes', () => {
  it('未知大小显示破折号而不是 0', () => {
    expect(fmtBytes(null)).toBe('—')
    expect(fmtBytes(undefined)).toBe('—')
  })

  it('小于 10 的带一位小数，其余取整', () => {
    expect(fmtBytes(512)).toBe('512 B')
    expect(fmtBytes(1536)).toBe('1.5 KB')
    expect(fmtBytes(200 * 1024 * 1024)).toBe('200 MB')
  })

  it('速度为 0 或未知时不显示 0 B/s', () => {
    expect(fmtSpeed(0)).toBe('—')
    expect(fmtSpeed(null)).toBe('—')
    expect(fmtSpeed(2 * 1024 * 1024)).toBe('2.0 MB/s')
  })
})

describe('时长', () => {
  it('不足一小时省略小时位', () => {
    expect(fmtDuration(65)).toBe('1:05')
    expect(fmtDuration(3725)).toBe('1:02:05')
  })

  it('剩余时间为 0 时提示即将完成', () => {
    expect(fmtEta(0)).toBe('即将完成')
    expect(fmtEta(90)).toBe('剩余 1:30')
  })
})

describe('路径截断', () => {
  it('shortenPath 只保留最后几级', () => {
    expect(shortenPath('C:\\Users\\me\\Downloads\\a.mp4')).toBe('…\\Downloads\\a.mp4')
    expect(shortenPath('C:\\a.mp4')).toBe('C:\\a.mp4')
  })

  it('ellipsizeMiddle 保留扩展名，按显示宽度截断', () => {
    const name = '超长的中文文件名'.repeat(10) + ' [BV1xx411c7mD].mp4'
    const out = ellipsizeMiddle(name, 40)
    expect(out.endsWith('.mp4')).toBe(true)
    expect(out).toContain('…')
    expect(ellipsizeMiddle('short.mp4', 40)).toBe('short.mp4')
  })

  it('ellipsizePath 放得下时原样返回', () => {
    expect(ellipsizePath('D:\\v\\a.mp4', 84)).toBe('D:\\v\\a.mp4')
  })
})

describe('splitError', () => {
  it('第一行是说明，其余是原始报错', () => {
    const { summary, detail } = splitError('需要登录：配置 Cookie\n[youtube] x: Sign in to confirm')
    expect(summary).toBe('需要登录：配置 Cookie')
    expect(detail).toBe('[youtube] x: Sign in to confirm')
  })

  it('单行报错没有详情', () => {
    expect(splitError('下载失败（yt-dlp 退出码 1）')).toEqual({
      summary: '下载失败（yt-dlp 退出码 1）',
      detail: '',
    })
    expect(splitError(null)).toEqual({ summary: '', detail: '' })
  })
})

describe('cacheBust', () => {
  it('按已有查询串选择分隔符', () => {
    expect(cacheBust('https://a/x.jpg', 2)).toBe('https://a/x.jpg?_refresh=2')
    expect(cacheBust('https://a/x.jpg?w=1', 2)).toBe('https://a/x.jpg?w=1&_refresh=2')
  })

  it('data: 内联图不能加查询串', () => {
    expect(cacheBust('data:image/png;base64,AA', 1)).toBe('data:image/png;base64,AA')
    expect(cacheBust(null, 1)).toBeNull()
  })
})
