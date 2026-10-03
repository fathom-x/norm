# norm-demo E2B template

What every cloud-sandbox visitor gets: E2B's base image (Debian, user
`user`) with

- `norm` and `owallet` in `/home/user/.norm/bin` (`NORM_HOME=/home/user/.norm`,
  so the wallet, `auth.json` and sessions all live under it),
- `/usr/local/bin/norm-demo` (`norm-demo.sh`): runs norm in
  `/home/user/workspace` and starts it again when it exits — the program the
  broker runs in the sandbox's PTY,
- the demo workspace (the same files as the browser build's,
  `packages/web-tui/src/demo-workspace.ts`),
- `TERM=xterm-256color`, `COLORTERM=truecolor`, `NORM_OWALLET_ENV=staging`.

The broker adds `OWALLET_PASSWORD` per sandbox, so norm's first run asks no
password and goes straight to "connect to Overpay" (new account — no login —
by default, then the demo credits on a $0 balance).

## Build

```sh
export E2B_API_KEY=…
# binaries built from this checkout (no release needed)
(cd packages/opencode && NORM_BUILD_TARGETS=linux-x64 bun script/build.ts)
(cd owallet && cargo build --release -p owallet --features dev-envs --target x86_64-unknown-linux-musl)
cd packages/web-sandbox && bun run template:build
# or from the latest GitHub releases
bun run template:build --source release
```

`NORM_BIN` / `OWALLET_BIN` point at other binaries; `--name` builds another
template (the broker's `E2B_TEMPLATE`). The build fails early if the CPU lacks
AVX2 (`norm-linux-x64` needs it) or a binary does not run.
