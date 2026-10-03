// Effect `FileSystem` over a Node-compatible virtual file system — ZenFS
// (@zenfs/core) in the browser build, where there is no disk. The VFS is a
// parameter so tests can hand in a fresh in-memory instance; the browser
// platform layer (app-browser-platform.ts) passes ZenFS's global `fs`, the
// same object `fs`/`node:fs` imports are aliased to, so code that still uses
// raw fs and code that uses this service see one tree.
//
// Mirrors @effect/platform-node-shared's NodeFileSystem: same errno →
// PlatformError mapping, same File cursor semantics. `watch` never emits:
// there is no external writer to observe in a tab.
import { Effect, FileSystem, Layer, Option, Stream } from "effect"
import * as PlatformError from "effect/PlatformError"
import type { Stats, fs as ZenFs } from "@zenfs/core"

export type Vfs = typeof ZenFs

type FileHandle = Awaited<ReturnType<Vfs["promises"]["open"]>>

export function make(vfs: Vfs): FileSystem.FileSystem {
  const fs = vfs.promises
  const call = <A>(method: string, path: string | number, run: () => Promise<A>) =>
    Effect.tryPromise({ try: run, catch: (cause) => toPlatformError(method, path, cause) })

  const makeTempDirectory = (method: string, options?: { directory?: string; prefix?: string }) =>
    call(method, options?.directory ?? "/tmp", async () => {
      const directory = (options?.directory ?? "/tmp").replace(/\/+$/, "")
      await fs.mkdir(directory, { recursive: true })
      return fs.mkdtemp(`${directory}/${options?.prefix ?? ""}`)
    })

  const makeTempFile = (method: string, options?: { directory?: string; prefix?: string; suffix?: string }) =>
    Effect.gen(function* () {
      const directory = yield* makeTempDirectory(method, options)
      const name = `${directory}/${randomName()}${options?.suffix ?? ""}`
      yield* call(method, name, () => fs.writeFile(name, new Uint8Array(0)))
      return name
    })

  const remove = (method: string, path: string, options?: { recursive?: boolean; force?: boolean }) =>
    call(method, path, () => fs.rm(path, { recursive: options?.recursive ?? false, force: options?.force ?? false }))

  return FileSystem.make({
    // stat first: with access checks disabled (as the browser build runs
    // ZenFS) `access` alone does not report a missing path.
    access: (path, options) =>
      call("access", path, async () => {
        await fs.stat(path)
        const mode = (options?.readable ? vfs.constants.R_OK : 0) | (options?.writable ? vfs.constants.W_OK : 0)
        if (mode) await fs.access(path, mode)
      }),
    copy: (from, to, options) =>
      call("copy", from, () =>
        fs.cp(from, to, {
          force: options?.overwrite ?? false,
          preserveTimestamps: options?.preserveTimestamps ?? false,
          recursive: true,
        }),
      ),
    copyFile: (from, to) => call("copyFile", from, () => fs.copyFile(from, to)),
    chmod: (path, mode) => call("chmod", path, () => fs.chmod(path, mode)),
    chown: (path, uid, gid) => call("chown", path, () => fs.chown(path, uid, gid)),
    link: (from, to) => call("link", from, () => fs.link(from, to)),
    makeDirectory: (path, options) =>
      call("makeDirectory", path, () =>
        fs.mkdir(path, { recursive: options?.recursive ?? false, mode: options?.mode }).then(() => undefined),
      ),
    makeTempDirectory: (options) => makeTempDirectory("makeTempDirectory", options),
    makeTempDirectoryScoped: (options) =>
      Effect.acquireRelease(makeTempDirectory("makeTempDirectoryScoped", options), (directory) =>
        Effect.orDie(remove("makeTempDirectoryScoped", directory, { recursive: true })),
      ),
    makeTempFile: (options) => makeTempFile("makeTempFile", options),
    makeTempFileScoped: (options) =>
      Effect.acquireRelease(makeTempFile("makeTempFileScoped", options), (file) =>
        Effect.orDie(remove("makeTempFileScoped", file.slice(0, file.lastIndexOf("/")), { recursive: true })),
      ),
    open: (path, options) =>
      Effect.acquireRelease(
        call("open", path, () => fs.open(path, options?.flag ?? "r", options?.mode)),
        (handle) => Effect.promise(() => handle.close().catch(() => undefined)),
      ).pipe(Effect.map((handle) => makeFile(handle, options?.flag?.startsWith("a") ?? false))),
    readDirectory: (path, options) =>
      call("readDirectory", path, () => fs.readdir(path, { recursive: options?.recursive ?? false })),
    readFile: (path) => call("readFile", path, () => fs.readFile(path).then((data) => new Uint8Array(data))),
    readLink: (path) => call("readLink", path, () => fs.readlink(path)),
    realPath: (path) => call("realPath", path, () => fs.realpath(path)),
    remove: (path, options) => remove("remove", path, options),
    rename: (from, to) => call("rename", from, () => fs.rename(from, to)),
    stat: (path) => call("stat", path, () => fs.stat(path)).pipe(Effect.map(fileInfo)),
    symlink: (target, path) => call("symlink", path, () => fs.symlink(target, path)),
    truncate: (path, length) =>
      call("truncate", path, () => fs.truncate(path, length !== undefined ? Number(length) : undefined)),
    utimes: (path, atime, mtime) => call("utime", path, () => fs.utimes(path, atime, mtime)),
    watch: () => Stream.never,
    writeFile: (path, data, options) =>
      call("writeFile", path, () => fs.writeFile(path, data, { flag: options?.flag, mode: options?.mode })),
  })

  function makeFile(handle: FileHandle, append: boolean): FileSystem.File {
    const fd = FileSystem.FileDescriptor(handle.fd)
    // The cursor lives here, as in NodeFileSystem: reads and writes are
    // positional so concurrent fibers on one handle can't race the VFS's own.
    let position = 0n
    const write = (method: string, buffer: Uint8Array) =>
      Effect.suspend(() => {
        const at = position
        return call(method, handle.fd, () => handle.write(buffer, 0, buffer.length, append ? null : Number(at))).pipe(
          Effect.map(({ bytesWritten }) => {
            if (!append) position = at + BigInt(bytesWritten)
            return bytesWritten
          }),
        )
      })
    const writeAll = (buffer: Uint8Array): Effect.Effect<void, PlatformError.PlatformError> =>
      write("writeAll", buffer).pipe(
        Effect.flatMap((written) => {
          if (written === 0)
            return Effect.fail(
              PlatformError.systemError({
                module: "FileSystem",
                method: "writeAll",
                _tag: "WriteZero",
                pathOrDescriptor: handle.fd,
                description: "write returned 0 bytes written",
              }),
            )
          return written < buffer.length ? writeAll(buffer.subarray(written)) : Effect.void
        }),
      )
    const read = (method: string, buffer: Uint8Array) =>
      Effect.suspend(() => {
        const at = position
        return call(method, handle.fd, () => handle.read(buffer, 0, buffer.length, Number(at))).pipe(
          Effect.map(({ bytesRead }) => {
            position = at + BigInt(bytesRead)
            return bytesRead
          }),
        )
      })
    return {
      [FileSystem.FileTypeId]: FileSystem.FileTypeId,
      fd,
      get stat() {
        return call("stat", handle.fd, () => handle.stat()).pipe(Effect.map(fileInfo))
      },
      get sync() {
        return call("sync", handle.fd, () => handle.sync())
      },
      seek: (offset, from) =>
        Effect.sync(() => {
          position = from === "start" ? FileSystem.Size(offset) : position + FileSystem.Size(offset)
        }),
      read: (buffer) => read("read", buffer).pipe(Effect.map(FileSystem.Size)),
      readAlloc: (size) =>
        Effect.suspend(() => {
          const buffer = new Uint8Array(Number(size))
          return read("readAlloc", buffer).pipe(
            Effect.map((bytes) => (bytes === 0 ? Option.none() : Option.some(buffer.subarray(0, bytes)))),
          )
        }),
      truncate: (length) =>
        call("truncate", handle.fd, () => handle.truncate(length !== undefined ? Number(length) : undefined)).pipe(
          Effect.map(() => {
            if (!append && position > BigInt(length ?? 0)) position = BigInt(length ?? 0)
          }),
        ),
      write: (buffer) => write("write", buffer).pipe(Effect.map(FileSystem.Size)),
      writeAll,
    }
  }
}

export const layer = (vfs: Vfs) => Layer.succeed(FileSystem.FileSystem, make(vfs))

function fileInfo(stat: Stats): FileSystem.File.Info {
  return {
    type: stat.isFile()
      ? "File"
      : stat.isDirectory()
        ? "Directory"
        : stat.isSymbolicLink()
          ? "SymbolicLink"
          : stat.isBlockDevice()
            ? "BlockDevice"
            : stat.isCharacterDevice()
              ? "CharacterDevice"
              : stat.isFIFO()
                ? "FIFO"
                : stat.isSocket()
                  ? "Socket"
                  : "Unknown",
    mtime: Option.fromNullishOr(stat.mtime),
    atime: Option.fromNullishOr(stat.atime),
    birthtime: Option.fromNullishOr(stat.birthtime),
    dev: stat.dev,
    rdev: Option.fromNullishOr(stat.rdev),
    ino: Option.fromNullishOr(stat.ino),
    mode: stat.mode,
    nlink: Option.fromNullishOr(stat.nlink),
    uid: Option.fromNullishOr(stat.uid),
    gid: Option.fromNullishOr(stat.gid),
    size: FileSystem.Size(stat.size),
    blksize: stat.blksize !== undefined ? Option.some(FileSystem.Size(stat.blksize)) : Option.none(),
    blocks: Option.fromNullishOr(stat.blocks),
  }
}

// Same mapping as @effect/platform-node-shared's handleErrnoException, so
// callers matching on `NotFound` / `AlreadyExists` behave as on disk.
function toPlatformError(method: string, path: string | number, cause: unknown) {
  const code = cause && typeof cause === "object" && "code" in cause ? cause.code : undefined
  const tag =
    code === "ENOENT"
      ? "NotFound"
      : code === "EACCES" || code === "EPERM"
        ? "PermissionDenied"
        : code === "EEXIST"
          ? "AlreadyExists"
          : code === "EISDIR" || code === "ENOTDIR" || code === "ELOOP"
            ? "BadResource"
            : code === "EBUSY"
              ? "Busy"
              : "Unknown"
  return PlatformError.systemError({
    _tag: tag,
    module: "FileSystem",
    method,
    pathOrDescriptor: path,
    syscall: cause && typeof cause === "object" && "syscall" in cause ? String(cause.syscall) : undefined,
    cause,
  })
}

function randomName() {
  return Array.from(crypto.getRandomValues(new Uint8Array(6)), (byte) => byte.toString(16).padStart(2, "0")).join("")
}

export * as VfsFileSystem from "./vfs-filesystem"
