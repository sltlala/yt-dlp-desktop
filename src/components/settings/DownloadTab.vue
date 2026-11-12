<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { api, type UpdateInfo } from '../../ipc'
import { useTaskStore } from '../../stores/tasks'
import type { JsRuntimeInfo } from '../../types'
import { ellipsizePath, fmtBytes } from '../../utils'
import { useSettingsContext } from './context'

const store = useTaskStore()
const { s, browseFile, revealPath } = useSettingsContext()

/** 展示用的 aria2c 落点信息（随包副本还是 PATH 上那份）。 */
const ariaInfo = ref<{ found: boolean; path: string | null; version: string | null } | null>(null)

async function loadAria2c() {
  try {
    ariaInfo.value = await api.aria2cInfo()
  } catch {
    /* 纯展示信息，取不到不影响使用 */
  }
}

/* ── JS 运行时 ──
 *
 * 实测：同一份 yt-dlp、同一个链接、同样的 cookie 与代理，只差 `--js-runtimes node`
 * 就是「需要重载页面」与「成功」的区别。yt-dlp **不会**自动启用已安装的运行时，
 * 所以这里默认自动检测并传进去。
 */
const jsInfo = ref<JsRuntimeInfo | null>(null)

async function loadJsRuntimes() {
  try {
    jsInfo.value = await api.detectJsRuntimes()
  } catch {
    /* 纯诊断信息，取不到不影响使用 */
  }
}

/* ───────────────────── yt-dlp 自更新（DESIGN §8）───────────────────── */

const updateInfo = ref<UpdateInfo | null>(null)
const updateMsg = ref<string | null>(null)
const updateOk = ref(false)
const updateBusy = ref(false)

async function checkUpdate() {
  updateBusy.value = true
  updateMsg.value = null
  updateInfo.value = null
  try {
    updateInfo.value = await api.checkYtdlpUpdate()
  } catch (e) {
    updateOk.value = false
    updateMsg.value = String(e)
  } finally {
    updateBusy.value = false
  }
}

/** 更新本身**不依赖 GitHub API**（API 有速率配额），所以即使 check 失败也能更新。 */
async function runUpdate() {
  updateBusy.value = true
  updateMsg.value = null
  try {
    updateMsg.value = await api.applyYtdlpUpdate()
    updateOk.value = true
    updateInfo.value = await api.checkYtdlpUpdate().catch(() => null)
  } catch (e) {
    updateOk.value = false
    updateMsg.value = String(e)
  } finally {
    updateBusy.value = false
  }
}

onMounted(() => {
  void loadJsRuntimes()
  void loadAria2c()
  void store.loadFfmpeg()
})
</script>

<template>
  <section>
    <label class="check">
      <input v-model="s.aria2c" type="checkbox" />
      <span>使用 aria2c 多线程下载<em>高级选项，默认关闭</em></span>
    </label>
    <p class="note warn">
      进度改由 aria2c 上报，精度略低；遇到不支持 Range 的服务器会失败，失败时自动回落原生下载。
    </p>

    <h3>章节切分</h3>
    <label class="check">
      <input v-model="s.splitChapters" type="checkbox" />
      <span>按章节拆分成多个文件<em>长视频会拆成每章一个文件，显著增加后处理时间</em></span>
    </label>

    <h3>SponsorBlock</h3>
    <label class="field">
      <span>赞助片段<em>只对 YouTube 生效，片段数据来自社区标注</em></span>
      <select v-model="s.sponsorblock">
        <option value="off">不处理</option>
        <option value="mark">标成章节（视频不变）</option>
        <option value="remove">剪掉赞助、自我推广、求关注片段</option>
      </select>
    </label>

    <h3>下载后动作</h3>
    <label class="field">
      <span>完成后执行命令<em>留空 = 不执行</em></span>
      <input
        v-model="s.execCommand"
        class="mono"
        spellcheck="false"
        placeholder="例如：shutdown /s /t 60"
      />
    </label>
    <p class="note warn">
      ⚠️ 该命令会在下载完成后<strong>以本机权限执行</strong>。支持
      <code class="mono">{{ '{filename}' }}</code> 占位符（每个成品文件执行一次）。请只填写你信任的命令。
    </p>

    <h3>下载归档</h3>
    <label class="check">
      <input v-model="s.archiveEnabled" type="checkbox" />
      <span>启用 download-archive<em>已下载过的视频将被跳过</em></span>
    </label>
    <div v-if="s.archiveEnabled" class="field">
      <span>归档文件路径<em>不存在会自动创建</em></span>
      <div class="path-row">
        <input v-model="s.archivePath" class="mono" spellcheck="false" />
        <button class="btn sm" title="选择已存在的归档文件" @click="browseFile('archivePath')">
          浏览…
        </button>
        <button
          class="btn sm"
          :disabled="!s.archivePath.trim()"
          title="在资源管理器中定位该文件"
          @click="revealPath(s.archivePath)"
        >
          定位
        </button>
      </div>
    </div>
    <p class="note">
      跳过的任务会显示为「已跳过」而非「已完成」——两者在 yt-dlp 里都返回退出码 0。
    </p>

    <h3>JS 运行时</h3>
    <div class="js-list">
      <span
        v-for="c in jsInfo?.candidates ?? []"
        :key="c.name"
        class="js-chip"
        :class="{ on: !!c.path }"
        :title="c.path ?? '未安装'"
      >
        {{ c.name }}{{ c.path ? ' ✓' : '' }}
      </span>
    </div>
    <label class="field">
      <span>指定运行时<em>逗号分隔；留空 = 自动检测</em></span>
      <input
        v-model="s.jsRuntime"
        class="mono"
        :placeholder="(jsInfo?.detected ?? []).join(',') || '（未检测到）'"
      />
    </label>

    <h3>aria2c（多线程下载器）</h3>
    <p v-if="ariaInfo?.found" class="sub-line">
      {{ ariaInfo.version }}<br />
      <code class="mono">{{ ellipsizePath(ariaInfo.path ?? '', 76) }}</code>
    </p>
    <p v-else class="path-warn">
      没有找到可用的 aria2c——勾了「多线程下载」也会自动回落到内置下载器，
      并在任务详情里留一条提示。
    </p>

    <h3>ffmpeg（合并 / 转码）</h3>
    <p v-if="store.ffmpeg?.found" class="sub-line">
      {{ store.ffmpeg.version }}<br />
      <code class="mono">{{ ellipsizePath(store.ffmpeg.path ?? '', 76) }}</code>
    </p>
    <p v-else-if="store.ffmpeg" class="path-warn">
      没有找到 ffmpeg：高画质视频的音视频无法合并（会降级为低画质），
      也无法转换音频格式、嵌入字幕和封面。
    </p>
    <div class="row-inline test-row">
      <button
        class="btn sm"
        :class="{ primary: store.ffmpeg && !store.ffmpeg.found }"
        :disabled="!!store.ffmpegInstall"
        @click="store.installFfmpeg()"
      >
        {{ store.ffmpegInstall ? '下载中…' : store.ffmpeg?.found ? '重新下载' : '一键下载 ffmpeg' }}
      </button>
      <span v-if="store.ffmpegInstall" class="ver mono">
        {{ fmtBytes(store.ffmpegInstall.done) }}
        <template v-if="store.ffmpegInstall.total"> / {{ fmtBytes(store.ffmpegInstall.total) }}</template>
      </span>
    </div>
    <p class="note">
      来自 yt-dlp 官方维护的 FFmpeg-Builds（约 140 MB，GPL），装到数据目录的 bin 下，
      走当前代理设置。也可以自己装好并加入 PATH。
    </p>
    <p
      v-if="store.ffmpegInstallMsg"
      class="res"
      :class="store.ffmpegInstallMsg.ok ? 'ok' : 'bad'"
    >
      {{ store.ffmpegInstallMsg.text }}
    </p>

    <h3>yt-dlp 版本</h3>
    <div class="row-inline test-row">
      <button class="btn sm" :disabled="updateBusy" @click="checkUpdate">
        {{ updateBusy ? '处理中…' : '检查更新' }}
      </button>
      <button class="btn sm primary" :disabled="updateBusy" @click="runUpdate">
        立即更新
      </button>
      <span v-if="updateInfo" class="ver mono">
        {{ updateInfo.current || '未知' }} → {{ updateInfo.latest }}
      </span>
    </div>
    <p v-if="updateInfo && updateInfo.newer" class="note">
      有新版本可用（{{ updateInfo.current }} → {{ updateInfo.latest }}）。
    </p>
    <p v-else-if="updateInfo" class="note">已是最新版本（{{ updateInfo.current }}）。</p>
    <p v-if="updateMsg" class="res" :class="updateOk ? 'ok' : 'bad'">{{ updateMsg }}</p>
  </section>
</template>
