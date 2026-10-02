// node:fs for the browser: there is no filesystem. Reads fail with ENOENT,
// writes are dropped. Enough for @opentui/core's optional file features
// (debug dumps, file-backed text buffers) to fail soft.
function enoent(path: unknown): Error {
  const error = new Error(`ENOENT: no such file or directory, '${String(path)}'`) as Error & { code: string }
  error.code = "ENOENT"
  return error
}

export const constants = { F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1 }
export function existsSync(): boolean {
  return false
}
export function readFileSync(path: unknown): never {
  throw enoent(path)
}
export function writeFileSync(): void {}
export function appendFileSync(): void {}
export function mkdirSync(): undefined {
  return undefined
}
export function mkdtempSync(prefix: string): string {
  return `${prefix}${Math.random().toString(36).slice(2, 8)}`
}
export function rmSync(): void {}
export function unlinkSync(): void {}
export function symlinkSync(): void {}
export function statSync(path: unknown): never {
  throw enoent(path)
}
export function lstatSync(path: unknown): never {
  throw enoent(path)
}
export function realpathSync(path: string): string {
  return path
}
export function readdirSync(): string[] {
  return []
}
export function accessSync(path: unknown): never {
  throw enoent(path)
}
export function openSync(path: unknown): never {
  throw enoent(path)
}
export function closeSync(): void {}
export function writeSync(): number {
  return 0
}
export function createWriteStream() {
  return { write: () => true, end: () => {}, on: () => {}, once: () => {}, close: () => {} }
}
export function watch() {
  return { close: () => {}, on: () => {} }
}
export { promises } from "./fs-promises.js"
import * as promisesNs from "./fs-promises.js"

export default {
  constants,
  existsSync,
  readFileSync,
  writeFileSync,
  appendFileSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  unlinkSync,
  symlinkSync,
  statSync,
  lstatSync,
  realpathSync,
  readdirSync,
  accessSync,
  openSync,
  closeSync,
  writeSync,
  createWriteStream,
  watch,
  promises: promisesNs,
}
