# The playground's house bots (deploy/host-bots.sh): examples/trick-or-treat-bot.mjs and
# examples/guestbook.mjs, on the SDK packed from the same checkout. The base image is the one the
# release builds the wallet in, pinned by digest (scripts/reproduce-release.sh WALLET_IMAGE).
FROM node:22.21.1-bookworm@sha256:b84d52cd45bfe261096ccbf886955d431b8b9ed01b72eaef588e8886bda09e78
WORKDIR /app
COPY sdk.tgz /app/sdk.tgz
# The SDK and its locked runtime dependencies only; no install scripts run.
RUN npm init -y >/dev/null && npm install --omit=dev --ignore-scripts --no-audit --no-fund ./sdk.tgz && rm sdk.tgz
COPY examples/trick-or-treat-bot.mjs examples/guestbook.mjs /app/examples/
COPY deploy/bots/entrypoint.sh /entrypoint.sh
# The entrypoint starts as root only to fix the volume's ownership, then drops to `node`.
ENTRYPOINT ["/entrypoint.sh"]
