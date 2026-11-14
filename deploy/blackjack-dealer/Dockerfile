# The blackjack dealer daemon (services/blackjack/dealer) for Fly.io: one machine, no public ports,
# its key, secret and seed store on the volume at /data (fly.toml, entrypoint.sh). The build context
# is the repository's root, with Dockerfile.dockerignore beside this file as its ignore file. The
# base image is the one the release builds the wallet in, pinned by digest (deploy/bots/Dockerfile,
# scripts/reproduce-release.sh WALLET_IMAGE).
FROM node:22.21.1-bookworm@sha256:b84d52cd45bfe261096ccbf886955d431b8b9ed01b72eaef588e8886bda09e78 AS build
WORKDIR /src
COPY package.json package-lock.json ./
# npm ci resolves every workspace's manifest; only these three packages' sources are needed.
COPY sdk-ts/package.json sdk-ts/package.json
COPY web/package.json web/package.json
COPY mcp/package.json mcp/package.json
COPY services/blackjack/core/package.json services/blackjack/core/package.json
COPY services/blackjack/dealer/package.json services/blackjack/dealer/package.json
COPY services/blackjack/render/package.json services/blackjack/render/package.json
COPY services/blackjack/xbot/package.json services/blackjack/xbot/package.json
# The locked dependencies of the three packages (with TypeScript, to build them); no install scripts run.
RUN npm ci --ignore-scripts --no-audit --no-fund -w @blockula/sdk -w @blockula/blackjack -w @blockula/blackjack-dealer
COPY sdk-ts/ sdk-ts/
COPY services/blackjack/core/ services/blackjack/core/
COPY services/blackjack/dealer/ services/blackjack/dealer/
RUN npm run build -w @blockula/sdk && npm run build -w @blockula/blackjack && npm run build -w @blockula/blackjack-dealer
# What runs: the manifests, the locked production dependencies and the built JS. No sources, no TypeScript.
RUN rm -rf node_modules \
 && npm ci --omit=dev --ignore-scripts --no-audit --no-fund -w @blockula/sdk -w @blockula/blackjack -w @blockula/blackjack-dealer \
 && mkdir -p /out/sdk-ts /out/services/blackjack/core /out/services/blackjack/dealer \
 && cp package.json package-lock.json /out/ \
 && cp -R node_modules /out/node_modules \
 && cp -R sdk-ts/package.json sdk-ts/dist /out/sdk-ts/ \
 && cp -R services/blackjack/core/package.json services/blackjack/core/dist /out/services/blackjack/core/ \
 && cp -R services/blackjack/dealer/package.json services/blackjack/dealer/dist /out/services/blackjack/dealer/

FROM node:22.21.1-bookworm@sha256:b84d52cd45bfe261096ccbf886955d431b8b9ed01b72eaef588e8886bda09e78
ENV NODE_ENV=production
WORKDIR /app
COPY --from=build /out/ /app/
COPY deploy/blackjack-dealer/entrypoint.sh /entrypoint.sh
# The entrypoint starts as root only to fix the volume's ownership and to move the first boot's
# secrets into files, then drops to `node` and runs the dealer.
ENTRYPOINT ["/entrypoint.sh"]
