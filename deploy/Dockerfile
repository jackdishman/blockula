# Blockula node image. Fly builds it remotely (x86_64), so nothing is cross-compiled locally.
# Base images are pinned by digest and the rust tag matches rust-toolchain.toml
# (scripts/check-pins.sh). Bump both deliberately: `docker buildx imagetools inspect <image:tag>`.
FROM rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build
WORKDIR /src
COPY rust-toolchain.toml ./
COPY node/ node/
ARG GIT_SHA=unknown
ENV BLOCKULA_GIT_SHA=$GIT_SHA
RUN cargo build --release --locked --manifest-path node/Cargo.toml && cp node/target/release/blockula /blockula

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data --shell /usr/sbin/nologin blockula
COPY --from=build /blockula /usr/local/bin/blockula
COPY deploy/entrypoint.sh /entrypoint.sh
# The entrypoint starts as root only to fix volume ownership, then drops to the `blockula` user.
ENTRYPOINT ["/entrypoint.sh"]
