# blazonry — one build, two images
#
#   docker build -t blazonry .                      → runtime (default, ~6 MB)
#   docker build -t blazonry --target toolchain .   → toolchain (~950 MB)
#
# `runtime` is `FROM scratch` around the static musl server: the smallest thing
# that runs a display. It cannot author an extension, because there is no compiler
# in it and `ext_host` cannot `dlopen` a musl artifact.
#
# `toolchain` carries the server, the extension host, the MCP server, and the Rust
# toolchain, so one container runs the whole authoring loop: write, build, check,
# apply (ADR-0004). The loader rejects an artifact built by a different `rustc`, so
# the builder and the host cannot be separate images that drift, and the compiler
# has to live where the build happens.
#
# The toolchain image is glibc, not musl: `ext_host` dlopens, and a plugin's libc
# must match the host that loads it. That is why its build is native per platform
# while the scratch build stays a musl cross-compile.
#
# ~950 MB is the toolchain, not the distro. `rust:1.99.1-slim-bookworm` measures
# 916 MB and `rust:1.99.1-alpine` measures 1.04 GB, so a smaller base buys nothing.
#
# Multi-arch (recommended for releases — TRMNL users love Raspberry Pis):
#   docker buildx build --platform linux/amd64,linux/arm64 -t blazonry .
#   docker buildx build --platform linux/amd64,linux/arm64 -t blazonry:toolchain \
#     --target toolchain .
#
# Run:
#   docker run -d --name blazonry -p 2301:2301 -v blazonry-data:/data blazonry
#   # admin UI: http://localhost:2301/  (device API on the same port)
#
# The MCP server sits next to the server in both images, so an agent on the host
# authors through the running container:
#   docker exec -i -e BLAZONRY_ADMIN_TOKEN "$container" blazonry_mcp
# Or over HTTP without a shell: POST /admin/mcp.

FROM --platform=$BUILDPLATFORM rust:1.99.1-bookworm AS builder-musl
ARG TARGETPLATFORM
# cmake+perl: aws-lc-sys (rustls backend). xz-utils: the toolchains are .tar.xz.
RUN apt-get update && apt-get install -y --no-install-recommends \
      cmake perl ca-certificates curl xz-utils \
    && rm -rf /var/lib/apt/lists/*
# buildx runs this stage once per target platform, so fetch only that toolchain.
# Debian's musl-tools is x86_64-only, hence a cross toolchain for arm64.
# Hosted on toolchains.bootlin.com: GitHub runners cannot reach musl.cc at all.
RUN set -e; \
    case "$TARGETPLATFORM" in \
      linux/amd64) DIR=x86-64;  TC=x86-64--musl--stable-2026.08-1 ;; \
      linux/arm64) DIR=aarch64; TC=aarch64--musl--stable-2026.08-1 ;; \
      *) echo "unsupported platform: $TARGETPLATFORM" >&2; exit 1 ;; \
    esac; \
    curl -fsSL --connect-timeout 15 --retry 5 --retry-delay 5 --retry-all-errors \
      -o /tc.xz \
      "https://toolchains.bootlin.com/downloads/releases/toolchains/$DIR/tarballs/$TC.tar.xz"; \
    tar xJf /tc.xz -C /opt; \
    rm /tc.xz; \
    ln -s /opt/$TC/bin/* /usr/local/bin/
RUN rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-buildroot-linux-musl-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-buildroot-linux-musl-gcc
WORKDIR /app
# only what the build actually reads (include_str!/include_bytes! targets + manifests)
COPY Cargo.toml Cargo.lock ./
COPY ext-api ./ext-api
COPY src ./src
COPY assets ./assets
COPY web ./web
RUN set -e; \
    case "$TARGETPLATFORM" in \
      linux/amd64) T=x86_64-unknown-linux-musl ;; \
      linux/arm64) T=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported platform: $TARGETPLATFORM" >&2; exit 1 ;; \
    esac; \
    cargo build --release --target "$T"; \
    cp "target/$T/release/blazonry" /blazonry; \
    cp "target/$T/release/blazonry_mcp" /blazonry_mcp

# The toolchain image needs a glibc build, and it cannot reuse the musl one.
FROM --platform=$TARGETPLATFORM rust:1.99.1-bookworm AS builder-gnu
# cmake and perl: aws-lc-sys, the rustls backend. rust:bookworm is buildpack-deps,
# so make and gcc are already here, which is also what links a cdylib later.
RUN apt-get update && apt-get install -y --no-install-recommends \
      cmake perl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY ext-api ./ext-api
COPY src ./src
COPY assets ./assets
COPY web ./web
# The default target, not musl: ext_host dlopens, and the plugins it builds must
# come from this toolchain against this libc.
RUN cargo build --release --bin ext_host

FROM --platform=$TARGETPLATFORM rust:1.99.1-slim-bookworm AS toolchain
# The toolchain stays here because `ext_host build` runs cargo in this container.
# rust:slim carries gcc and libc6-dev, which is what linking a cdylib needs, so the
# build job adds no packages of its own.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder-musl /blazonry /usr/local/bin/blazonry
COPY --from=builder-musl /blazonry_mcp /usr/local/bin/blazonry_mcp
COPY --from=builder-gnu /app/target/release/ext_host /usr/local/bin/ext_host
# The builder points cargo at this source with `--config
# patch.crates-io.blazonry-ext-api.path`, so an extension folder can declare
# blazonry-ext-api by version and build wherever it lands in the volume.
COPY ext-api /opt/ext-api
# SQLite DB, rendered screens, extension folders and their builds, and the socket
# the server and the host share.
VOLUME /data
RUN mkdir -p /data/extensions /data/run
ENV RUST_LOG=info
ENV BLAZONRY_EXT_API=/opt/ext-api
EXPOSE 2301
# This tag is the opt-in to authoring (ADR-0003, ADR-0004): the server starts the
# host, supervises it, and takes it down with itself. Pass a different CMD to run
# the server without it.
ENTRYPOINT ["/usr/local/bin/blazonry", "serve", "--db", "/data/blazonry.db", "--addr", "0.0.0.0:2301"]
CMD ["--ext-host", "/data/run/exthost.sock", "--ext-host-spawn", "/usr/local/bin/ext_host"]

# The default stage: `docker build -t blazonry .` lands here.
FROM scratch
COPY --from=builder-musl /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder-musl /blazonry /blazonry
COPY --from=builder-musl /blazonry_mcp /blazonry_mcp
# SQLite DB + rendered screens live here — mount a volume at /data
VOLUME /data
ENV RUST_LOG=info
EXPOSE 2301
ENTRYPOINT ["/blazonry", "serve", "--db", "/data/blazonry.db", "--addr", "0.0.0.0:2301"]
