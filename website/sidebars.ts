import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

// This runs in Node.js - Don't use client-side code here (browser APIs, JSX...)

const sidebars: SidebarsConfig = {
  docsSidebar: [
    'intro',
    {
      type: 'category',
      label: 'New AWK Syntax',
      collapsed: false,
      link: {type: 'doc', id: 'syntax/overview'},
      items: [
        'syntax/strings',
        'syntax/operators',
        'syntax/match',
        'syntax/control-flow',
        'syntax/structured-data',
      ],
    },
    {
      type: 'category',
      label: 'Standard Library',
      link: {type: 'doc', id: 'stdlib/overview'},
      items: [
        'stdlib/text',
        'stdlib/parsers',
        'stdlib/json-xml-html',
        'stdlib/array-math',
        'stdlib/datetime',
        'stdlib/crypto-encoding',
        'stdlib/network',
        'stdlib/database-kv',
        'stdlib/os-io',
      ],
    },
    {
      type: 'category',
      label: 'Tutorials',
      link: {type: 'generated-index', title: 'Tutorials', slug: '/tutorials'},
      items: [
        'tutorials/database',
        'tutorials/nats',
        'tutorials/kafka',
        'tutorials/ai',
        'tutorials/http-api',
        'tutorials/cloud-data',
        'tutorials/log-analysis',
        'tutorials/scripts-as-tools',
      ],
    },
    'tooling',
    'zawk-vs-gawk',
    'compatibility',
    'faq',
    'snippets',
    'zawk-cheat-sheet'
  ],
};

export default sidebars;
