import { describe, expect, test } from "bun:test"
import { ConfigError, loadConfig } from "../../server/config"

const SECRET = "s".repeat(32)

describe("config", () => {
  test("defaults: local provider, random secret", () => {
    const config = loadConfig({})
    expect(config.provider).toBe("local")
    expect(config.port).toBe(4330)
    expect(config.host).toBe("127.0.0.1")
    expect(config.ephemeralSecret).toBe(true)
    expect(config.sessionSecret.length).toBeGreaterThanOrEqual(32)
    expect(config.pauseGraceMs).toBe(60_000)
    expect(config.createsPerIpPerHour).toBe(5)
    expect(config.trustProxy).toBe(0)
    expect(config.local.command).toBe("norm-demo")
    expect(config.local.args).toEqual([])
    expect(config.e2b.template).toBe("norm-demo")
    expect(config.e2b.overpayHosts).toEqual(["overpay-eykm.onrender.com"])
    expect(config.normOwalletEnv).toBe("staging")
    expect(config.browserDist).toBeUndefined()
  })

  test("parses the knobs", () => {
    const config = loadConfig({
      PORT: "8080",
      SANDBOX_COMMAND: "bash",
      SANDBOX_ARGS: '["-c","echo hi"]',
      SANDBOX_ENV: '{"A":"1"}',
      SANDBOX_PASS_ENV: "PATH, LANG",
      ALLOWED_ORIGINS: "https://demo.example/, http://localhost:3000",
      TRUST_PROXY: "true",
      MAX_SANDBOXES: "3",
      CREATES_PER_IP_PER_HOUR: "2",
      PAUSE_GRACE_MS: "0",
      OVERPAY_HOSTS: "overpay.com,10.0.0.0/8",
      E2B_PTY_COMMAND: "",
      BROWSER_DIST: "/srv/browser",
    })
    expect(config.port).toBe(8080)
    expect(config.local.command).toBe("bash")
    expect(config.local.args).toEqual(["-c", "echo hi"])
    expect(config.local.env).toEqual({ A: "1" })
    expect(config.local.passEnv).toEqual(["PATH", "LANG"])
    expect(config.allowedOrigins).toEqual(["https://demo.example", "http://localhost:3000"])
    expect(config.trustProxy).toBe(1)
    expect(config.maxSandboxes).toBe(3)
    expect(config.createsPerIpPerHour).toBe(2)
    expect(config.pauseGraceMs).toBe(0)
    expect(config.e2b.overpayHosts).toEqual(["overpay.com", "10.0.0.0/8"])
    expect(config.e2b.ptyCommand).toBe("")
    expect(config.browserDist).toBe("/srv/browser")
    expect(loadConfig({ TRUST_PROXY: "2" }).trustProxy).toBe(2)
  })

  test("e2b needs an API key and a session secret", () => {
    expect(() => loadConfig({ SANDBOX_PROVIDER: "e2b", SESSION_SECRET: SECRET })).toThrow(/E2B_API_KEY/)
    expect(() => loadConfig({ SANDBOX_PROVIDER: "e2b", E2B_API_KEY: "e2b_x" })).toThrow(/SESSION_SECRET/)
    const config = loadConfig({ SANDBOX_PROVIDER: "e2b", E2B_API_KEY: "e2b_x", SESSION_SECRET: SECRET })
    expect(config.provider).toBe("e2b")
    expect(config.ephemeralSecret).toBe(false)
  })

  test("production needs a session secret", () => {
    expect(() => loadConfig({ NODE_ENV: "production" })).toThrow(ConfigError)
    expect(loadConfig({ NODE_ENV: "production", SESSION_SECRET: SECRET }).production).toBe(true)
  })

  test("refuses bad values", () => {
    for (const env of [
      { SANDBOX_PROVIDER: "docker" },
      { PORT: "http" },
      { PORT: "70000" },
      { MAX_SANDBOXES: "0" },
      { SESSION_SECRET: "short" },
      { SANDBOX_ARGS: "-c echo" },
      { SANDBOX_ARGS: '[1,2]' },
      { SANDBOX_ENV: '["A"]' },
      { ALLOWED_ORIGINS: "not a url" },
      { OVERPAY_HOSTS: "https://overpay.com/path" },
      { E2B_TIMEOUT_MS: "1000" },
    ])
      expect(() => loadConfig(env)).toThrow(ConfigError)
  })
})
