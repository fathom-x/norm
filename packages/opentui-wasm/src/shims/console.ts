// node:console's Console class over the browser console.
export class Console {
  constructor(_stdout?: unknown, _stderr?: unknown) {
    return Object.assign(Object.create(Console.prototype), globalThis.console)
  }
}
export default { Console }
