import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

// This runs in Node.js - Don't use client-side code here (browser APIs, JSX...)

const config: Config = {
  title: 'zawk',
  tagline: 'Modern AWK + stdlib + Rust: the AWK you know, with CSV, JSON, Parquet, databases, NATS and AI built in',
  favicon: 'img/logo.svg',

  future: {
    v4: true,
  },

  url: 'https://zawk.microservices.club',
  baseUrl: '/',

  organizationName: 'linux-china',
  projectName: 'zawk',

  onBrokenLinks: 'throw',

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  markdown: {
    // .md files are plain CommonMark (AWK code is full of braces), .mdx files are MDX
    format: 'detect',
    hooks: {
      onBrokenMarkdownLinks: 'throw',
    },
  },

  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: './sidebars.ts',
          editUrl: 'https://github.com/linux-china/zawk/tree/master/website/',
        },
        blog: {
          showReadingTime: true,
          feedOptions: {
            type: ['rss', 'atom'],
            xslt: true,
          },
          editUrl: 'https://github.com/linux-china/zawk/tree/master/website/',
          onInlineTags: 'warn',
          onInlineAuthors: 'warn',
          onUntruncatedBlogPosts: 'warn',
        },
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/zawk-overview.jpg',
    colorMode: {
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'zawk',
      logo: {
        alt: 'zawk Logo',
        src: 'img/logo.svg',
      },
      items: [
        {type: 'docSidebar', sidebarId: 'docsSidebar', position: 'left', label: 'Docs'},
        {to: '/docs/syntax/overview', label: 'Syntax', position: 'left'},
        {to: '/docs/stdlib/overview', label: 'Stdlib', position: 'left'},
        {to: '/docs/tutorials/database', label: 'Tutorials', position: 'left'},
        {to: '/docs/faq', label: 'FAQ', position: 'left'},
        {to: '/blog', label: 'Blog', position: 'left'},
        {
          href: 'https://github.com/linux-china/zawk',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Docs',
          items: [
            {label: 'Getting Started', to: '/docs/intro'},
            {label: 'New AWK Syntax', to: '/docs/syntax/overview'},
            {label: 'Standard Library', to: '/docs/stdlib/overview'},
            {label: 'FAQ', to: '/docs/faq'},
          ],
        },
        {
          title: 'Tutorials',
          items: [
            {label: 'Databases', to: '/docs/tutorials/database'},
            {label: 'NATS & MQTT', to: '/docs/tutorials/nats'},
            {label: 'AI Integration', to: '/docs/tutorials/ai'},
          ],
        },
        {
          title: 'More',
          items: [
            {label: 'Blog', to: '/blog'},
            {label: 'GitHub', href: 'https://github.com/linux-china/zawk'},
            {label: 'crates.io', href: 'https://crates.io/crates/zawk'},
            {label: 'Cheat Sheet', href: 'https://cheatography.com/linux-china/cheat-sheets/zawk/'},
          ],
        },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} linux-china. zawk is dual-licensed under MIT or Apache 2.0.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['awk', 'bash', 'sql', 'json', 'ini'],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
