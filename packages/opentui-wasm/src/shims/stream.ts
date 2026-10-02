// node:stream for the browser: minimal event-emitter based streams. opentui
// only uses these for console capture and optional custom stdout/stdin.
import { EventEmitter } from "events"

export class Stream extends EventEmitter {
  pipe<T extends Writable>(destination: T): T {
    this.on("data", (chunk: unknown) => destination.write(chunk as any))
    return destination
  }
}

export class Readable extends Stream {
  readable = true
  private _buffer: unknown[] = []
  private _flowing = false
  constructor(_options?: unknown) {
    super()
  }
  push(chunk: unknown): boolean {
    if (chunk === null) {
      this.emit("end")
      return false
    }
    if (this._flowing) this.emit("data", chunk)
    else this._buffer.push(chunk)
    return true
  }
  read(): unknown {
    return this._buffer.length > 0 ? this._buffer.shift() : null
  }
  resume() {
    this._flowing = true
    while (this._buffer.length > 0) this.emit("data", this._buffer.shift())
    return this
  }
  pause() {
    this._flowing = false
    return this
  }
  setEncoding() {
    return this
  }
  destroy() {
    this.emit("close")
    return this
  }
  static from(iterable: Iterable<unknown>) {
    const r = new Readable()
    queueMicrotask(() => {
      for (const chunk of iterable) r.push(chunk)
      r.push(null)
    })
    return r
  }
}

type WriteFn = (chunk: any, encoding: string, callback: (error?: Error | null) => void) => void

export class Writable extends Stream {
  writable = true
  private _writeImpl?: WriteFn
  constructor(options?: { write?: WriteFn }) {
    super()
    this._writeImpl = options?.write
  }
  _write(chunk: unknown, encoding: string, callback: (error?: Error | null) => void) {
    if (this._writeImpl) this._writeImpl.call(this, chunk, encoding, callback)
    else callback()
  }
  write(chunk: unknown, encoding?: unknown, callback?: (error?: Error | null) => void): boolean {
    const cb = typeof encoding === "function" ? (encoding as (error?: Error | null) => void) : callback
    this._write(chunk, typeof encoding === "string" ? encoding : "utf8", (error) => {
      if (error) this.emit("error", error)
      cb?.(error ?? null)
    })
    return true
  }
  end(chunk?: unknown) {
    if (chunk !== undefined && typeof chunk !== "function") this.write(chunk)
    this.emit("finish")
    return this
  }
  destroy() {
    this.emit("close")
    return this
  }
}

export class Duplex extends Readable {
  writable = true
  write(chunk: unknown): boolean {
    this.emit("data", chunk)
    return true
  }
  end() {
    this.emit("finish")
    return this
  }
}

export class Transform extends Duplex {}
export class PassThrough extends Transform {}

export default { Stream, Readable, Writable, Duplex, Transform, PassThrough }
