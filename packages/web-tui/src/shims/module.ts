// `module` for the browser build: there is no CommonJS loader at runtime.
export function createRequire(from: string | URL) {
  const require = (id: string): never => {
    throw new Error(`require("${id}") from ${String(from)} is not available in the browser build`)
  }
  return Object.assign(require, {
    resolve: (id: string): never => require(id),
    cache: {},
  })
}
export const builtinModules: string[] = []
export const isBuiltin = () => false
export default { createRequire, builtinModules, isBuiltin }
