import { beforeEach, describe, expect, test } from "bun:test"
import { configure, fs, InMemory } from "@zenfs/core"
import { Effect, Layer } from "effect"
import { VfsFileSystem } from "@opencode-ai/core/effect/vfs-filesystem"
import { Ripgrep } from "@opencode-ai/core/ripgrep.browser"

const run = <A, E>(effect: Effect.Effect<A, E, Ripgrep.Service>) =>
  Effect.runPromise(effect.pipe(Effect.provide(Ripgrep.layer.pipe(Layer.provide(VfsFileSystem.layer(fs))))))

const files: Record<string, string> = {
  "/w/.gitignore": "dist/\n*.log\n",
  "/w/README.md": "# Demo\nhello world\n",
  "/w/src/app.ts": 'export const greet = (name: string) => `hello ${name}`\n// TODO: héllo again\n',
  "/w/src/util/math.ts": "export const add = (a: number, b: number) => a + b\n",
  "/w/dist/app.js": "hello from build output\n",
  "/w/debug.log": "hello log\n",
  "/w/.hidden/secret.txt": "hello hidden\n",
  "/w/.git/HEAD": "ref: refs/heads/main\n",
}

beforeEach(async () => {
  await configure({ mounts: { "/": InMemory }, disableAccessChecks: true })
  for (const [path, content] of Object.entries(files)) {
    await fs.promises.mkdir(path.slice(0, path.lastIndexOf("/")), { recursive: true })
    await fs.promises.writeFile(path, content)
  }
  await fs.promises.writeFile("/w/src/blob.bin", new Uint8Array([104, 101, 108, 108, 111, 0, 1]))
})

describe("Ripgrep (browser)", () => {
  test("find lists files, honouring .gitignore, hidden and .git", async () => {
    const entries = await run(Effect.flatMap(Ripgrep.Service, (rg) => rg.find({ cwd: "/w", pattern: "*", limit: 100 })))
    expect(entries.map((entry) => entry.path as string)).toEqual([
      "README.md",
      "src/app.ts",
      "src/blob.bin",
      "src/util/math.ts",
    ])
    const hidden = await run(
      Effect.flatMap(Ripgrep.Service, (rg) => rg.find({ cwd: "/w", pattern: "*", limit: 100, hidden: true })),
    )
    expect(hidden.map((entry) => entry.path as string)).toContain(".hidden/secret.txt")
    expect(hidden.map((entry) => entry.path as string)).not.toContain(".git/HEAD")
  })

  test("glob matches basenames at any depth and paths with slashes", async () => {
    const ts = await run(Effect.flatMap(Ripgrep.Service, (rg) => rg.glob({ cwd: "/w", pattern: "*.ts", limit: 100 })))
    expect(ts.map((entry) => entry.path as string)).toEqual(["src/app.ts", "src/util/math.ts"])
    const nested = await run(
      Effect.flatMap(Ripgrep.Service, (rg) => rg.glob({ cwd: "/w", pattern: "src/util/**", limit: 100 })),
    )
    expect(nested.map((entry) => entry.path as string)).toEqual(["src/util/math.ts"])
    const limited = await run(Effect.flatMap(Ripgrep.Service, (rg) => rg.glob({ cwd: "/w", pattern: "*", limit: 2 })))
    expect(limited).toHaveLength(2)
  })

  test("grep reports lines, byte offsets and submatches", async () => {
    const matches = await run(
      Effect.flatMap(Ripgrep.Service, (rg) => rg.grep({ cwd: "/w", pattern: "h.llo", limit: 100 })),
    )
    expect(matches.map((match) => `${match.entry.path}:${match.line}`)).toEqual([
      ".hidden/secret.txt:1",
      "README.md:2",
      "src/app.ts:1",
      "src/app.ts:2",
    ])
    const second = matches.find((match) => match.entry.path === ("src/app.ts" as string) && match.line === 2)!
    // "// TODO: " is 9 bytes; "héllo" is 6 bytes in UTF-8.
    expect(second.submatches).toEqual([{ text: "héllo", start: 9, end: 15 }])
    expect(second.offset).toBe(Buffer.byteLength(files["/w/src/app.ts"].split("\n")[0]) + 1)
  })

  test("grep honours include, file and limit, and rejects bad patterns", async () => {
    const included = await run(
      Effect.flatMap(Ripgrep.Service, (rg) => rg.grep({ cwd: "/w", pattern: "export", include: "*.ts", limit: 100 })),
    )
    expect(included.map((match) => match.entry.path as string)).toEqual(["src/app.ts", "src/util/math.ts"])
    const single = await run(
      Effect.flatMap(Ripgrep.Service, (rg) => rg.grep({ cwd: "/w/src", pattern: "add", file: "util/math.ts", limit: 100 })),
    )
    expect(single).toHaveLength(1)
    const limited = await run(Effect.flatMap(Ripgrep.Service, (rg) => rg.grep({ cwd: "/w", pattern: "o", limit: 1 })))
    expect(limited).toHaveLength(1)
    const insensitive = await run(
      Effect.flatMap(Ripgrep.Service, (rg) => rg.grep({ cwd: "/w", pattern: "(?i)DEMO", limit: 10 })),
    )
    expect(insensitive.map((match) => match.entry.path as string)).toEqual(["README.md"])
    const error = await run(
      Effect.flatMap(Ripgrep.Service, (rg) => rg.grep({ cwd: "/w", pattern: "(", limit: 10 })).pipe(Effect.flip),
    )
    expect(error._tag).toBe("Ripgrep.InvalidPatternError")
  })
})
