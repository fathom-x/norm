import { beforeEach, describe, expect, test } from "bun:test"
import { configure, fs, InMemory } from "@zenfs/core"
import { Effect, FileSystem, Option, Scope, Stream } from "effect"
import { VfsFileSystem } from "@opencode-ai/core/effect/vfs-filesystem"

const run = <A, E>(effect: Effect.Effect<A, E, FileSystem.FileSystem | Scope.Scope>) =>
  Effect.runPromise(effect.pipe(Effect.provide(VfsFileSystem.layer(fs)), Effect.scoped))

beforeEach(() => configure({ mounts: { "/": InMemory }, disableAccessChecks: true }))

describe("VfsFileSystem", () => {
  test("mkdir, write, read, stat, readdir", async () => {
    const result = await run(
      Effect.gen(function* () {
        const vfs = yield* FileSystem.FileSystem
        yield* vfs.makeDirectory("/workspace/src", { recursive: true })
        yield* vfs.writeFileString("/workspace/src/a.ts", "export const a = 1\n")
        yield* vfs.writeFile("/workspace/b.bin", new Uint8Array([1, 2, 3]))
        return {
          text: yield* vfs.readFileString("/workspace/src/a.ts"),
          bytes: Array.from(yield* vfs.readFile("/workspace/b.bin")),
          file: yield* vfs.stat("/workspace/src/a.ts"),
          dir: yield* vfs.stat("/workspace/src"),
          top: (yield* vfs.readDirectory("/workspace")).sort(),
          deep: (yield* vfs.readDirectory("/workspace", { recursive: true })).sort(),
          exists: yield* vfs.exists("/workspace/nope"),
        }
      }),
    )
    expect(result.text).toBe("export const a = 1\n")
    expect(result.bytes).toEqual([1, 2, 3])
    expect(result.file.type).toBe("File")
    expect(Number(result.file.size)).toBe(19)
    expect(result.dir.type).toBe("Directory")
    expect(result.top).toEqual(["b.bin", "src"])
    expect(result.deep).toEqual(["b.bin", "src", "src/a.ts"])
    expect(result.exists).toBe(false)
  })

  test("maps errno codes to PlatformError reasons", async () => {
    const result = await run(
      Effect.gen(function* () {
        const vfs = yield* FileSystem.FileSystem
        yield* vfs.makeDirectory("/d")
        return {
          missing: yield* vfs.readFile("/missing").pipe(Effect.flip),
          exists: yield* vfs.makeDirectory("/d").pipe(Effect.flip),
        }
      }),
    )
    expect(result.missing.reason._tag).toBe("NotFound")
    expect(result.exists.reason._tag).toBe("AlreadyExists")
  })

  test("rm, rename, copy and temp files", async () => {
    const result = await run(
      Effect.gen(function* () {
        const vfs = yield* FileSystem.FileSystem
        yield* vfs.makeDirectory("/a/b", { recursive: true })
        yield* vfs.writeFileString("/a/b/f.txt", "x")
        yield* vfs.copy("/a", "/c")
        yield* vfs.rename("/c/b/f.txt", "/c/b/g.txt")
        yield* vfs.remove("/a", { recursive: true })
        const temp = yield* vfs.makeTempFileScoped({ suffix: ".log" })
        return {
          a: yield* vfs.exists("/a"),
          moved: yield* vfs.readFileString("/c/b/g.txt"),
          temp: (yield* vfs.stat(temp)).type,
          suffix: temp.endsWith(".log"),
        }
      }),
    )
    expect(result).toEqual({ a: false, moved: "x", temp: "File", suffix: true })
  })

  test("file handles keep their own cursor and append", async () => {
    const result = await run(
      Effect.gen(function* () {
        const vfs = yield* FileSystem.FileSystem
        const written = yield* Effect.scoped(
          Effect.gen(function* () {
            const file = yield* vfs.open("/log.txt", { flag: "w+" })
            yield* file.writeAll(new TextEncoder().encode("hello "))
            yield* file.writeAll(new TextEncoder().encode("world"))
            yield* file.seek(0, "start")
            const chunk = yield* file.readAlloc(5)
            return new TextDecoder().decode(Option.getOrThrow(chunk))
          }),
        )
        yield* Effect.scoped(
          Effect.gen(function* () {
            const file = yield* vfs.open("/log.txt", { flag: "a" })
            yield* file.writeAll(new TextEncoder().encode("!"))
          }),
        )
        return { written, all: yield* vfs.readFileString("/log.txt") }
      }),
    )
    expect(result).toEqual({ written: "hello", all: "hello world!" })
  })

  test("stream and sink go through open/read/write", async () => {
    const text = await run(
      Effect.gen(function* () {
        const vfs = yield* FileSystem.FileSystem
        yield* Stream.make(new TextEncoder().encode("abc".repeat(100))).pipe(Stream.run(vfs.sink("/s.txt")))
        const chunks = yield* Stream.runCollect(vfs.stream("/s.txt", { chunkSize: 64 }))
        return { count: chunks.length, text: new TextDecoder().decode(Buffer.concat([...chunks])) }
      }),
    )
    expect(text.text).toBe("abc".repeat(100))
    expect(text.count).toBe(5)
  })
})
