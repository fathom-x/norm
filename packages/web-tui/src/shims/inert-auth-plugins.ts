// Auth plugins for providers norm does not offer (it ships Overpay only, see
// norm.ts `enforceProviders`). The real ones start local OAuth callback
// servers (fastify, node:http) at import time; in the browser they are inert.
import type { Plugin } from "@opencode-ai/plugin"

const inert: Plugin = async () => ({})

export const gitlabAuthPlugin = inert
export const PoeAuthPlugin = inert
export default inert
