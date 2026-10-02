// node:tty for the browser. The terminal is xterm.js, reached through the
// process shim's stdout/stdin, not through file descriptors.
import { EventEmitter } from "events"
export function isatty(): boolean {
  return false
}
export class WriteStream extends EventEmitter {
  isTTY = true
  columns = 80
  rows = 24
}
export class ReadStream extends EventEmitter {
  isTTY = true
  setRawMode() {
    return this
  }
}
export default { isatty, WriteStream, ReadStream }
