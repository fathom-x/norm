// `@opentui/solid/runtime-plugin-support[/configure]` for the browser build.
// The real module registers a Bun loader plugin so external TUI plugins
// (npm packages, .opencode/plugin files) can be imported at runtime; a tab
// has no module loader to extend and no packages to load, so it is a no-op
// and only the TUI's built-in plugins run.
export function ensureRuntimePluginSupport(_options?: unknown) {
  return false
}

export default { ensureRuntimePluginSupport }
