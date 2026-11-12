import { describe, expect, it } from 'vitest'
import {
  DEFAULT_FILENAME_TEMPLATE,
  FOLDER_PRESETS,
  TEMPLATE_SAMPLE,
  fieldsOf,
  renderTemplate,
  truncateBytes,
} from './filenameTemplate'

describe('renderTemplate', () => {
  it('不会把字段后面的字面量一起吞掉', () => {
    expect(renderTemplate(DEFAULT_FILENAME_TEMPLATE, { title: 'T', id: 'x', ext: 'mp4' })).toBe(
      'T [x].mp4',
    )
  })

  it('按字节截断，不切坏中文', () => {
    expect(renderTemplate('%(title).7B', { title: '中文标题' })).toBe('中文')
    expect(truncateBytes('中文标题', 7)).toBe('中文')
  })

  it('补零、日期格式、备选字段与兜底值', () => {
    expect(renderTemplate('%(playlist_index)02d', { playlist_index: '3' })).toBe('03')
    expect(renderTemplate('%(upload_date>%Y-%m)s', { upload_date: '20260929' })).toBe('2026-09')
    expect(renderTemplate('%(uploader,channel)s', { channel: 'C' })).toBe('C')
    expect(renderTemplate('%(uploader|未知作者)s', {})).toBe('未知作者')
  })

  it('不认识的字段原样保留', () => {
    expect(renderTemplate('%(nope)s', {})).toBe('%(nope)s')
  })

  it('分目录预设都能渲染出不含 NA 的路径', () => {
    for (const p of FOLDER_PRESETS) {
      const out = renderTemplate(p.expr, { ...TEMPLATE_SAMPLE, uploader: undefined as never })
      expect(out).not.toContain('%(')
      expect(out).toContain('/')
    }
  })
})

describe('fieldsOf', () => {
  it('拆出备选字段并去重', () => {
    expect(fieldsOf('%(uploader,channel|x)s/%(title)s %(title)s.%(ext)s')).toEqual([
      'uploader',
      'channel',
      'title',
      'ext',
    ])
  })
})
