# Blockula mainnet node image: the signed release's own Linux binary, never a source build.
# deploy/mainnet/launch-mainnet.sh verifies the release (its signature against the pinned release
# key, checksums.txt's first line, the archive's SHA-256), then builds this with a context of
# exactly three files: that binary, the release's deploy/entrypoint.sh and this file. The build
# checks the binary's SHA-256 and version once more, so an image can't hold anything but what was
# verified. The base is the testnet image's runtime stage, pinned by the same digest
# (scripts/check-pins.sh, deploy/mainnet/test-launch-mainnet.sh), and no cargo feature is ever
# enabled for what ships (check-pins.sh).
FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
ARG BLOCKULA_SHA256
ARG BLOCKULA_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data --shell /usr/sbin/nologin blockula
COPY blockula /usr/local/bin/blockula
COPY entrypoint.sh /entrypoint.sh
RUN chmod 755 /usr/local/bin/blockula /entrypoint.sh \
 && test -n "$BLOCKULA_SHA256" && echo "$BLOCKULA_SHA256  /usr/local/bin/blockula" | sha256sum --strict -c - \
 && test -n "$BLOCKULA_VERSION" && test "$(blockula --version)" = "blockula $BLOCKULA_VERSION"
# The entrypoint starts as root only to fix volume ownership, then drops to the `blockula` user.
ENTRYPOINT ["/entrypoint.sh"]
