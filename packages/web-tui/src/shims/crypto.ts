// `crypto` / `node:crypto` for the browser build: the synchronous slice
// opencode uses (ids, content hashes), on WebCrypto randomness and
// @noble/hashes. Ciphers, keys and signatures are not provided.
import { Buffer } from "buffer"
import { hmac } from "@noble/hashes/hmac.js"
import { md5, sha1 } from "@noble/hashes/legacy.js"
import { sha256, sha384, sha512 } from "@noble/hashes/sha2.js"

type Encoding = "hex" | "base64" | "base64url" | "latin1" | "binary" | "utf8"

const ALGORITHMS = { md5, sha1, sha256, sha384, sha512 } as const
type Algorithm = keyof typeof ALGORITHMS

function algorithm(name: string) {
  const key = name.toLowerCase().replace("-", "") as Algorithm
  const hash = ALGORITHMS[key]
  if (!hash) throw new Error(`crypto: digest "${name}" is not available in the browser build`)
  return hash
}

function bytes(data: string | ArrayBufferView, encoding?: Encoding) {
  if (typeof data === "string") return Buffer.from(data, encoding ?? "utf8")
  return new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
}

function digester(compute: (input: Uint8Array) => Uint8Array) {
  const chunks: Uint8Array[] = []
  const self = {
    update(data: string | ArrayBufferView, encoding?: Encoding) {
      chunks.push(bytes(data, encoding))
      return self
    },
    digest(encoding?: Encoding) {
      const out = Buffer.from(compute(Buffer.concat(chunks)))
      return encoding ? out.toString(encoding) : out
    },
  }
  return self
}

export function createHash(name: string) {
  const hash = algorithm(name)
  return digester((input) => hash(input))
}

export function createHmac(name: string, key: string | ArrayBufferView) {
  const hash = algorithm(name)
  return digester((input) => hmac(hash, bytes(key), input))
}

export function randomBytes(size: number, callback?: (error: Error | null, buf: Buffer) => void) {
  const buf = Buffer.from(globalThis.crypto.getRandomValues(new Uint8Array(size)))
  if (callback) queueMicrotask(() => callback(null, buf))
  return buf
}

export function randomInt(min: number, max?: number) {
  const [low, high] = max === undefined ? [0, min] : [min, max]
  const range = high - low
  const [value] = globalThis.crypto.getRandomValues(new Uint32Array(1))
  return low + (value % range)
}

export const randomUUID = () => globalThis.crypto.randomUUID()
export const getRandomValues = <T extends ArrayBufferView>(array: T) =>
  globalThis.crypto.getRandomValues(array as never) as T

export function timingSafeEqual(a: ArrayBufferView, b: ArrayBufferView) {
  const left = bytes(a)
  const right = bytes(b)
  if (left.length !== right.length) throw new RangeError("Input buffers must have the same byte length")
  return left.reduce((diff, value, index) => diff | (value ^ right[index]), 0) === 0
}

export const webcrypto = globalThis.crypto
export const subtle = globalThis.crypto?.subtle
export const getHashes = () => Object.keys(ALGORITHMS)

export default {
  createHash,
  createHmac,
  randomBytes,
  randomInt,
  randomUUID,
  getRandomValues,
  timingSafeEqual,
  webcrypto,
  subtle,
  getHashes,
}
