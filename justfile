# blazonry — local dev
#
#   just            build the web UI + icons, then serve
#   just <name>     see `just --list`

addr    := "0.0.0.0:2301"
release := "false"   # just release=true serve

# build everything and serve at http://localhost:2301
default: serve

# build web UI + icons, then run the server (Ctrl-C to stop)
serve: web icons
    @cargo run --quiet {{ if release == "true" { "--release" } else { "" } }} --bin blazonry -- serve --addr {{addr}}

# build the embedded web UI (Svelte shell → web/index.html)
web: deps
    @cd web-app && npm run build

# generate PWA icons from icon.svg (PNGs are gitignored, so they must exist locally)
icons:
    #!/usr/bin/env bash
    set -euo pipefail
    cd web/icons
    [ -f icon-192.png ] && [ -f icon-512.png ] && exit 0
    conv=$(command -v rsvg-convert || command -v magick || command -v convert || true)
    if [ -z "$conv" ]; then
        echo "icons: need rsvg-convert or ImageMagick to rasterize icon.svg" >&2
        exit 1
    fi
    "$conv" -w 192 -h 192 icon.svg -o icon-192.png 2>/dev/null \
        || magick -background none -size 192x192 icon.svg icon-192.png
    "$conv" -w 512 -h 512 icon.svg -o icon-512.png 2>/dev/null \
        || magick -background none -size 512x512 icon.svg icon-512.png

# install web deps if missing
deps:
    #!/usr/bin/env bash
    [ -d web-app/node_modules ] || (cd web-app && npm install)

# build the binary (just release=true build for a release build)
build: web icons
    @cargo build {{ if release == "true" { "--release" } else { "" } }} --bin blazonry

# stop a running server
stop:
    @pkill -f 'blazonry serve' && echo "stopped" || echo "not running"

# run the test suite
test: web icons
    @cargo test

# show what is listening on the port
status:
    @curl -s -o /dev/null -w "http://localhost:2301 → %{http_code}\n" http://127.0.0.1:2301/ || echo "not running"
