# mntr — TRMNL BYOS server (single static binary + SQLite)
#
# Multi-stage: the builder compiles a fully static musl binary (SQLite is
# bundled, rustls needs only the system CA bundle), so the runtime stage is
# `FROM scratch` — final image is ~7 MB compressed.
#
# Build (plain docker):
#   docker build -t mntr .
# Multi-arch (recommended for releases — TRMNL users love Raspberry Pis):
#   docker buildx build --platform linux/amd64,linux/arm64 -t mntr .
#
# Run:
#   docker run -d --name mntr -p 2301:2301 -v mntr-data:/data mntr
#   # admin UI: http://localhost:2301/  (device API on the same port)

FROM --platform=$BUILDPLATFORM rust:1.99.1-bookworm AS builder
ARG TARGETPLATFORM
# cmake+perl: aws-lc-sys (rustls backend). musl.cc cross toolchains: static
# musl linking for both amd64 and arm64 (Debian's musl-tools is x86_64-only).
RUN apt-get update && apt-get install -y --no-install-recommends \
      cmake perl ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && curl -sSL https://musl.cc/x86_64-linux-musl-cross.tgz | tar xz -C /opt \
    && curl -sSL https://musl.cc/aarch64-linux-musl-cross.tgz | tar xz -C /opt \
    && ln -s /opt/x86_64-linux-musl-cross/bin/* /usr/local/bin/ \
    && ln -s /opt/aarch64-linux-musl-cross/bin/* /usr/local/bin/
RUN rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc
WORKDIR /app
# only what the build actually reads (include_str!/include_bytes! targets + manifests)
COPY Cargo.toml Cargo.lock ./
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
    cp "target/$T/release/mntr" /mntr

FROM scratch
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /mntr /mntr
# SQLite DB + rendered screens live here — mount a volume at /data
VOLUME /data
ENV RUST_LOG=info
EXPOSE 2301
ENTRYPOINT ["/mntr", "serve", "--db", "/data/mntr.db", "--addr", "0.0.0.0:2301"]
