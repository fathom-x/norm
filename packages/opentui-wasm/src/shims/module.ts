// node:module for the browser: there is no CommonJS loader.
export function createRequire(_from?: string | URL) {
  const require = (id: string) => {
    throw new Error(`require(${JSON.stringify(id)}) is not available in the browser`)
  }
  ;(require as any).resolve = (id: string) => id
  return require
}
export const builtinModules: string[] = []
export default { createRequire, builtinModules }
