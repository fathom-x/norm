// Node globals the core and its dependencies expect, installed before any of
// them is evaluated: imported first by both entries (core.worker.ts, main.ts).
import { Buffer } from "buffer"
import { ENV, WORKSPACE } from "../env"
import { process } from "./process"

Object.assign(process.env, ENV)
process.chdir(WORKSPACE)

const scope = globalThis as Record<string, unknown>
scope.process ??= process
scope.Buffer ??= Buffer
scope.global ??= globalThis
scope.setImmediate ??= (fn: (...args: unknown[]) => void, ...args: unknown[]) => setTimeout(fn, 0, ...args)
scope.clearImmediate ??= (id: number) => clearTimeout(id)
