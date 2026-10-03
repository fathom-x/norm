// Builds the E2B template the cloud-sandbox demo runs in ("norm-demo" by
// default): E2B's base image (Debian, user `user`), the native `norm` and
// `owallet` binaries under NORM_HOME=/home/user/.norm, the norm-demo wrapper
// the broker starts in a PTY, and the demo workspace.
//
//   bun run template:build                  local binaries (default)
//   bun run template:build --source release install from GitHub releases
//   bun run template:build --name norm-demo-dev
//
// Local binaries (no release needed — cutting one is a deliberate step):
//   NORM_BIN    default packages/opencode/dist/norm-linux-x64/bin/norm
//               (cd packages/opencode && NORM_BUILD_TARGETS=linux-x64 bun script/build.ts)
//   OWALLET_BIN default owallet/target/x86_64-unknown-linux-musl/release/owallet
//               (cd owallet && cargo build --release -p owallet --features dev-envs
//                --target x86_64-unknown-linux-musl)
// Needs E2B_API_KEY.
import { cp, mkdir, rm, writeFile } from "node:fs/promises"
import { existsSync } from "node:fs"
import path from "node:path"
import { parseArgs } from "node:util"
import { defaultBuildLogger, Template } from "e2b"
import { DEMO_WORKSPACE } from "../../web-tui/src/demo-workspace"

const here = import.meta.dirname
const repo = path.resolve(here, "../../..")
const { values } = parseArgs({
  options: {
    source: { type: "string", default: "local" },
    name: { type: "string", default: process.env.E2B_TEMPLATE ?? "norm-demo" },
  },
})

export const NORM_HOME = "/home/user/.norm"
export const WORKSPACE = "/home/user/workspace"

if (!process.env.E2B_API_KEY) throw new Error("E2B_API_KEY is not set")

// Everything copied into the image is staged here (gitignored).
const context = path.join(here, ".context")
await rm(context, { recursive: true, force: true })
await mkdir(path.join(context, "workspace"), { recursive: true })
await cp(path.join(here, "norm-demo.sh"), path.join(context, "norm-demo"))
for (const [relative, content] of Object.entries(DEMO_WORKSPACE)) {
  const file = path.join(context, "workspace", relative)
  await mkdir(path.dirname(file), { recursive: true })
  await writeFile(file, content)
}

let template = Template({ fileContextPath: context })
  .fromBaseImage()
  .setEnvs({
    NORM_HOME,
    NORM_WORKSPACE: WORKSPACE,
    NORM_OWALLET_ENV: "staging",
    TERM: "xterm-256color",
    COLORTERM: "truecolor",
  })
  .makeDir(`${NORM_HOME}/bin`)

if (values.source === "local") {
  const norm = process.env.NORM_BIN ?? path.join(repo, "packages/opencode/dist/norm-linux-x64/bin/norm")
  const owallet =
    process.env.OWALLET_BIN ?? path.join(repo, "owallet/target/x86_64-unknown-linux-musl/release/owallet")
  for (const [what, file] of [
    ["norm", norm],
    ["owallet", owallet],
  ])
    if (!existsSync(file)) throw new Error(`${what} binary not found at ${file} (see the header of this file)`)
  await cp(norm, path.join(context, "norm"))
  await cp(owallet, path.join(context, "owallet"))
  template = template
    .copy("norm", `${NORM_HOME}/bin/norm`, { mode: 0o755 })
    .copy("owallet", `${NORM_HOME}/bin/owallet`, { mode: 0o755 })
} else if (values.source === "release") {
  // The installer picks the newest `v*` norm and `owallet-v*` owallet.
  template = template.runCmd(
    `curl -fsSL https://raw.githubusercontent.com/fathom-x/norm/main/install | NORM_HOME=${NORM_HOME} bash`,
  )
} else throw new Error(`--source must be local or release, not ${values.source}`)

template = template
  .copy("norm-demo", "/usr/local/bin/norm-demo", { mode: 0o755, user: "root" })
  .copy("workspace", WORKSPACE)
  // norm-linux-x64 needs AVX2; fail the build here rather than at first run.
  .runCmd("grep -q avx2 /proc/cpuinfo || { echo 'this CPU lacks AVX2: build norm-linux-x64-baseline' >&2; exit 1; }")
  // Warm the binaries (and fail early if one does not run here).
  .runCmd([`${NORM_HOME}/bin/norm --version`, `${NORM_HOME}/bin/owallet --version`])

const info = await Template.build(template, values.name!, {
  cpuCount: 2,
  memoryMB: 2048,
  onBuildLogs: defaultBuildLogger(),
})
console.log(`built ${values.name}:`, JSON.stringify(info))
await rm(context, { recursive: true, force: true })
