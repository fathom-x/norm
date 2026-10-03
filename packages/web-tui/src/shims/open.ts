// The `open` package for the browser build: links open in a new tab. In the
// worker (no `window`) it does nothing.
export default async function open(target: string) {
  const scope = globalThis as { open?: (url: string, target?: string, features?: string) => unknown }
  scope.open?.(target, "_blank", "noopener")
  return undefined
}
