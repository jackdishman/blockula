# A hosted chain's node image (deploy/host-chain.sh): the published release binary, which has the
# passkey wallet built in, not a source build. host-chain.sh verifies the release's build
# provenance before it passes VERSION and the archive's SHA256 here, and this build refuses any
# other bytes. Base images are pinned by digest (scripts/check-pins.sh).
FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3 AS fetch
ARG VERSION
ARG SHA256
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN test -n "$VERSION" && test -n "$SHA256" \
 && curl -fsSL -o /blockula.tar.gz "https://github.com/jackdishman/blockula/releases/download/$VERSION/blockula-x86_64-unknown-linux-gnu.tar.gz" \
 && echo "$SHA256  /blockula.tar.gz" | sha256sum -c - \
 && mkdir /out && tar -xzf /blockula.tar.gz -C /out \
 && find /out -type f -name blockula -exec install -m 755 {} /blockula \; \
 && /blockula --version

FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data --shell /usr/sbin/nologin blockula
COPY --from=fetch /blockula /usr/local/bin/blockula
COPY deploy/entrypoint.sh /entrypoint.sh
# The entrypoint starts as root only to fix volume ownership, then drops to the `blockula` user.
ENTRYPOINT ["/entrypoint.sh"]
