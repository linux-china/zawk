# zawk website

The zawk site, built with [Docusaurus](https://docusaurus.io/) and TypeScript.

- `docs/`: getting started, new AWK syntax, standard library, tutorials, tooling, gawk differences and FAQ
- `blog/`: articles about zawk
- `src/pages/index.tsx`: the home page

## Development

```shell
npm install
npm start          # dev server with live reload
npm run typecheck
npm run build      # static site in build/
npm run serve      # serve the build locally
```

Docs `.md` files are plain CommonMark (`markdown.format: 'detect'`), so AWK braces need no escaping.
Examples in the docs were run against `zawk 0.6.6`; please keep them runnable when you edit them.
