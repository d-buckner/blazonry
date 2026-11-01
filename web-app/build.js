/* SPDX-License-Identifier: AGPL-3.0-only */
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

// 3. PWA assets: manifest, icons, and the service worker. sw.js must be a
//    separate top-level file (its scope can't be inline), so it is emitted
//    alongside index.html and embedded via include_bytes! on the Rust side.
//    The build stamp makes every deploy produce new bytes => SW update.
const stamp = new Date().toISOString().replace(/[-:T]/g, '').slice(0, 14);
fs.mkdirSync(OUT, { recursive: true });
fs.copyFileSync(path.join(SRC, 'manifest.webmanifest'), path.join(OUT, 'manifest.webmanifest'));
for (const f of fs.readdirSync(path.join(SRC, 'icons'))) {
  fs.mkdirSync(path.join(OUT, 'icons'), { recursive: true });
  fs.copyFileSync(path.join(SRC, 'icons', f), path.join(OUT, 'icons', f));
}
const swSrc = fs.readFileSync(path.join(SRC, 'sw.js'), 'utf8');
fs.writeFileSync(path.join(OUT, 'sw.js'), swSrc.replace('__BUILD__', stamp));

// 4. assemble the single-file page
// Self-host the device font (TRMNL) as a base64 @font-face so the UI stays
// fully offline — mntr never makes a cloud round-trip for core function.
const fontB64 = fs
  .readFileSync(path.join(HERE, '..', 'assets', 'framework', 'fonts', 'TRMNL16-Bold.woff2'))
  .toString('base64');
const fontFace =
  `@font-face{font-family:'TRMNL';font-style:normal;font-weight:700;font-display:swap;` +
  `src:url(data:font/woff2;base64,${fontB64}) format('woff2')}`;
const css = fontFace + '\n' + fs.readFileSync(path.join(SRC, 'app.css'), 'utf8');
const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>mntr</title>
<meta name="theme-color" content="#1a1a1a">
<link rel="manifest" href="/manifest.webmanifest">
<link rel="icon" type="image/svg+xml" href="/icons/icon.svg">
<link rel="apple-touch-icon" href="/icons/icon-192.png">
<style>${css}</style>
</head>
<body>
<div id="app">${body}</div>
<script>${clientJs}</script>
<script>
if ('serviceWorker' in navigator) {
  navigator.serviceWorker.register('/sw.js').then((reg) => {
    reg.addEventListener('updatefound', () => {
      const nw = reg.installing;
      nw && nw.addEventListener('statechange', () => {
        if (nw.state === 'installed' && navigator.serviceWorker.controller) {
          window.dispatchEvent(new CustomEvent('mntr-update'));
        }
      });
    });
  }).catch(() => {});
  navigator.serviceWorker.addEventListener('message', (e) => {
    if (e.data === 'online' || e.data === 'offline') {
      window.dispatchEvent(new CustomEvent('mntr-net', { detail: e.data }));
    }
  });
}
</script>
</body>
</html>
`;

fs.writeFileSync(path.join(OUT, 'index.html'), html);
const kb = (n) => (n / 1024).toFixed(1) + ' KB';
console.log(`web/index.html: ${kb(html.length)} (shell ${kb(body.length)} + js ${kb(clientJs.length)})`);
