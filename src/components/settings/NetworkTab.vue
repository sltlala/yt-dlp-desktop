<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api, type ProbeResult } from '../../ipc'
import { ellipsizePath } from '../../utils'
import { useSettingsContext } from './context'

const { s, importError } = useSettingsContext()

/* ───────────────────── 代理 ───────────────────── */

const proxyProbe = ref<ProbeResult | null>(null)
const testingProxy = ref(false)

/** 系统代理快照，只在「跟随系统代理」时读取并展示。 */
const sysProxy = ref<{
  enabled: boolean
  server: string
  bypass: string
  autoConfigUrl: string
  resolved: string | null
  usesPac: boolean
} | null>(null)
const sysLoading = ref(false)

async function loadSystemProxy() {
  sysLoading.value = true
  try {
    sysProxy.value = await api.systemProxy()
  } catch (e) {
    importError.value = `读取系统代理失败：${String(e)}`
  } finally {
    sysLoading.value = false
  }
}

/**
 * 切换协议时顺便把端口带到该协议的惯例值。
 *
 * 只在端口**还是上一个协议的默认值**时才改——用户自己填过的端口不能被覆盖。
 */
function setProtocol(p: 'http' | 'socks5') {
  const defaults: Record<string, number[]> = { http: [80, 8080], socks5: [1080] }
  const others = Object.entries(defaults)
    .filter(([k]) => k !== p)
    .flatMap(([, v]) => v)
  const wasDefault = others.includes(Number(s.proxyPort)) || !s.proxyPort
  s.proxyProtocol = p
  if (wasDefault) s.proxyPort = p === 'socks5' ? 1080 : 8080
}

/**
 * 「实际传给 yt-dlp」的预览。
 *
 * ⚠️ **绝不能显示密码**——所以这里自己拼，不复用后端的完整 URL。
 */
const effectiveProxyLabel = computed(() => {
  const host = s.proxyHost.trim()
  if (!host) return '（还没填主机名）'
  const scheme = s.proxyProtocol === 'socks5' ? 'socks5h' : 'http'
  const auth = s.proxyAuth && s.proxyUser.trim() ? `${s.proxyUser.trim()}:***@` : ''
  return `${scheme}://${auth}${host}:${s.proxyPort}`
})

/** 真发一次请求验证——只校验格式的话，代理没启动也会显示「格式正确」。 */
async function testProxy() {
  testingProxy.value = true
  proxyProbe.value = null
  try {
    proxyProbe.value = await api.testProxy()
  } finally {
    testingProxy.value = false
  }
}

/* ───────────────────── 按站点分流（ROADMAP §F18）───────────────────── */

function addProxyRule() {
  if (!Array.isArray(s.proxyRules)) s.proxyRules = []
  s.proxyRules.push({ host: '', proxy: '' })
}

function removeProxyRule(i: number) {
  s.proxyRules.splice(i, 1)
}

// 选「跟随系统代理」时要把系统那份读出来展示，进页面就先取一次
onMounted(() => {
  if (s.proxyMode === 'system') void loadSystemProxy()
})
</script>

<template>
  <section>
    <h3>代理</h3>

    <!--
      结构照着 Windows「设置 → 网络和 Internet → 代理」那一页来：
      三选一（不用 / 跟随系统 / 手动），手动的展开协议、主机、端口、绕过、认证。
    -->
    <div class="radios">
      <label class="radio">
        <input v-model="s.proxyMode" type="radio" value="none" />
        <span>不使用代理</span>
      </label>

      <label class="radio">
        <input v-model="s.proxyMode" type="radio" value="system" />
        <span>跟随系统代理</span>
        <button class="btn sm" type="button" @click="loadSystemProxy">
          {{ sysLoading ? '读取中…' : '读取系统设置' }}
        </button>
      </label>
      <div v-if="s.proxyMode === 'system'" class="sub-block">
        <p v-if="sysProxy && sysProxy.resolved" class="sub-line">
          系统代理 <code class="mono">{{ sysProxy.resolved }}</code>
          <template v-if="sysProxy.bypass">
            <br />绕过 <code class="mono">{{ ellipsizePath(sysProxy.bypass, 76) }}</code>
          </template>
        </p>
        <p v-else class="sub-line warn-text">
          系统没有启用代理（注册表里 <code>ProxyEnable</code> 为 0）。
        </p>
        <!-- yt-dlp 不支持 PAC，必须如实说，否则用户会以为配了就能用 -->
        <p v-if="sysProxy?.usesPac" class="path-warn">
          检测到系统使用 PAC 自动配置（{{ sysProxy.autoConfigUrl }}）。
          yt-dlp 不支持 PAC，无法自动解析——请改用手动配置填写 PAC 解析出的地址。
        </p>
        <p v-else class="sub-line muted-text">
          每次探测/下载时重新读取，改了系统代理不用回来点一次。
        </p>
      </div>

      <label class="radio">
        <input v-model="s.proxyMode" type="radio" value="manual" />
        <span>手动配置</span>
      </label>
      <div v-if="s.proxyMode === 'manual'" class="sub-block">
        <div class="seg narrow">
          <button :class="{ on: s.proxyProtocol === 'http' }" @click="setProtocol('http')">
            HTTP
          </button>
          <button
            :class="{ on: s.proxyProtocol === 'socks5' }"
            @click="setProtocol('socks5')"
          >
            SOCKS5
          </button>
        </div>

        <div class="two-col">
          <label class="field">
            <span>主机名</span>
            <input v-model="s.proxyHost" class="mono" spellcheck="false" placeholder="127.0.0.1" />
          </label>
          <label class="field">
            <span>端口</span>
            <input v-model.number="s.proxyPort" type="number" min="1" max="65535" />
          </label>
        </div>

        <label class="check">
          <input v-model="s.proxyAuth" type="checkbox" />
          <span>代理身份验证<em>用户名密码会拼进代理地址</em></span>
        </label>
        <div v-if="s.proxyAuth" class="sub-block tight">
          <label class="field inline">
            <span>用户名</span>
            <input v-model="s.proxyUser" class="mono" spellcheck="false" />
          </label>
          <label class="field inline">
            <span>密码</span>
            <input v-model="s.proxyPassword" type="password" class="mono" spellcheck="false" />
          </label>
          <label class="check sub">
            <input v-model="s.proxyRemember" type="checkbox" />
            <span>记住密码<em>不勾则只保留在内存，重启后需要重填</em></span>
          </label>
        </div>

        <p class="sub-line">
          实际传给 yt-dlp：<code class="mono">{{ effectiveProxyLabel }}</code>
        </p>
      </div>
    </div>

    <div class="row-inline">
      <button
        class="btn sm"
        :disabled="s.proxyMode === 'none' || testingProxy"
        @click="testProxy"
      >
        {{ testingProxy ? '测试中…' : '检查连接' }}
      </button>
      <span class="ver">会真的发一次请求验证，不是只校验格式</span>
    </div>
    <p v-if="proxyProbe" class="res" :class="proxyProbe.ok ? 'ok' : 'bad'">
      {{ proxyProbe.summary }}
    </p>

    <h3>按站点分流</h3>
    <p class="note">
      为特定站点指定不同的代理（例如 B 站走 A 代理、YouTube 走 B）。
      规则按<strong>从上到下</strong>匹配，第一个命中的生效；留空 <code class="mono">proxy</code>
      或填 <code class="mono">direct</code> 表示该站点<strong>直连</strong>。主机名支持
      <code class="mono">*.example.com</code> 通配（也匹配裸域）。
    </p>
    <div v-if="s.proxyRules.length" class="proxy-rules">
      <div v-for="(r, i) in s.proxyRules" :key="i" class="proxy-rule">
        <input
          v-model="r.host"
          class="mono"
          spellcheck="false"
          placeholder="*.bilibili.com"
          title="站点主机名（支持 * 通配）"
        />
        <input
          v-model="r.proxy"
          class="mono"
          spellcheck="false"
          placeholder="socks5://127.0.0.1:1080 或 direct"
          title="代理 URL；留空或 direct = 直连"
        />
        <button class="btn sm ghost" title="删除这条规则" @click="removeProxyRule(i)">✕</button>
      </div>
    </div>
    <button class="btn sm" @click="addProxyRule">+ 添加规则</button>

    <h3>反爬拦截</h3>
    <label class="check">
      <input v-model="s.impersonate" type="checkbox" />
      <span>
        绕过 Cloudflare 拦截
        <em>开启浏览器指纹模拟（仅对通用提取器）</em>
      </span>
    </label>
    <p class="note">
      有些站点用 Cloudflare 挡机器人，会返回
      <strong>HTTP 403</strong> 并提示
      <code>try again with --extractor-args "generic:impersonate"</code>。
      勾上这项即可——它就是那个参数，只是不用自己去敲命令行。
    </p>
    <p class="note warn">
      默认关闭是因为 yt-dlp 自己也不默认开：强制模拟会<strong>拖慢速度、降低稳定性</strong>。
      所以遇到拦截再打开，别一直挂着。
    </p>
  </section>
</template>
