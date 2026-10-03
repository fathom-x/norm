// `clipboardy` for the browser build: the async Clipboard API, best effort
// (it needs focus and, for reads, permission; failures resolve quietly the
// way the TUI already treats a missing clipboard tool).
const clipboard = () => (globalThis.navigator as Navigator | undefined)?.clipboard

export async function write(text: string) {
  await clipboard()?.writeText(text)
}

export async function read() {
  return (await clipboard()?.readText()) ?? ""
}

export default { write, read }
