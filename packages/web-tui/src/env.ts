// The environment the browser build gives the core: what `process.env` holds
// in the worker and on the main thread (shims/globals.ts installs it).
export const ENV: Record<string, string> = {
  // norm's browser switch (packages/opencode/src/norm/host.ts): owallet is the
  // WebAssembly module behind http://owallet.internal, not a process.
  NORM_RUNTIME: "browser",
  NODE_ENV: "production",
  // Everything norm owns lives under one root in the virtual FS.
  NORM_HOME: "/norm",
  HOME: "/home/norm",
  USER: "norm",
  SHELL: "/bin/sh",
  TERM: "xterm-256color",
  PATH: "/usr/bin:/bin",
  PWD: "/workspace",
  TMPDIR: "/tmp",
  // Nothing to update, download or watch in a tab.
  OPENCODE_DISABLE_AUTOUPDATE: "1",
  OPENCODE_DISABLE_LSP_DOWNLOAD: "1",
  OPENCODE_DISABLE_FFF: "1",
  OPENCODE_EXPERIMENTAL_DISABLE_FILEWATCHER: "true",
  OPENCODE_DISABLE_EMBEDDED_WEB_UI: "1",
  // norm offers only the Overpay provider, whose models come from owallet's
  // /v1/models; the models.dev catalog (a ~1 MB download, CORS-readable) is
  // not needed and would only delay the first prompt.
  OPENCODE_DISABLE_MODELS_FETCH: "1",
  OPENCODE_DISABLE_SHARE: "1",
  // No ~/.claude to read; skills come from the marketplace.
  OPENCODE_DISABLE_CLAUDE_CODE: "1",
  OPENCODE_DISABLE_EXTERNAL_SKILLS: "1",
  OPENCODE_CLIENT: "web",
}

/** The project the browser build opens. */
export const WORKSPACE = "/workspace"
