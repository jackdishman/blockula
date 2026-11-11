# Blockula node image. Fly builds it remotely (x86_64), so nothing is cross-compiled locally.
# Base images are pinned by digest and the rust tag matches rust-toolchain.toml
# (scripts/check-pins.sh). Bump both deliberately: `docker buildx imagetools inspect <image:tag>`.
# The served wallet uses exactly the Node.js image used by the release wallet build
# (WALLET_IMAGE in scripts/reproduce-release.sh; scripts/check-pins.sh checks parity).
FROM node:22.21.1-bookworm@sha256:b84d52cd45bfe261096ccbf886955d431b8b9ed01b72eaef588e8886bda09e78 AS wallet
WORKDIR /src
COPY package.json package-lock.json ./
# npm ci resolves all three workspace manifests, but no MCP source is needed here.
COPY sdk-ts/package.json sdk-ts/package.json
COPY web/package.json web/package.json
COPY mcp/package.json mcp/package.json
RUN npm ci
COPY sdk-ts/ sdk-ts/
COPY web/ web/
COPY scripts/wallet-built.mjs scripts/wallet-built.mjs
# wallet-built.mjs touches this file so subsequent node builds embed the export.
COPY node/build.rs node/build.rs
ARG GIT_SHA=unknown
ENV BLOCKULA_GIT_SHA=$GIT_SHA NEXT_TELEMETRY_DISABLED=1
RUN npm run build:wallet

FROM rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build
WORKDIR /src
COPY rust-toolchain.toml ./
COPY node/ node/
COPY --from=wallet /src/web/out /wallet
ENV BLOCKULA_WALLET_DIR=/wallet
ARG GIT_SHA=unknown
ENV BLOCKULA_GIT_SHA=$GIT_SHA
RUN cargo build --release --locked --manifest-path node/Cargo.toml && cp node/target/release/blockula /blockula

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data --shell /usr/sbin/nologin blockula
COPY --from=build /blockula /usr/local/bin/blockula
COPY deploy/entrypoint.sh /entrypoint.sh
# The entrypoint starts as root only to fix volume ownership, then drops to the `blockula` user.
ENTRYPOINT ["/entrypoint.sh"]
