// Browser sibling of sqlite.bun.ts / sqlite.node.ts, selected by the `browser`
// condition on `#sqlite`. SQLite is the official WebAssembly build
// (@sqlite.org/sqlite-wasm) driven through its synchronous OO1 API, so the
// Effect SqlClient and the drizzle client behave exactly like the bun:sqlite
// ones: one connection, statements run to completion on the calling fiber.
//
// Storage: in a dedicated worker with OPFS the database lives in the
// `opfs-sahpool` VFS (persistent, no COOP/COEP needed, one tab at a time);
// anywhere else — tests, the main thread, browsers without
// `createSyncAccessHandle` — it is an in-memory database. `:memory:` is always
// in-memory. WAL pragmas are accepted and ignored by sahpool.
import sqlite3InitModule, { type Database, type PreparedStatement, type Sqlite3Static } from "@sqlite.org/sqlite-wasm"
import { drizzle } from "drizzle-orm/sqlite-proxy"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Fiber from "effect/Fiber"
import { identity } from "effect/Function"
import * as Layer from "effect/Layer"
import * as Scope from "effect/Scope"
import * as Semaphore from "effect/Semaphore"
import * as Stream from "effect/Stream"
import * as Reactivity from "effect/unstable/reactivity/Reactivity"
import * as Client from "effect/unstable/sql/SqlClient"
import type { Connection } from "effect/unstable/sql/SqlConnection"
import { classifySqliteError, SqlError } from "effect/unstable/sql/SqlError"
import * as Statement from "effect/unstable/sql/Statement"
import { Sqlite } from "./sqlite"

const ATTR_DB_SYSTEM_NAME = "db.system.name"

const TypeId = "~@opencode-ai/core/database/SqliteBrowser" as const
type TypeId = typeof TypeId

interface SqliteClient extends Client.SqlClient {
  readonly [TypeId]: TypeId
  readonly config: Config
  readonly export: Effect.Effect<Uint8Array, SqlError>
  readonly updateValues: never
}

interface Config {
  readonly filename: string
  readonly readonly?: boolean
  readonly create?: boolean
  readonly readwrite?: boolean
  readonly disableWAL?: boolean
  /** "auto" (default): OPFS sahpool when available, else memory. */
  readonly storage?: "auto" | "memory"
  readonly spanAttributes?: Record<string, unknown>
  readonly transformResultNames?: (str: string) => string
  readonly transformQueryNames?: (str: string) => string
}

interface SqliteConnection extends Connection {
  readonly export: Effect.Effect<Uint8Array, SqlError>
}

/** The opened database plus the module that owns it (for capi helpers). */
export interface Handle {
  readonly sqlite3: Sqlite3Static
  readonly db: Database
  /** Where the bytes live: "opfs" (sahpool) or "memory". */
  readonly storage: "opfs" | "memory"
}

// One module instance and one sahpool per worker: the pool holds exclusive
// OPFS sync-access handles, so installing it twice would fail.
const POOL_NAME = "norm-sqlite"
let runtime:
  | Promise<{ sqlite3: Sqlite3Static; pool?: Awaited<ReturnType<Sqlite3Static["installOpfsSAHPoolVfs"]>> }>
  | undefined

function loadRuntime() {
  runtime ??= (async () => {
    const sqlite3 = await sqlite3InitModule()
    if (!opfsAvailable()) return { sqlite3 }
    const pool = await sqlite3
      .installOpfsSAHPoolVfs({ name: POOL_NAME, directory: `.${POOL_NAME}`, initialCapacity: 8 })
      .catch((error: unknown) => {
        console.warn("[sqlite] OPFS sahpool unavailable, using an in-memory database", error)
        return undefined
      })
    return { sqlite3, pool }
  })()
  return runtime
}

function opfsAvailable() {
  const scope = globalThis as { FileSystemFileHandle?: { prototype: object }; navigator?: { storage?: object } }
  return (
    typeof scope.navigator?.storage === "object" &&
    "getDirectory" in (scope.navigator.storage ?? {}) &&
    !!scope.FileSystemFileHandle &&
    "createSyncAccessHandle" in scope.FileSystemFileHandle.prototype
  )
}

export async function open(config: Config): Promise<Handle> {
  const { sqlite3, pool } = await loadRuntime()
  const handle: Handle =
    config.filename === ":memory:" || config.storage === "memory" || !pool
      ? { sqlite3, db: new sqlite3.oo1.DB(":memory:", "c"), storage: "memory" }
      : { sqlite3, db: new pool.OpfsSAHPoolDb(config.filename), storage: "opfs" }
  // Report SQLITE_CONSTRAINT_UNIQUE etc. like bun:sqlite and node:sqlite do.
  sqlite3.capi.sqlite3_extended_result_codes(handle.db, 1)
  return handle
}

const make = (options: Config) =>
  Effect.gen(function* () {
    const { sqlite3, db } = (yield* Sqlite.Native) as Handle

    const compiler = Statement.makeCompilerSqlite(options.transformQueryNames)
    const transformRows = options.transformResultNames
      ? Statement.defaultTransforms(options.transformResultNames).array
      : undefined

    const statement = <A>(query: string, params: ReadonlyArray<unknown>, shape: "object" | "array") =>
      Effect.withFiber<Array<A>, SqlError>((fiber) => {
        const safe = Context.get(fiber.context, Client.SafeIntegers)
        try {
          return Effect.succeed(all(sqlite3, db, { query, params, safe, shape }) as Array<A>)
        } catch (cause) {
          return Effect.fail(
            new SqlError({
              reason: classifySqliteError(withCode(sqlite3, cause), {
                message: "Failed to execute statement",
                operation: "execute",
              }),
            }),
          )
        }
      })

    const run = (query: string, params: ReadonlyArray<unknown> = []) =>
      statement<Record<string, unknown>>(query, params, "object")
    const runValues = (query: string, params: ReadonlyArray<unknown> = []) =>
      statement<unknown[]>(query, params, "array")

    const exportDb = Effect.try({
      try: () => sqlite3.capi.sqlite3_js_db_export(db),
      catch: (cause) =>
        new SqlError({
          reason: classifySqliteError(withCode(sqlite3, cause), {
            message: "Failed to export database",
            operation: "export",
          }),
        }),
    })

    const connection = identity<SqliteConnection>({
      execute(query, params, transformRows) {
        return transformRows ? Effect.map(run(query, params), transformRows) : run(query, params)
      },
      executeRaw(query, params) {
        return run(query, params)
      },
      executeValues(query, params) {
        return runValues(query, params)
      },
      executeUnprepared(query, params, transformRows) {
        return this.execute(query, params, transformRows)
      },
      executeStream() {
        return Stream.die("executeStream not implemented")
      },
      export: exportDb,
    })

    const semaphore = yield* Semaphore.make(1)
    const acquirer = semaphore.withPermits(1)(Effect.succeed(connection))
    const transactionAcquirer = Effect.uninterruptibleMask((restore) => {
      const fiber = Fiber.getCurrent()!
      const scope = Context.getUnsafe(fiber.context, Scope.Scope)
      return Effect.as(
        Effect.tap(restore(semaphore.take(1)), () => Scope.addFinalizer(scope, semaphore.release(1))),
        connection,
      )
    })

    return Object.assign(
      (yield* Client.make({
        acquirer,
        compiler,
        transactionAcquirer,
        spanAttributes: [
          ...(options.spanAttributes ? Object.entries(options.spanAttributes) : []),
          [ATTR_DB_SYSTEM_NAME, "sqlite"],
        ],
        transformRows,
      })) as SqliteClient,
      {
        [TypeId]: TypeId,
        config: options,
        export: Effect.flatMap(acquirer, (_) => _.export),
      },
    )
  })

/**
 * Run one statement to completion, as objects (bun's `.all()`) or as arrays
 * (`.values()`).
 */
function all(
  sqlite3: Sqlite3Static,
  db: Database,
  input: { query: string; params: ReadonlyArray<unknown>; safe: boolean; shape: "object" | "array" },
) {
  const stmt = db.prepare(input.query)
  try {
    if (input.params.length > 0) stmt.bind(input.params.map(bindable))
    const names = input.shape === "object" && stmt.columnCount > 0 ? stmt.getColumnNames() : []
    const rows: unknown[] = []
    while (stmt.step()) {
      const values = Array.from({ length: stmt.columnCount }, (_, index) => column(sqlite3, stmt, index, input.safe))
      rows.push(input.shape === "object" ? Object.fromEntries(names.map((name, index) => [name, values[index]])) : values)
    }
    return rows
  } finally {
    stmt.finalize()
  }
}

// `SafeIntegers` (bun's `safeIntegers`, node's `setReadBigInts`) returns every
// INTEGER column as a bigint. Nothing in opencode turns it on today, but honour
// it so the driver matches its siblings.
function column(sqlite3: Sqlite3Static, stmt: PreparedStatement, index: number, safe: boolean) {
  const value = stmt.get(index)
  if (!safe || typeof value !== "number") return value
  if (sqlite3.capi.sqlite3_column_type(stmt.pointer!, index) !== sqlite3.capi.SQLITE_INTEGER) return value
  return BigInt(value)
}

// bun:sqlite binds booleans as 0/1 and undefined as NULL; OO1 rejects both.
function bindable(value: unknown) {
  if (value === undefined) return null
  if (typeof value === "boolean") return value ? 1 : 0
  return value as never
}

// sqlite-wasm errors carry `resultCode`; Effect's classifier reads `code`
// (e.g. "SQLITE_CONSTRAINT_UNIQUE") and `errno`, as node and bun report them.
function withCode(sqlite3: Sqlite3Static, cause: unknown) {
  if (!cause || typeof cause !== "object" || !("resultCode" in cause) || typeof cause.resultCode !== "number")
    return cause
  return Object.assign(cause, {
    code: sqlite3.capi.sqlite3_js_rc_str(cause.resultCode),
    errno: cause.resultCode,
  })
}

const nativeLayer = (config: Config) =>
  Layer.effect(
    Sqlite.Native,
    Effect.gen(function* () {
      const handle = yield* Effect.promise(() => open(config))
      yield* Effect.addFinalizer(() => Effect.sync(() => handle.db.close()))
      if (config.disableWAL !== true && handle.storage === "memory") handle.db.exec("PRAGMA journal_mode = MEMORY;")
      return handle
    }),
  )

const sqliteLayer = (config: Config) => Layer.effect(Client.SqlClient, make(config))

// drizzle's sqlite-proxy driver: drizzle builds SQL + params, we run them
// synchronously on the same handle and hand rows back as arrays.
const drizzleLayer = Layer.effect(
  Sqlite.Drizzle,
  Effect.gen(function* () {
    const { sqlite3, db } = (yield* Sqlite.Native) as Handle
    return drizzle(async (query, params, method) => {
      const rows = all(sqlite3, db, { query, params, safe: false, shape: "array" }) as unknown[][]
      if (method === "run") return { rows: [] }
      if (method === "get") return { rows: rows[0] ?? [] }
      return { rows }
    }) as unknown as Sqlite.DrizzleClient
  }),
)

export const layer = (config: Config) => {
  const native = nativeLayer(config)
  return Layer.merge(native, Layer.merge(sqliteLayer(config), drizzleLayer).pipe(Layer.provide(native))).pipe(
    Layer.provide(Reactivity.layer),
  )
}
