import type { Config } from "../config"
import type { SandboxProvider } from "../provider"
import { E2BProvider } from "./e2b"
import { LocalProvider } from "./local"

/** The provider SANDBOX_PROVIDER names, configured from `config`. */
export function createProvider(config: Config, log?: (message: string) => void): SandboxProvider {
  if (config.provider === "e2b") return new E2BProvider({ ...config.e2b, normOwalletEnv: config.normOwalletEnv, log })
  return new LocalProvider({ ...config.local, normOwalletEnv: config.normOwalletEnv })
}
