// Browser twin of ripgrep.ts, swapped in by the web build. There is no rg
// binary to download or spawn in a tab, so the same three operations —
// list files, match a glob, search contents — walk the virtual file system
// in JS. Rows, errors and the Service key are identical to ripgrep.ts, so the
// grep/glob tools, FileSystemSearch and the /find routes are unchanged.
//
// Matching follows rg's defaults closely enough for an agent: .gitignore /
// .ignore files are honoured (via `ignore`), hidden entries are skipped unless
// asked for, `.git` is always skipped, binary files (a NUL in the first 8 KiB)
// are not searched, and patterns are JavaScript regular expressions (rg's
// Rust syntax is nearly a subset; `(?i)` is translated).
import { Context, Effect, FileSystem, Layer, Schema } from "effect"
import ignore, { type Ignore } from "ignore"
import { minimatch } from "minimatch"
import { Entry, Match } from "@opencode-ai/schema/filesystem"
import { makeGlobalNode } from "./effect/app-node"
import { filesystem } from "./effect/app-node-platform"
import { RelativePath } from "./schema"
import type { FindInput, GlobInput, GrepInput, Interface } from "./ripgrep"

export type { FindInput, GlobInput, GrepInput, Interface } from "./ripgrep"

const MAX_SUBMATCHES = 100
const MAX_LINE = 2_000
const BINARY_SNIFF = 8 * 1024

export class Error extends Schema.TaggedErrorClass<Error>()("Ripgrep.Error", {
  message: Schema.String,
  cause: Schema.optional(Schema.Defect()),
}) {}

export class InvalidPatternError extends Schema.TaggedErrorClass<InvalidPatternError>()("Ripgrep.InvalidPatternError", {
  pattern: Schema.String,
  message: Schema.String,
}) {}

export class Service extends Context.Service<Service, Interface>()("@opencode/v2/Ripgrep") {}

type Walk = {
  readonly cwd: string
  readonly hidden?: boolean
  readonly signal?: AbortSignal
  readonly accept: (relative: string) => boolean
}

/** Needs only an Effect FileSystem — exported for tests. */
export const layer = Layer.effect(
  Service,
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem

    // Depth-first, directory entries sorted so results are stable. `visit`
    // returns false to stop the walk (limit reached).
    const walk = (input: Walk, visit: (relative: string) => Effect.Effect<boolean, Error>) => {
      const readIgnore = (directory: string) =>
        Effect.forEach([".gitignore", ".ignore", ".rgignore"], (name) =>
          fs.readFileString(`${directory}/${name}`).pipe(Effect.orElseSucceed(() => "")),
        ).pipe(Effect.map((files) => files.filter(Boolean)))

      const step = (
        relative: string,
        rules: ReadonlyArray<{ base: string; ignore: Ignore }>,
      ): Effect.Effect<boolean, Error> =>
        Effect.gen(function* () {
          if (input.signal?.aborted) return false
          const directory = relative ? `${input.cwd}/${relative}` : input.cwd
          const local = yield* readIgnore(directory)
          const scoped = local.length > 0 ? [...rules, { base: relative, ignore: ignore().add(local.join("\n")) }] : rules
          const names = yield* fs.readDirectory(directory).pipe(
            Effect.map((entries) => entries.toSorted()),
            Effect.mapError((cause) => new Error({ message: `Failed to read ${directory}`, cause })),
          )
          for (const name of names) {
            if (name === ".git") continue
            if (!input.hidden && name.startsWith(".")) continue
            const path = relative ? `${relative}/${name}` : name
            const info = yield* fs.stat(`${input.cwd}/${path}`).pipe(Effect.option)
            if (info._tag === "None") continue
            const isDirectory = info.value.type === "Directory"
            if (ignored(scoped, path, isDirectory)) continue
            if (isDirectory) {
              if (!(yield* step(path, scoped))) return false
              continue
            }
            if (info.value.type !== "File" || !input.accept(path)) continue
            if (!(yield* visit(path))) return false
          }
          return true
        })

      return step("", [])
    }

    const files = (input: FindInput | GlobInput, pattern: string | undefined) =>
      Effect.gen(function* () {
        const found: Entry[] = []
        const onEntry = "onEntry" in input ? input.onEntry : undefined
        const accept = pattern ? globMatcher(pattern) : () => true
        yield* walk({ cwd: input.cwd, hidden: input.hidden, signal: input.signal, accept }, (relative) =>
          Effect.gen(function* () {
            const entry = Entry.make({ path: RelativePath.make(relative), type: "file" })
            found.push(entry)
            if (onEntry) yield* onEntry(entry)
            return found.length < input.limit
          }),
        )
        return found
      })

    return Service.of({
      find: (input) => files(input, input.pattern === "*" ? undefined : input.pattern),
      glob: (input) => files(input, input.pattern),
      grep: (input) =>
        Effect.gen(function* () {
          const regex = yield* compile(input.pattern)
          const matches: Match[] = []
          const include = input.include ? globMatcher(input.include) : () => true
          const search = (relative: string) =>
            Effect.gen(function* () {
              const bytes = yield* fs.readFile(`${input.cwd}/${relative}`).pipe(Effect.orElseSucceed(() => undefined))
              if (!bytes || bytes.subarray(0, BINARY_SNIFF).includes(0)) return true
              for (const match of matchLines(new TextDecoder().decode(bytes), regex, relative)) {
                matches.push(match)
                if (matches.length >= input.limit) return false
              }
              return true
            })
          if (input.file) {
            yield* search(input.file)
            return matches
          }
          yield* walk({ cwd: input.cwd, hidden: true, signal: input.signal, accept: include }, search)
          return matches
        }),
    })
  }),
)

function ignored(rules: ReadonlyArray<{ base: string; ignore: Ignore }>, path: string, directory: boolean) {
  return rules.some((rule) => {
    const relative = rule.base ? path.slice(rule.base.length + 1) : path
    return rule.ignore.ignores(directory ? `${relative}/` : relative)
  })
}

// rg globs are gitignore-style: a pattern without a slash matches the file
// name at any depth; `!pattern` excludes.
function globMatcher(pattern: string) {
  const negated = pattern.startsWith("!")
  const body = negated ? pattern.slice(1) : pattern
  const matchBase = !body.includes("/")
  return (relative: string) => minimatch(relative, body, { dot: true, matchBase }) !== negated
}

function compile(pattern: string) {
  const insensitive = pattern.startsWith("(?i)")
  const source = insensitive ? pattern.slice(4) : pattern
  const flags = insensitive ? "gi" : "g"
  return Effect.try({
    // Unicode mode first (closest to rg); it rejects identity escapes such as
    // `\-` that rg accepts, so retry without it before giving up.
    try: () => {
      try {
        return new RegExp(source, flags + "u")
      } catch {
        return new RegExp(source, flags)
      }
    },
    catch: (cause) =>
      new InvalidPatternError({
        pattern,
        message: `regex parse error: ${cause instanceof globalThis.Error ? cause.message : String(cause)}`,
      }),
  })
}

function matchLines(text: string, regex: RegExp, relative: string) {
  const encoder = new TextEncoder()
  const byteLength = (value: string) => encoder.encode(value).length
  const state = { offset: 0 }
  return text.split("\n").flatMap((raw, index) => {
    const start = state.offset
    state.offset += byteLength(raw) + 1
    const line = raw.endsWith("\r") ? raw.slice(0, -1) : raw
    const found = [...line.matchAll(regex)]
    if (found.length === 0) return []
    return [
      Match.make({
        entry: Entry.make({ path: RelativePath.make(relative), type: "file" }),
        line: index + 1,
        offset: start,
        text: line.length > MAX_LINE ? line.slice(0, MAX_LINE) + "..." : line,
        submatches: found
          .filter((match) => match[0].length > 0)
          .slice(0, MAX_SUBMATCHES)
          .map((match) => ({
            text: match[0],
            start: byteLength(line.slice(0, match.index)),
            end: byteLength(line.slice(0, match.index + match[0].length)),
          })),
      }),
    ]
  })
}

export const node = makeGlobalNode({ service: Service, layer, deps: [filesystem] })

export * as Ripgrep from "./ripgrep.browser"
