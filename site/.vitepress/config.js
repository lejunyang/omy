// omy 文档站配置。
//
// 语言选择：zh-CN 作为 root（项目的设计文档、代码注释、CLI 输出都是中文，
// 中文用户是主要读者），英文放在 /en/ 下。这样中文读者进站不用先跳转，
// 而 VitePress 的 root locale 也不会在 URL 里多一层前缀。
//
// 侧边栏和导航必须按语言各写一份：只写一份会让另一种语言的读者看到
// 中文栏目名却点进英文页面。

import { defineConfig } from 'vitepress'
import { zh } from './locales/zh.js'
import { en } from './locales/en.js'

export default defineConfig({
  // 部署到 GitHub Pages 的项目页时路径是 /omy/，不是根路径。
  // 写死成 '/' 会让所有 CSS/JS 的绝对路径 404，页面出来是无样式的纯文本。
  base: '/omy/',

  // 站点标题与描述由各 locale 覆盖，这里只放兜底值
  title: 'omy',
  description: '加密之后依然能直接用的文件管理工具',

  // 产物目录。默认是 .vitepress/dist，提到 site/dist 便于流水线直接取
  outDir: './dist',
  cacheDir: './.vitepress/cache',

  // 干净 URL：/guide/install 而不是 /guide/install.html
  cleanUrls: true,

  // 死链直接让构建失败。文档站最常见的退化就是改了文件名忘了改链接，
  // 而这种错误在本地点一遍不一定能发现
  ignoreDeadLinks: false,

  lastUpdated: true,

  head: [
    ['link', { rel: 'icon', href: '/omy/favicon.svg', type: 'image/svg+xml' }],
    ['meta', { name: 'theme-color', content: '#5b8def' }],
  ],

  locales: {
    root: zh,
    en: en,
  },

  themeConfig: {
    // 跨语言共享的配置放这里，语言相关的一律进 locales
    logo: '/logo.svg',
    socialLinks: [{ icon: 'github', link: 'https://github.com/lejunyang/omy' }],
    search: {
      provider: 'local',
      options: {
        locales: {
          root: {
            translations: {
              button: { buttonText: '搜索文档', buttonAriaLabel: '搜索文档' },
              modal: {
                noResultsText: '无法找到相关结果',
                resetButtonTitle: '清除查询条件',
                footer: { selectText: '选择', navigateText: '切换', closeText: '关闭' },
              },
            },
          },
        },
      },
    },
  },
})
