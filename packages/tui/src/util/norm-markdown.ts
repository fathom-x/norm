import type { FiletypeParserOptions } from "@opentui/core"
// Relative paths on purpose: @opentui/core does not export its assets, and
// the override below has to name the built-in grammar and queries itself.
import wasm from "../../node_modules/@opentui/core/assets/markdown_inline/tree-sitter-markdown_inline.wasm" with { type: "file" }
import highlights from "../../node_modules/@opentui/core/assets/markdown_inline/highlights.scm" with { type: "file" }
import dollar from "./norm-dollar.scm" with { type: "file" }

// Fenced code, code spans and existing escapes pass through; a bare `$` is
// the last alternative.
const DOLLAR = /```[\s\S]*?(?:```|$)|~~~[\s\S]*?(?:~~~|$)|(`+)[\s\S]*?\1|\\[\s\S]|\$/g

/**
 * The TUI's markdown grammar reads everything between two dollar signs as
 * LaTeX and parses nothing inside it, so a line naming two prices lost its
 * bold and showed raw asterisks. The terminal cannot typeset math anyway,
 * and norm talks about money constantly: escape every dollar sign outside
 * code, and let `inlineParser`'s extra rule show `\$` as `$`.
 *
 * Only for concealed rendering. With concealment off nothing hides the
 * backslash, so pass the model's text through as written.
 */
export function plainDollars(text: string) {
  return text.replace(DOLLAR, (match) => (match === "$" ? "\\$" : match))
}

/** opentui's built-in inline markdown parser plus the rule that hides the escape. */
export const inlineParser: FiletypeParserOptions = {
  filetype: "markdown_inline",
  wasm,
  queries: { highlights: [highlights, dollar] },
}
