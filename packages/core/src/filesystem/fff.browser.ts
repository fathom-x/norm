// Browser variant of #fff: there is no native fff, so this is the same
// always-unavailable picker the node runtime uses. FileSystemSearch then takes
// its ripgrep layer, which in the browser build is ripgrep.browser.ts (a JS
// walk of the virtual FS) with fuzzysort for find.
export * from "./fff.node"
