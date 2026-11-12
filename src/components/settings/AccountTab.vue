<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api, type ProbeResult } from '../../ipc'
import type { BrowserChoice, CookieProfile } from '../../types'
import { fmtTime } from '../../utils'
import { useSettingsContext } from './context'

const { s, importError, browseFile, revealPath } = useSettingsContext()

/* ───────────────────── Cookie（DESIGN §6）───────────────────── */

const profiles = ref<CookieProfile[]>([])
/** 每个 profile 的预检结果，按 id 存。 */
const profileTest = ref<Record<string, ProbeResult>>({})
const browserTest = ref<ProbeResult | null>(null)
const testing = ref(false)
const importing = ref(false)
const fileInput = ref<HTMLInputElement | null>(null)

/* ── 浏览器选择 ──
 *
 * 不写死下拉项：后端按 yt-dlp 自己的路径映射探测本机装了哪些浏览器、
 * 各有哪些 profile。同一个浏览器常有多套登录态（工作/个人），
 * 选错 profile 会得到一个「没登录」的假象。
 */
const browsers = ref<BrowserChoice[]>([])
const selBrowser = ref('firefox')
const selProfile = ref('')

const curBrowser = computed(() => browsers.value.find((b) => b.name === selBrowser.value))

/**
 * 选中 profile 的「上次使用」提示。
 *
 * 有用：用户常有多个 profile，选错那个会得到一个「没登录」的假象；
 * 显示上次使用时间能立刻看出选的是不是自己正在用的那个。
 */
const selProfileInfo = computed(() => {
  const p = curBrowser.value?.profiles.find((x) => x.id === selProfile.value)
  if (!p?.lastUsed) return null
  const days = Math.floor((Date.now() - p.lastUsed) / 86_400_000)
  const ago = days === 0 ? '今天' : days === 1 ? '昨天' : `${days} 天前`
  return `该配置文件最近使用：${fmtTime(p.lastUsed)}（${ago}）`
})

/** 拼 `--cookies-from-browser` 的值。规则与 Rust 侧 `build_browser_spec` 一致。 */
function buildSpec(browser: string, profile: string): string {
  const p = profile.trim()
  return p ? `${browser}:${p}` : browser
}

/** 从已保存的 spec 反解出浏览器与 profile。 */
function syncFromSpec() {
  const spec = (s.cookieBrowser || 'firefox').trim()
  const head = spec.split('::')[0] // 丢掉 ::CONTAINER
  const idx = head.indexOf(':')
  // 丢掉 +KEYRING
  const name = (idx >= 0 ? head.slice(0, idx) : head).split('+')[0]
  selBrowser.value = name || 'firefox'

  const explicit = idx >= 0 ? head.slice(idx + 1) : ''
  // 没写 profile 时，yt-dlp 自己会挑**最近用过**的那个。
  // 这里把它显式化：**界面显示的应当就是实际会用的**，
  // 否则用户看到一个空的 profile 会以为「没用任何 profile」。
  selProfile.value = explicit || curBrowser.value?.recommendedProfile || ''
  if (!explicit && selProfile.value) {
    syncToSpec()
  }
}

function syncToSpec() {
  s.cookieBrowser = buildSpec(selBrowser.value, selProfile.value)
}

/** 换浏览器时自动选它最近用过的 profile——几乎总是用户想要的那个。 */
function onBrowserChange(e: Event) {
  selBrowser.value = (e.target as HTMLSelectElement).value
  selProfile.value = curBrowser.value?.recommendedProfile ?? ''
  syncToSpec()
  browserTest.value = null
}

async function loadBrowsers() {
  try {
    browsers.value = await api.listBrowsers()
    syncFromSpec()
  } catch (e) {
    importError.value = `读取浏览器列表失败：${String(e)}`
  }
}

async function loadProfiles() {
  try {
    profiles.value = await api.listCookieProfiles()
  } catch (e) {
    importError.value = `读取 cookie 列表失败：${String(e)}`
  }
}

function pickFile() {
  fileInput.value?.click()
}

/**
 * 用原生 `<input type="file">` 读文件内容再交给后端校验。
 *
 * 这样不必引入 Tauri 的 dialog 插件；而**校验仍在后端做**——
 * 前端只负责把内容送过去，格式错误的行号提示由 `validate_netscape` 给出。
 */
async function onFileChosen(e: Event) {
  const input = e.target as HTMLInputElement
  const f = input.files?.[0]
  if (!f) return
  importing.value = true
  importError.value = null
  try {
    const text = await f.text()
    const p = await api.importCookieProfile(f.name.replace(/\.txt$/i, ''), text, f.name)
    await loadProfiles()
    s.cookieProfileId = p.id
    s.cookieMode = 'profile'
  } catch (err) {
    // 后端返回的错误已经带了行号和原因，原样展示
    importError.value = String(err)
  } finally {
    importing.value = false
    input.value = ''
  }
}

async function removeProfile(id: string) {
  try {
    await api.deleteCookieProfile(id)
    if (s.cookieProfileId === id) s.cookieProfileId = ''
    delete profileTest.value[id]
    await loadProfiles()
  } catch (e) {
    importError.value = `删除失败：${String(e)}`
  }
}

async function testProfile(id: string) {
  testing.value = true
  try {
    profileTest.value[id] = await api.testCookieProfile(id)
  } finally {
    testing.value = false
  }
}

/**
 * 浏览器预检。**必须真的跑一次**：实测 Chrome 与 Edge 在 Windows 上都失败，
 * 但原因不同（数据库被占用 vs DPAPI 解密失败），指引也不同（DESIGN §6.2）。
 */
async function testBrowser() {
  testing.value = true
  browserTest.value = null
  try {
    browserTest.value = await api.testCookieBrowser(s.cookieBrowser)
  } finally {
    testing.value = false
  }
}

onMounted(() => {
  void loadProfiles()
  void loadBrowsers()
})
</script>

<template>
  <section>
    <h3>Cookie</h3>
    <div class="seg">
      <button :class="{ on: s.cookieMode === 'none' }" @click="s.cookieMode = 'none'">不用</button>
      <button :class="{ on: s.cookieMode === 'profile' }" @click="s.cookieMode = 'profile'">
        已导入 ({{ profiles.length }})
      </button>
      <button :class="{ on: s.cookieMode === 'file' }" @click="s.cookieMode = 'file'">
        指定文件
      </button>
      <button :class="{ on: s.cookieMode === 'browser' }" @click="s.cookieMode = 'browser'">
        从浏览器
      </button>
    </div>

    <!-- 已导入的 profile：多账号场景（B站账号 A / YouTube 账号 B） -->
    <template v-if="s.cookieMode === 'profile'">
      <p v-if="profiles.length === 0" class="note">
        还没有导入任何 cookies.txt。点下面的按钮选一个文件——格式会在导入时校验，
        有问题会直接告诉你第几行不对。
      </p>
      <div v-else class="pl-list">
        <label
          v-for="p in profiles"
          :key="p.id"
          class="pl-row"
          :class="{ on: s.cookieProfileId === p.id }"
        >
          <input v-model="s.cookieProfileId" type="radio" :value="p.id" />
          <span class="pl-name">{{ p.name }}</span>
          <span class="pl-meta">{{ p.cookieCount }} 条 · {{ p.origin }}</span>
          <button class="btn ghost sm" :disabled="testing" @click.prevent="testProfile(p.id)">
            测试
          </button>
          <button class="btn ghost sm danger" @click.prevent="removeProfile(p.id)">
            删除
          </button>
        </label>
      </div>

      <div class="row-inline test-row">
        <button class="btn sm" :disabled="importing" @click="pickFile">
          {{ importing ? '导入中…' : '导入 cookies.txt' }}
        </button>
      </div>

      <p
        v-for="(r, id) in profileTest"
        :key="id"
        class="res"
        :class="r.ok ? 'ok' : 'bad'"
      >
        {{ r.summary }}
      </p>
    </template>

    <template v-else-if="s.cookieMode === 'file'">
      <div class="field">
        <span>cookies.txt 路径<em>Netscape 格式</em></span>
        <div class="path-row">
          <input v-model="s.cookieFile" class="mono" spellcheck="false" />
          <button class="btn sm" title="选择 cookies.txt" @click="browseFile('cookieFile')">
            浏览…
          </button>
          <button
            class="btn sm"
            :disabled="!s.cookieFile.trim()"
            title="在资源管理器中定位该文件"
            @click="revealPath(s.cookieFile)"
          >
            定位
          </button>
        </div>
      </div>
    </template>

    <template v-else-if="s.cookieMode === 'browser'">
      <label class="field inline">
        <span>浏览器<em>装了的排在前面</em></span>
        <select :value="selBrowser" @change="onBrowserChange">
          <option
            v-for="b in browsers"
            :key="b.name"
            :value="b.name"
            :disabled="!b.installed"
          >
            {{ b.label }}{{ b.installed ? '' : '（未安装）' }}
          </option>
        </select>
      </label>

      <!-- profile 选择：同一浏览器常有多套登录态 -->
      <label
        v-if="curBrowser?.supportsProfiles && (curBrowser?.profiles.length ?? 0) > 0"
        class="field inline"
      >
        <span>配置文件<em>按最近使用排序</em></span>
        <select v-model="selProfile" @change="syncToSpec">
          <option value="">（让 yt-dlp 自动挑）</option>
          <option v-for="p in curBrowser!.profiles" :key="p.id" :value="p.id">
            {{ p.label }}
            {{ p.id === curBrowser!.recommendedProfile ? ' · 最近用过' : '' }}
          </option>
        </select>
      </label>

      <p v-if="selProfileInfo" class="note">{{ selProfileInfo }}</p>

      <p v-if="curBrowser && !curBrowser.installed" class="note warn">
        本机没有检测到 {{ curBrowser.label }} 的 cookie 库。
      </p>
      <p
        v-else-if="curBrowser?.supportsProfiles && curBrowser.profiles.length === 0"
        class="note"
      >
        没找到 {{ curBrowser.label }} 的 cookie 库——可能还没用它登录过任何网站，
        或者装在了非默认位置。
      </p>

      <!--
        只在选中 Chromium 系时才提示。一直挂着那段 Windows 警告会把
        真正有用的信息淹没——用 Firefox 的人根本不需要看它。
        「是不是 Chromium 系」以后端下发的 chromium 字段为准，界面不猜名字。
      -->
      <p v-if="curBrowser?.chromium" class="note warn">
        Windows 上 Chromium 系（Chrome / Edge 等）把 cookie 库加密了，
        yt-dlp 目前解不开——<strong>关掉浏览器也没用</strong>。
        先点「测试能否读取」确认一下，读不到就换 Firefox。
      </p>

      <div class="row-inline test-row">
        <button class="btn sm" :disabled="testing" @click="testBrowser">
          {{ testing ? '测试中…' : '测试能否读取' }}
        </button>
      </div>
      <p v-if="browserTest" class="res" :class="browserTest.ok ? 'ok' : 'bad'">
        {{ browserTest.summary }}
      </p>
    </template>

    <p v-if="importError" class="res bad">{{ importError }}</p>

    <input
      ref="fileInput"
      class="hidden-file"
      type="file"
      accept=".txt,text/plain"
      @change="onFileChosen"
    />
  </section>
</template>
