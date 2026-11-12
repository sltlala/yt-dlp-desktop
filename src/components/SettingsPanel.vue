<script setup lang="ts">
import { ref } from 'vue'
import { useTaskStore } from '../stores/tasks'
import type { SettingsTab } from '../types'
import { deepClone } from '../utils'
import AccountTab from './settings/AccountTab.vue'
import DownloadTab from './settings/DownloadTab.vue'
import FormatTab from './settings/FormatTab.vue'
import GeneralTab from './settings/GeneralTab.vue'
import NetworkTab from './settings/NetworkTab.vue'
import { provideSettingsContext } from './settings/context'
import './settings/settings.css'

const props = defineProps<{ initialTab?: SettingsTab }>()
const emit = defineEmits<{ close: [] }>()
const store = useTaskStore()

// store.settings 是 Vue Proxy，必须用 deepClone 而非 structuredClone。
const { s, importError } = provideSettingsContext(deepClone(store.settings!))
const tab = ref<SettingsTab>(props.initialTab ?? 'general')
const saved = ref(false)

async function save() {
  await store.saveSettings(deepClone(s))
  saved.value = true
  setTimeout(() => (saved.value = false), 1600)
}

/** 丢弃未保存的改动。设置现在是主区页面，用户可能改了一半就想退出去。 */
function revert() {
  Object.assign(s, deepClone(store.settings!))
  importError.value = null
}
</script>

<template>
  <!--
    设置是**主区的一个页面**，不是右侧抽屉（DESIGN §9.2）。
    因此这里没有遮罩、没有 fixed 定位——宽度由外层 `.main` 决定。
  -->
  <div class="settings">
    <header class="page-head">
      <div class="head-text">
        <h1>设置</h1>
        <p>保存后只影响<strong>之后</strong>的探测与下载，已在跑的任务不受影响。</p>
      </div>
    </header>

    <nav class="tabs">
      <button :class="{ on: tab === 'general' }" @click="tab = 'general'">常规</button>
      <button :class="{ on: tab === 'format' }" @click="tab = 'format'">格式</button>
      <button :class="{ on: tab === 'download' }" @click="tab = 'download'">下载器</button>
      <button :class="{ on: tab === 'network' }" @click="tab = 'network'">网络</button>
      <button :class="{ on: tab === 'account' }" @click="tab = 'account'">账号</button>
    </nav>

    <div class="body">
      <!-- v-show 而不是 v-if：切页不重新加载，预检结果之类的也不会丢 -->
      <div class="body-inner">
        <GeneralTab v-show="tab === 'general'" />
        <FormatTab v-show="tab === 'format'" />
        <DownloadTab v-show="tab === 'download'" />
        <NetworkTab v-show="tab === 'network'" />
        <AccountTab v-show="tab === 'account'" />
      </div>
    </div>

    <footer>
      <span v-if="saved" class="saved">已保存</span>
      <button class="btn" @click="emit('close')">返回任务</button>
      <button class="btn" @click="revert">还原改动</button>
      <button class="btn primary" @click="save">保存设置</button>
    </footer>
  </div>
</template>
