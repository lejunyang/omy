// English locale, served under /en/.
//
// The sidebar structure mirrors zh.js on purpose: adding a page means editing
// both files. Keeping the two in the same shape makes a missing entry obvious.

export const en = {
  label: 'English',
  lang: 'en',
  title: 'omy',
  description: 'Encrypted files you can still use directly',

  themeConfig: {
    nav: [
      { text: 'Guide', link: '/en/guide/what-is-omy', activeMatch: '/en/guide/' },
      { text: 'Reference', link: '/en/reference/cli', activeMatch: '/en/reference/' },
      {
        text: 'Design docs',
        link: 'https://github.com/lejunyang/omy/tree/main/docs/research',
      },
    ],

    sidebar: {
      '/en/guide/': [
        {
          text: 'Introduction',
          items: [
            { text: 'What is omy', link: '/en/guide/what-is-omy' },
            { text: 'Installation', link: '/en/guide/install' },
            { text: 'Getting started', link: '/en/guide/getting-started' },
          ],
        },
        {
          text: 'Usage',
          items: [
            { text: 'Encrypt and decrypt', link: '/en/guide/encrypt-decrypt' },
            { text: 'Passwords and key slots', link: '/en/guide/passwords' },
            { text: 'Password-manager sync keys', link: '/en/guide/password-managers' },
            { text: 'Media preview and playback', link: '/en/guide/media' },
            { text: 'Sharding', link: '/en/guide/sharding' },
            { text: 'LAN sharing', link: '/en/guide/lan-sharing' },
            { text: 'Remote locations (cloud / WebDAV / Telegram)', link: '/en/guide/remote-locations' },
            { text: 'Graphical interface', link: '/en/guide/gui' },
          ],
        },
        {
          text: 'Advanced',
          items: [
            { text: 'Configuration file', link: '/en/guide/configuration' },
            { text: 'Security boundaries', link: '/en/guide/security' },
            { text: 'Building from source', link: '/en/guide/building' },
          ],
        },
      ],
      '/en/reference/': [
        {
          text: 'Reference',
          items: [
            { text: 'Command overview', link: '/en/reference/cli' },
            { text: 'Exit codes', link: '/en/reference/exit-codes' },
          ],
        },
      ],
    },

    editLink: {
      pattern: 'https://github.com/lejunyang/omy/edit/main/site/:path',
      text: 'Edit this page on GitHub',
    },

    footer: {
      message:
        'omy-core / omy-net are MIT OR Apache-2.0, omy-media is LGPL-2.1+, omy-cli / omy-gui are GPL-3.0+.',
      copyright: 'Copyright © 2026 omy contributors',
    },
  },
}
