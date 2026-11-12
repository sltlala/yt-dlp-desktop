<script setup lang="ts">
import { computed, nextTick, onMounted, ref } from 'vue'
import { api, type AppUpdateInfo } from '../../ipc'
import { useTaskStore } from '../../stores/tasks'
import type { DataDirInfo, TempDirInUse } from '../../types'
import { fmtBytes } from '../../utils'
import { useSettingsContext } from './context'
import {
  DEFAULT_FILENAME_TEMPLATE,
  FOLDER_PRESETS,
  TEMPLATE_FIELD_GROUPS,
  TEMPLATE_FIELDS,
  TEMPLATE_MODIFIERS,
  TEMPLATE_PRESETS,
  TEMPLATE_SAMPLE,
  fieldsOf,
  renderTemplate,
} from './filenameTemplate'

const store = useTaskStore()
const { s, importError, revealPath } = useSettingsContext()

/* ── 数据目录（DESIGN §5.6）──
 *
 * 用户最常问的就是「我的历史存哪了」。AppData 在资源管理器里默认还是隐藏的，
 * 所以把路径直接摆出来，并说明怎么切成便携模式。
 */
const dataDir = ref<DataDirInfo | null>(null)

/** 临时目录现在能不能改（还有 .part 碎片时不能）。 */
const tempInUse = ref<TempDirInUse>({ inUse: false, count: 0, taskIds: [] })

async function loadTempInUse() {
  try {
    tempInUse.value = await api.tempDirInUse()
  } catch {
    // 取不到就当「没占用」：宁可让用户改，也不要因为一个探测失败把人锁死
    tempInUse.value = { inUse: false, count: 0, taskIds: [] }
  }
}

async function loadDataDir() {
  try {
    dataDir.value = await api.dataDir()
  } catch {
    /* 取不到就不显示这一栏，不影响别的设置 */
  }
}

/* ───────────────── 路径字段：对话框 + 手填 两条路都要有 ─────────────────
 *
 * 原生对话框拿不到就手填——**两条路都必须留着**：
 * 路径常常是从别处（下载目录、另一个软件）复制过来的，只能粘贴；
 * 而要找「那个盘上的某个文件夹」时，手打又太痛苦。
 *
 * 对话框返回 `null` 表示用户取消，此时**什么都不做**（不能把字段清空）。
 */

async function browseFolder(key: 'outputDir' | 'tempDir') {
  try {
    const picked = await api.pickFolder(s[key])
    if (picked) s[key] = picked
  } catch (e) {
    importError.value = `打开文件夹对话框失败：${String(e)}`
  }
}

/** 在资源管理器里定位（文件会被选中，目录会被打开）。 */
async function openDir(path: string) {
  const dir = path.trim()
  if (!dir) return
  try {
    await api.openFile(dir)
  } catch (e) {
    importError.value = `打开目录失败：${String(e)}`
  }
}

/**
 * 目录字段的即时提示。
 *
 * 只做**形态**检查，不验存在性——后端没有「这个路径存不存在」的查询，
 * 而且网络盘/移动硬盘暂时不在线也不该拦着用户保存。
 * 这里拦的是最容易犯的错：填了个文件而不是目录、或者填了相对路径。
 */
function dirWarning(key: 'outputDir' | 'tempDir'): string | null {
  const v = s[key].trim()
  if (!v) return '留空会用默认目录'
  if (!/^[a-zA-Z]:[\\/]/.test(v) && !v.startsWith('\\\\') && !v.startsWith('/')) {
    return '看起来不是绝对路径，建议用「浏览…」选一个'
  }
  if (/[\\/][^\\/]*\.[a-zA-Z0-9]{1,5}$/.test(v)) {
    return '这看起来是一个文件而不是目录'
  }
  return null
}

/* ── 应用自身的更新 ── */
const appUpdate = ref<AppUpdateInfo | null>(null)
const appUpdateMsg = ref<string | null>(null)
const appUpdateBusy = ref(false)
const appUpdateProgress = ref<{ downloaded: number; total: number | null } | null>(null)

async function checkAppUpdate() {
  appUpdateBusy.value = true
  appUpdateMsg.value = null
  try {
    appUpdate.value = await api.checkAppUpdate()
  } catch (e) {
    appUpdateMsg.value = String(e)
  } finally {
    appUpdateBusy.value = false
  }
}

async function installAppUpdate() {
  appUpdateBusy.value = true
  appUpdateMsg.value = null
  const un = await api.onAppUpdateProgress((p) => (appUpdateProgress.value = p))
  try {
    await api.installAppUpdate()
  } catch (e) {
    appUpdateMsg.value = String(e)
  } finally {
    un()
    appUpdateBusy.value = false
    appUpdateProgress.value = null
  }
}

const tplInput = ref<HTMLInputElement | null>(null)

/** 在光标处插入一个字段。没有焦点时追加到末尾。 */
function insertField(name: string) {
  const token = `%(${name})s`
  const el = tplInput.value
  if (!el) {
    s.filenameTemplate += token
    return
  }
  const start = el.selectionStart ?? s.filenameTemplate.length
  const end = el.selectionEnd ?? start
  s.filenameTemplate =
    s.filenameTemplate.slice(0, start) + token + s.filenameTemplate.slice(end)
  void nextTick(() => {
    el.focus()
    el.setSelectionRange(start + token.length, start + token.length)
  })
}

/**
 * 预览文本。有真实任务时用它的标题，预览才有说服力
 * （长标题截断的效果一眼就能看出来）。
 */
const templatePreview = computed(() => {
  const tpl = s.filenameTemplate.trim()
  if (!tpl) return '（留空会用默认模板）'
  const real = store.tasks.find((t) => t.title)?.title
  const sample = real ? { ...TEMPLATE_SAMPLE, title: real } : TEMPLATE_SAMPLE
  return renderTemplate(tpl, sample)
})

const templateFields = computed(() => fieldsOf(s.filenameTemplate))

const templateWarning = computed(() => {
  const tpl = s.filenameTemplate.trim()
  if (!tpl) return '留空会用默认模板'
  if (!templateFields.value.includes('ext')) {
    return '模板里没有 %(ext)s —— 实测下出来的文件会真的没有扩展名，建议加上'
  }
  const known = new Set(TEMPLATE_FIELDS.map((f) => f.name))
  const unknown = templateFields.value.filter((n) => !known.has(n))
  if (unknown.length) {
    return `未知字段 ${unknown.map((n) => `%(${n})s`).join('、')} —— yt-dlp 不会报错，会静默填成 NA。是不是拼错了？`
  }
  return null
})

onMounted(() => {
  void loadDataDir()
  void loadTempInUse()
})
</script>

<template>
  <section>
    <h3>主题</h3>
    <div class="radios row">
      <label class="radio">
        <input v-model="s.theme" type="radio" value="light" />
        <span>亮色</span>
      </label>
      <label class="radio">
        <input v-model="s.theme" type="radio" value="dark" />
        <span>暗色</span>
      </label>
      <label class="radio">
        <input v-model="s.theme" type="radio" value="system" />
        <span>跟随系统</span>
      </label>
    </div>

    <div class="field">
      <span>输出目录</span>
      <div class="path-row">
        <input v-model="s.outputDir" class="mono" spellcheck="false" />
        <button class="btn sm" title="选择文件夹" @click="browseFolder('outputDir')">
          浏览…
        </button>
        <button
          class="btn sm"
          :disabled="!s.outputDir.trim()"
          title="在资源管理器中打开这个目录"
          @click="openDir(s.outputDir)"
        >
          打开
        </button>
      </div>
      <em v-if="dirWarning('outputDir')" class="path-warn">{{ dirWarning('outputDir') }}</em>
    </div>
    <div class="field">
      <span>临时目录<em>断点续传依赖它稳定不变</em></span>
      <div class="path-row">
        <input
          v-model="s.tempDir"
          class="mono"
          spellcheck="false"
          :disabled="tempInUse.inUse"
        />
        <button
          class="btn sm"
          title="选择文件夹"
          :disabled="tempInUse.inUse"
          @click="browseFolder('tempDir')"
        >
          浏览…
        </button>
        <button
          class="btn sm"
          :disabled="!s.tempDir.trim()"
          title="在资源管理器中打开这个目录"
          @click="openDir(s.tempDir)"
        >
          打开
        </button>
      </div>
      <!--
        有 .part 碎片时**锁住**这个字段，而不是只弹个警告：
        改了以后那些碎片在新目录里根本找不到，续传会静默失效、
        旧目录还会永久残留（清理只扫当前 tempRoot）。
      -->
      <em v-if="tempInUse.inUse" class="path-warn">
        有 {{ tempInUse.count }} 个任务的临时目录里还留着未完成的分片，现在改这里会让它们的断点续传失效。
        先让这些任务下载完，或在任务详情里「移除记录」（会一并清理临时文件）后再改。
      </em>
      <em v-else-if="dirWarning('tempDir')" class="path-warn">{{ dirWarning('tempDir') }}</em>
    </div>
    <div class="field">
      <span>数据目录<em>设置、历史记录、cookies 都在这里{{ dataDir?.portable ? '（便携模式）' : '' }}</em></span>
      <div class="path-row">
        <input :value="dataDir?.root ?? ''" class="mono" readonly spellcheck="false" />
        <button
          class="btn sm"
          :disabled="!dataDir?.root"
          title="在资源管理器中打开这个目录"
          @click="openDir(dataDir?.root ?? '')"
        >
          打开
        </button>
      </div>
      <em v-if="dataDir && !dataDir.portable" class="path-hint">
        想让整个文件夹可以拷走就用便携模式：在程序目录（exe 旁边）建一个空的
        <code>{{ dataDir.marker }}</code>，重启后数据会改放到程序目录下的 <code>data\</code>。
      </em>
    </div>
    <div class="field">
      <span>日志<em>下载失败时可以把这个文件发出来定位</em></span>
      <div class="path-row">
        <input :value="dataDir?.log ?? '（启动中…）'" class="mono" readonly spellcheck="false" />
        <button
          class="btn sm"
          :disabled="!dataDir?.log"
          title="在资源管理器中定位日志文件"
          @click="revealPath(dataDir?.log ?? '')"
        >
          定位
        </button>
      </div>
    </div>
    <div class="field">
      <span>文件名模板<em>决定下载下来的文件叫什么；下面有可用字段清单</em></span>
      <div class="path-row">
        <input
          ref="tplInput"
          v-model="s.filenameTemplate"
          class="mono"
          spellcheck="false"
          placeholder="%(title).150B [%(id)s].%(ext)s"
        />
        <button
          class="btn sm"
          :disabled="s.filenameTemplate === DEFAULT_FILENAME_TEMPLATE"
          title="恢复成默认模板"
          @click="s.filenameTemplate = DEFAULT_FILENAME_TEMPLATE"
        >
          恢复默认
        </button>
      </div>

      <!-- 实时预览：模板写对没有，看一眼就知道 -->
      <div class="tpl-preview">
        <span class="tp-label">预览</span>
        <code class="mono">{{ templatePreview }}</code>
      </div>
      <em v-if="templateWarning" class="path-warn">{{ templateWarning }}</em>

      <!-- 常用模板：点一下就能用，省得从零拼 -->
      <div class="tpl-presets">
        <button
          v-for="p in TEMPLATE_PRESETS"
          :key="p.label"
          class="tpl-preset"
          :class="{ on: s.filenameTemplate === p.expr }"
          :title="p.expr"
          @click="s.filenameTemplate = p.expr"
        >
          {{ p.label }}
        </button>
      </div>
      <div class="tpl-presets">
        <span class="tp-label">存到子目录</span>
        <button
          v-for="p in FOLDER_PRESETS"
          :key="p.label"
          class="tpl-preset"
          :class="{ on: s.filenameTemplate === p.expr }"
          :title="p.expr"
          @click="s.filenameTemplate = p.expr"
        >
          {{ p.label }}
        </button>
      </div>

      <!-- 可用字段：默认折叠，需要时展开 -->
      <details class="tpl-vars">
        <summary>可用变量（点一下插入到光标处）</summary>
        <div v-for="g in TEMPLATE_FIELD_GROUPS" :key="g.group" class="vg">
          <div class="vg-title">{{ g.group }}</div>
          <div class="vg-grid">
            <button
              v-for="f in g.fields"
              :key="f.name"
              class="var"
              :title="`${f.desc}　样例：${f.sample}`"
              @click="insertField(f.name)"
            >
              <code class="mono">{{ f.name }}</code>
              <span>{{ f.desc }}</span>
            </button>
          </div>
        </div>
        <div class="vg">
          <div class="vg-title">格式修饰符（写在字段后面）</div>
          <table class="mods">
            <tr v-for="m in TEMPLATE_MODIFIERS" :key="m.code">
              <td><code class="mono">{{ m.code }}</code></td>
              <td>{{ m.desc }}</td>
            </tr>
          </table>
        </div>
        <p class="note">
          字段名拼错时 yt-dlp <strong>不会报错</strong>，会静默填成 <code>NA</code>。
        </p>
      </details>
    </div>
    <label class="field">
      <span>限速<em>留空为不限速，如 5M</em></span>
      <input v-model="s.limitRate" class="mono" placeholder="不限速" />
    </label>

    <h3>并发</h3>
    <div class="tri-col">
      <label class="field inline">
        <span>探测并发</span>
        <input v-model.number="s.probeConcurrency" type="number" min="1" max="16" />
      </label>
      <label class="field inline">
        <span>下载并发</span>
        <input v-model.number="s.downloadConcurrency" type="number" min="1" max="8" />
      </label>
      <label class="field inline">
        <span>同一站点并发</span>
        <input v-model.number="s.perHostConcurrency" type="number" min="1" max="4" />
      </label>
    </div>

    <h3>窗口</h3>
    <label class="check">
      <input v-model="s.closeToTray" type="checkbox" />
      <span>点关闭按钮时最小化到托盘，下载在后台继续（右键托盘图标可退出）</span>
    </label>

    <h3>通知</h3>
    <label class="check">
      <input v-model="s.notifyOnComplete" type="checkbox" />
      <span>下载完成时弹系统通知</span>
    </label>
    <label class="check">
      <input v-model="s.notifyOnFailure" type="checkbox" />
      <span>下载失败时弹系统通知</span>
    </label>
    <label class="check">
      <input v-model="s.notifyOnSkip" type="checkbox" />
      <span>
        「已跳过」也通知
        <em>跳过 = 归档命中或文件已存在，通常不是坏事，默认关</em>
      </span>
    </label>
    <p class="note">
      任务在后台排队时，切到别的窗口也能收到结果提醒。
      通知由应用发出，无需额外的系统权限（开发模式 `tauri dev` 下可能不弹，打包安装后正常）。
    </p>

    <label class="check">
      <input v-model="s.watchClipboard" type="checkbox" />
      <span>
        监听剪贴板，复制链接即提示下载
        <em>复制一个视频链接后弹「要下载吗」气泡；默认关，避免复制别的文本时误弹</em>
      </span>
    </label>

    <h3>浏览器扩展一键推送</h3>
    <label class="check">
      <input v-model="s.browserPush" type="checkbox" />
      <span>
        启用本机接收端口
        <em>允许浏览器扩展把当前页链接一键推给应用；只监听 127.0.0.1，且网页无法冒充扩展</em>
      </span>
    </label>
    <p class="note">
      应用会在 <code class="mono">127.0.0.1:19090</code> 上接收浏览器扩展发来的链接。
      配合配套的浏览器扩展（右键「用 yt-dlp 下载」）使用。
      取消勾选后应用不再监听该端口（重启生效）。
    </p>

    <h3>应用更新</h3>
    <label class="check">
      <input v-model="s.autoCheckAppUpdate" type="checkbox" />
      <span>启动时检查新版本</span>
    </label>
    <div class="row-inline test-row">
      <button class="btn sm" :disabled="appUpdateBusy" @click="checkAppUpdate">
        {{ appUpdateBusy && !appUpdateProgress ? '检查中…' : '检查更新' }}
      </button>
      <button
        v-if="appUpdate?.latest"
        class="btn sm primary"
        :disabled="appUpdateBusy"
        @click="installAppUpdate"
      >
        下载并安装 v{{ appUpdate.latest }}
      </button>
      <span v-if="appUpdateProgress" class="ver mono">
        {{ fmtBytes(appUpdateProgress.downloaded) }}
        <template v-if="appUpdateProgress.total">
          / {{ fmtBytes(appUpdateProgress.total) }}
        </template>
      </span>
    </div>
    <p v-if="appUpdate?.latest" class="note">
      有新版本（v{{ appUpdate.current }} → v{{ appUpdate.latest }}）。安装时应用会关闭，装好后自动重开；
      没下完的任务重开后会显示为「已暂停」，点继续即可接着下。
      <template v-if="appUpdate.notes"><br />{{ appUpdate.notes }}</template>
    </p>
    <p v-else-if="appUpdate" class="note">已是最新版本（v{{ appUpdate.current }}）。</p>
    <p v-if="appUpdateMsg" class="res bad">{{ appUpdateMsg }}</p>
  </section>
</template>
