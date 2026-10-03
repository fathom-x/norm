// `path` / `node:path` for the browser build: Node's POSIX implementation
// (path-browserify), resolving relative paths against process.cwd().
import path from "path-browserify"

export const {
  basename,
  delimiter,
  dirname,
  extname,
  format,
  isAbsolute,
  join,
  normalize,
  parse,
  relative,
  resolve,
  sep,
} = path
export const toNamespacedPath = (value: string) => value
export const matchesGlob = () => {
  throw new Error("path.matchesGlob is not available in the browser build")
}
export const posix = { ...path, toNamespacedPath, matchesGlob }
export const win32 = posix
export default Object.assign(posix, { posix, win32 })
