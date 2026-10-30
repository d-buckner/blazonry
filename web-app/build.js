// Build: Svelte 5 -> one static HTML file (SSR-prerendered shell + inlined
// hydrating client bundle), written to ../web/index.html for include_str!.
//
//   node build.js
//
// Two esbuild passes through an inline svelte plugin:
//   1. client bundle  (generate: 'client', IIFE, minified)
//   2. server bundle  (generate: 'server', ESM) — imported here to prerender
// The JS is inlined into the HTML so the Rust binary embeds exactly one file.

import { build } from 'esbuild';
import { compile, compileModule, preprocess } from 'svelte/compiler';
import fs from 'node:fs';
import path from 'node:path';
import url from 'node:url';
import os from 'node:os';

const HERE = path.dirname(url.fileURLToPath(import.meta.url));
const SRC = path.join(HERE, 'src');
const OUT = path.join(HERE, '..', 'web');

function sveltePlugin({ generate }) {
  return {
    name: 'svelte',
    setup(b) {
      b.onResolve({ filter: /\.(svelte|svelte\.js)$/ }, (args) => ({
        path: path.resolve(args.resolveDir, args.path),
        namespace: 'svelte:' + generate,
      }));
      b.onLoad({ filter: /.*/, namespace: 'svelte:' + generate }, async (args) => {
        let source = fs.readFileSync(args.path, 'utf8');
        const filename = path.relative(HERE, args.path);
        let js;
        if (args.path.endsWith('.svelte.js')) {
          // runes-only JS module: compileModule, not compile
          js = compileModule(source, { filename }).js.code;
        } else {
          const pre = await preprocess(source, {}, { filename });
          js = compile(pre.code, {
            generate,
            filename,
            dev: false,
            hydratable: true,
          }).js.code;
        }
        return {
          contents: js,
          loader: 'js',
          resolveDir: path.dirname(args.path),
        };
      });
    },
  };
}

// 1. client bundle (inlined into the page)
const client = await build({
  entryPoints: [path.join(SRC, 'main.js')],
  bundle: true,
  minify: true,
  format: 'iife',
  target: 'es2022',
  write: false,
  plugins: [sveltePlugin({ generate: 'client' })],
});
const clientJs = client.outputFiles[0].text;

// 2. server bundle -> prerender the logged-out shell
const ssrEntry = path.join(SRC, 'ssr-entry.js');
const tmp = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'trmnl-ssr-')), 'ssr.mjs');
await build({
  entryPoints: [ssrEntry],
  bundle: true,
  format: 'esm',
  platform: 'node',
  target: 'node20',
  outfile: tmp,
  plugins: [sveltePlugin({ generate: 'server' })],
});
const { renderApp } = await import(url.pathToFileURL(tmp).href);
const { body } = renderApp();
fs.rmSync(path.dirname(tmp), { recursive: true, force: true });

// 3. assemble the single-file page
const css = fs.readFileSync(path.join(SRC, 'app.css'), 'utf8');
const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>mntr</title>
<style>${css}</style>
</head>
<body>
<div id="app">${body}</div>
<script>${clientJs}</script>
</body>
</html>
`;

fs.mkdirSync(OUT, { recursive: true });
fs.writeFileSync(path.join(OUT, 'index.html'), html);
const kb = (n) => (n / 1024).toFixed(1) + ' KB';
console.log(`web/index.html: ${kb(html.length)} (shell ${kb(body.length)} + js ${kb(clientJs.length)})`);
