#!/usr/bin/env bash
# Re-vendor trmnl-framework assets into assets/framework (brotli-compressed).
# Usage: FW=/path/to/framework/public scripts/vendor_fw_assets.sh
set -euo pipefail
FW="${FW:-/workspace/trmnl-framework/public}"
DST="$(cd "$(dirname "$0")/.." && pwd)/assets/framework"
rm -rf "$DST"; mkdir -p "$DST"
cp "$FW/css/latest/plugins.min.css.br" "$DST/css/latest/" 2>/dev/null || { mkdir -p "$DST/css/latest"; cp "$FW/css/latest/plugins.min.css.br" "$DST/css/latest/"; }
mkdir -p "$DST/js/latest" && cp "$FW/js/latest/plugins.min.js.br" "$DST/js/latest/"
for f in "$FW"/fonts/*.woff2 "$FW"/fonts/*.ttf; do
  case "$f" in *TRMNL*.ttf) continue;; esac
  cp "$f" "$DST/fonts/" 2>/dev/null || { mkdir -p "$DST/fonts"; cp "$f" "$DST/fonts/"; }
done
for d in "$FW"/images/*/; do
  case "$d" in *-preview/) continue;; esac
  mkdir -p "$DST/images/$(basename "$d")"
  cp "$d"* "$DST/images/$(basename "$d")/" 2>/dev/null || true
done
# Branding strip: TRMNL wordmark in recipe title bars becomes blank.
printf '<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>' > "$DST/images/plugins/trmnl--render.svg"
# Brotli-compress everything that isn't already .br
node -e '
const fs=require("fs"),zlib=require("zlib"),path=require("path");
function walk(d){for(const f of fs.readdirSync(d)){const p=path.join(d,f);
 if(fs.statSync(p).isDirectory())walk(p);
 else if(!p.endsWith(".br")){const raw=fs.readFileSync(p);const br=zlib.brotliCompressSync(raw,{params:{[zlib.constants.BROTLI_PARAM_QUALITY]:11}});
 if(br.length<raw.length){fs.writeFileSync(p+".br",br);fs.unlinkSync(p);}else fs.renameSync(p,p+".br");}}}
walk(process.argv[2]);' "$DST"
# Regenerate the embedded asset table.
{
  echo '//! Generated: brotli-compressed trmnl-framework assets (MIT, github.com/usetrmnl/framework).'
  echo '//! Regenerate with scripts/vendor_fw_assets.sh. Keys are URL paths (no leading slash).'
  echo ''
  echo 'pub static ASSETS: &[(&str, &[u8])] = &['
  (cd "$DST" && find . -type f | sed 's|^\./||' | sort) | while read -r rel; do
    printf '    ("%s", include_bytes!("../../assets/framework/%s")),\n' "$rel" "$rel"
  done
  echo '];'
} > "$(dirname "$0")/../src/browser/assets_data.rs"
echo "vendored $(find "$DST" -type f | wc -l) files, $(du -sh "$DST" | cut -f1)"
