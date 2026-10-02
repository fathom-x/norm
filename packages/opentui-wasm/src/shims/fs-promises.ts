// node:fs/promises for the browser (see fs.ts).
function enoent(path: unknown): Error {
  const error = new Error(`ENOENT: no such file or directory, '${String(path)}'`) as Error & { code: string }
  error.code = "ENOENT"
  return error
}
export async function readFile(path: unknown): Promise<never> {
  throw enoent(path)
}
export async function writeFile(): Promise<void> {}
export async function appendFile(): Promise<void> {}
export async function mkdir(): Promise<undefined> {
  return undefined
}
export async function mkdtemp(prefix: string): Promise<string> {
  return `${prefix}${Math.random().toString(36).slice(2, 8)}`
}
export async function readdir(): Promise<string[]> {
  return []
}
export async function stat(path: unknown): Promise<never> {
  throw enoent(path)
}
export async function access(path: unknown): Promise<never> {
  throw enoent(path)
}
export async function unlink(): Promise<void> {}
export async function rm(): Promise<void> {}
export async function realpath(path: string): Promise<string> {
  return path
}
export const promises = { readFile, writeFile, appendFile, mkdir, mkdtemp, readdir, stat, access, unlink, rm, realpath }
export default promises
