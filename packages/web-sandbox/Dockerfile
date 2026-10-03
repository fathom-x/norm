# The demo service: one origin serving the landing page, /sandbox/ (norm in
# an E2B sandbox, streamed) and /browser/ (norm compiled for the browser).
#
# Build from the repo root after staging the static files:
#   packages/web-sandbox/scripts/stage-demo.sh
#   docker build -f packages/web-sandbox/Dockerfile -t norm-demo .
# Run: E2B_API_KEY and SESSION_SECRET (>= 32 chars) are required.
FROM oven/bun:1.3.14-slim AS base

WORKDIR /app

ENV NODE_ENV=production
ENV BUN_RUNTIME_TRANSPILER_CACHE_PATH=0

FROM base AS pruner

COPY . .

RUN bunx turbo@2.8.13 prune @opencode-ai/web-sandbox --docker --no-update-notifier --no-color

FROM base AS installer

COPY --from=pruner /app/out/json/ ./

# As packages/stats/server/Dockerfile: Bun 1.3.x needs the pruned workspace
# globs and lockfile metadata refreshed before the frozen production install.
RUN bun -e 'const packageJson = await Bun.file("package.json").json(); packageJson.workspaces.packages = Array.from(new Bun.Glob("packages/**/package.json").scanSync(".")).map((file) => file.slice(0, -"/package.json".length)).sort(); await Bun.write("package.json", JSON.stringify(packageJson, null, 2) + "\n")'
RUN rm -f bun.lock && bun install --filter @opencode-ai/web-sandbox --lockfile-only --ignore-scripts
RUN bun install --filter @opencode-ai/web-sandbox --frozen-lockfile --production --ignore-scripts

FROM base AS runner

COPY --from=installer /app ./
COPY --from=pruner /app/out/full/ ./
COPY .demo/ /app/demo/

WORKDIR /app/packages/web-sandbox

ENV SANDBOX_PROVIDER=e2b \
    HOST=0.0.0.0 \
    PORT=8080 \
    WEB_DIST=/app/demo/web \
    BROWSER_DIST=/app/demo/browser \
    COOKIE_SECURE=1

EXPOSE 8080

USER bun

CMD ["bun", "server/main.ts"]
