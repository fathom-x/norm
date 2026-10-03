import { describe, expect, test } from "bun:test"
import { sql } from "drizzle-orm"
import { Effect, Layer } from "effect"
import * as Client from "effect/unstable/sql/SqlClient"
import { EffectDrizzleSqlite } from "@opencode-ai/effect-drizzle-sqlite"
import { DatabaseMigration } from "@opencode-ai/core/database/migration"
import { migrations } from "@opencode-ai/core/database/migration.gen"
import { Sqlite } from "@opencode-ai/core/database/sqlite"
import { layer as browserSqlite } from "@opencode-ai/core/database/sqlite.browser"

const run = <A, E>(effect: Effect.Effect<A, E, Client.SqlClient | Sqlite.Native | Sqlite.Drizzle>) =>
  Effect.runPromise(effect.pipe(Effect.provide(browserSqlite({ filename: ":memory:" })), Effect.scoped))

describe("sqlite.browser", () => {
  test("runs statements through the Effect SqlClient", async () => {
    const rows = await run(
      Effect.gen(function* () {
        const client = yield* Client.SqlClient
        yield* client`CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, flag INTEGER, data BLOB)`
        yield* client`INSERT INTO t (name, flag, data) VALUES (${"a"}, ${true}, ${new Uint8Array([1, 2])})`
        yield* client`INSERT INTO t (name, flag, data) VALUES (${"b"}, ${false}, ${null})`
        return yield* client<{ id: number; name: string; flag: number; data: Uint8Array | null }>`SELECT * FROM t ORDER BY id`
      }),
    )
    expect(rows.map((row) => [row.id, row.name, row.flag])).toEqual([
      [1, "a", 1],
      [2, "b", 0],
    ])
    expect(Array.from(rows[0].data!)).toEqual([1, 2])
  })

  test("returns bigints under SafeIntegers", async () => {
    const value = await run(
      Effect.gen(function* () {
        const client = yield* Client.SqlClient
        const rows = yield* client<{ n: bigint }>`SELECT 9007199254740993 AS n`.pipe(
          Effect.provideService(Client.SafeIntegers, true),
        )
        return rows[0].n
      }),
    )
    expect(value).toBe(9007199254740993n)
  })

  test("classifies constraint violations", async () => {
    const error = await run(
      Effect.gen(function* () {
        const client = yield* Client.SqlClient
        yield* client`CREATE TABLE u (id INTEGER PRIMARY KEY, name TEXT UNIQUE)`
        yield* client`INSERT INTO u (name) VALUES (${"x"})`
        return yield* client`INSERT INTO u (name) VALUES (${"x"})`.pipe(Effect.flip)
      }),
    )
    expect(error.reason._tag).toBe("UniqueViolation")
  })

  test("applies the real migrations on an empty database, then again as a no-op", async () => {
    const result = await run(
      Effect.gen(function* () {
        const db = yield* EffectDrizzleSqlite.makeWithDefaults()
        yield* DatabaseMigration.apply(db)
        yield* DatabaseMigration.apply(db)
        const tables = yield* db.all<{ name: string }>(
          sql`SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'`,
        )
        const applied = yield* db.all<{ id: string }>(sql`SELECT id FROM migration`)
        return { tables: tables.map((row) => row.name), applied: applied.length }
      }),
    )
    expect(result.tables).toContain("session")
    expect(result.tables).toContain("project")
    expect(result.applied).toBe(migrations.length)
  })

  test("drizzle's sqlite-proxy client reads the same handle", async () => {
    const rows = await run(
      Effect.gen(function* () {
        const client = yield* Client.SqlClient
        const drizzle = yield* Sqlite.Drizzle
        yield* client`CREATE TABLE v (id INTEGER PRIMARY KEY, name TEXT)`
        yield* client`INSERT INTO v (name) VALUES (${"x"}), (${"y"})`
        return yield* Effect.promise(async () => drizzle.all<{ id: number; name: string }>(sql`SELECT id, name FROM v`))
      }),
    )
    expect(rows).toEqual([
      [1, "x"],
      [2, "y"],
    ] as never)
  })
})
