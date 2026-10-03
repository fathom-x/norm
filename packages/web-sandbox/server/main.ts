// Entry point: `bun server/main.ts` (package script `start`). Configuration
// comes from the environment — see README.md ("Environment").
import { ConfigError, loadConfig } from "./config"
import { createServer } from "./server"

let config
try {
  config = loadConfig()
} catch (error) {
  if (!(error instanceof ConfigError)) throw error
  console.error(`web-sandbox: ${error.message}`)
  process.exit(1)
}

if (config.ephemeralSecret)
  console.warn("web-sandbox: SESSION_SECRET is not set; using a random one (sessions end when the server restarts)")

const demo = createServer(config)
console.log(`web-sandbox: ${config.provider} provider, listening on ${demo.url.href}`)

for (const signal of ["SIGINT", "SIGTERM"] as const)
  process.on(signal, () => {
    demo.stop().finally(() => process.exit(0))
  })
