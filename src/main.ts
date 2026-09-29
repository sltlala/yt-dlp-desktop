import { createApp } from 'vue'
import { createPinia } from 'pinia'
import App from './App.vue'
import { installContextMenuGuard } from './contextMenu'
import './style.css'

// 在 mount 之前装：右键菜单与渲染无关，越早拦住越好
installContextMenuGuard()

createApp(App).use(createPinia()).mount('#app')
