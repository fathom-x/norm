// The project a first-time visitor finds at /workspace: small enough to read
// in one go, with something for read/grep/edit to do.
export const DEMO_WORKSPACE: Record<string, string> = {
  "README.md": `# Tip calculator

A tiny TypeScript project to try norm on. Ask it to:

- explain what \`src/tip.ts\` does
- add rounding to the nearest cent
- write a test for \`splitBill\`
- find every TODO in the project

Everything here lives in your browser (the origin private file system);
nothing is uploaded unless you ask the agent to send it somewhere.
`,
  "package.json": `{
  "name": "tip-calculator",
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "test": "bun test"
  }
}
`,
  "src/tip.ts": `export type Bill = {
  subtotal: number
  tipPercent: number
  people: number
}

// TODO: round to the nearest cent
export function tip(bill: Bill) {
  return (bill.subtotal * bill.tipPercent) / 100
}

export function splitBill(bill: Bill) {
  if (bill.people <= 0) throw new Error("people must be positive")
  return (bill.subtotal + tip(bill)) / bill.people
}
`,
  "src/format.ts": `// TODO: support other currencies
export function formatUsd(amount: number) {
  return "$" + amount.toFixed(2)
}
`,
  "src/main.ts": `import { formatUsd } from "./format"
import { splitBill } from "./tip"

const share = splitBill({ subtotal: 84.5, tipPercent: 18, people: 3 })
console.log("Each person pays", formatUsd(share))
`,
  ".gitignore": "node_modules/\ndist/\n",
}
