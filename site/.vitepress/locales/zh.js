// 中文站配置（root locale，URL 无语言前缀）。
//
// 侧边栏与导航是「文档有哪些页」的唯一声明处，新增页面时这里和
// en.js 两边都要加——只加一边会让另一种语言少一个入口，而构建不会报错。

export const zh = {
  label: '简体中文',
  lang: 'zh-CN',
  title: 'omy',
  description: '加密之后依然能直接用的文件管理工具',

  themeConfig: {
    nav: [
      { text: '指南', link: '/guide/what-is-omy', activeMatch: '/guide/' },
      { text: '命令参考', link: '/reference/cli', activeMatch: '/reference/' },
      {
        text: '设计文档',
        link: 'https://github.com/lejunyang/omy/tree/main/docs/research',
      },
    ],

    sidebar: {
      '/guide/': [
        {
          text: '开始',
          items: [
            { text: 'omy 是什么', link: '/guide/what-is-omy' },
            { text: '安装', link: '/guide/install' },
            { text: '快速上手', link: '/guide/getting-started' },
          ],
        },
        {
          text: '使用',
          items: [
            { text: '加密与解密', link: '/guide/encrypt-decrypt' },
            { text: '密码与密钥槽', link: '/guide/passwords' },
            { text: '密码管理器同步密钥', link: '/guide/password-managers' },
            { text: '媒体预览与播放', link: '/guide/media' },
            { text: '分片', link: '/guide/sharding' },
            { text: '局域网共享', link: '/guide/lan-sharing' },
            { text: '远程位置（云盘 / WebDAV / Telegram）', link: '/guide/remote-locations' },
            { text: '图形界面', link: '/guide/gui' },
          ],
        },
        {
          text: '进阶',
          items: [
            { text: '配置文件', link: '/guide/configuration' },
            { text: '安全边界', link: '/guide/security' },
            { text: '从源码构建', link: '/guide/building' },
          ],
        },
      ],
      '/reference/': [
        {
          text: '命令参考',
          items: [
            { text: '命令总览', link: '/reference/cli' },
            { text: '退出码', link: '/reference/exit-codes' },
          ],
        },
      ],
    },

    outline: { label: '页面导航', level: [2, 3] },
    docFooter: { prev: '上一页', next: '下一页' },
    lastUpdated: { text: '最后更新于' },
    returnToTopLabel: '回到顶部',
    sidebarMenuLabel: '菜单',
    darkModeSwitchLabel: '主题',
    lightModeSwitchTitle: '切换到浅色模式',
    darkModeSwitchTitle: '切换到深色模式',
    langMenuLabel: '切换语言',

    editLink: {
      pattern: 'https://github.com/lejunyang/omy/edit/main/site/:path',
      text: '在 GitHub 上编辑此页',
    },

    footer: {
      message:
        'omy-core / omy-net 以 MIT OR Apache-2.0 发布，omy-media 为 LGPL-2.1+，omy-cli / omy-gui 为 GPL-3.0+。',
      copyright: 'Copyright © 2026 omy contributors',
    },
  },
}
