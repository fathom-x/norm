// Stand-in for Node built-ins @opentui/core imports only from code paths a
// browser never takes (node:http, node:vm, node:assert, bun:ffi, ...).
function unavailable(): never {
  throw new Error("This Node/Bun built-in is not available in the browser")
}
export const createServer = unavailable
export const runInNewContext = unavailable
export const dlopen = unavailable
export const ptr = unavailable
export const toArrayBuffer = unavailable
export const suffix = ""
export const FFIType = {}
export const JSCallback = unavailable
export default {}
