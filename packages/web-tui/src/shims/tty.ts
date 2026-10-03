// `tty` for the browser build: nothing is a terminal.
export const isatty = () => false
export class ReadStream {}
export class WriteStream {}
export default { isatty, ReadStream, WriteStream }
