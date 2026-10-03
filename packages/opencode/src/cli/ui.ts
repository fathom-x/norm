import { EOL } from "os"
import { Schema } from "effect"
import { InstallationVersion } from "@opencode-ai/core/installation/version"


export class CancelledError extends Schema.TaggedErrorClass<CancelledError>()("UICancelledError", {}) {}

export const Style = {
  TEXT_HIGHLIGHT: "\x1b[96m",
  TEXT_HIGHLIGHT_BOLD: "\x1b[96m\x1b[1m",
  TEXT_DIM: "\x1b[90m",
  TEXT_DIM_BOLD: "\x1b[90m\x1b[1m",
  TEXT_NORMAL: "\x1b[0m",
  TEXT_NORMAL_BOLD: "\x1b[1m",
  TEXT_WARNING: "\x1b[93m",
  TEXT_WARNING_BOLD: "\x1b[93m\x1b[1m",
  TEXT_DANGER: "\x1b[91m",
  TEXT_DANGER_BOLD: "\x1b[91m\x1b[1m",
  TEXT_SUCCESS: "\x1b[92m",
  TEXT_SUCCESS_BOLD: "\x1b[92m\x1b[1m",
  TEXT_INFO: "\x1b[94m",
  TEXT_INFO_BOLD: "\x1b[94m\x1b[1m",
}

export function println(...message: string[]) {
  print(...message)
  process.stderr.write(EOL)
}

export function print(...message: string[]) {
  blank = false
  process.stderr.write(message.join(" "))
}

let blank = false
export function empty() {
  if (blank) return
  println("" + Style.TEXT_NORMAL)
  blank = true
}

// norm: the banner is the name and version on one line (no ASCII art).
export function logo(pad?: string) {
  const text = `Norm ${InstallationVersion}`
  if (!process.stdout.isTTY && !process.stderr.isTTY) return (pad ?? "") + text
  return `${pad ?? ""}\x1b[1mNorm\x1b[0m \x1b[90m${InstallationVersion}\x1b[0m`
}

export async function input(prompt: string): Promise<string> {
  const readline = require("readline")
  const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout,
  })

  return new Promise((resolve) => {
    rl.question(prompt, (answer: string) => {
      rl.close()
      resolve(answer.trim())
    })
  })
}

/**
 * Like `input`, but with no echo — for passwords. Reads raw keystrokes so
 * nothing (not even asterisks) hits the terminal; backspace edits, Enter
 * submits, Ctrl-C re-raises SIGINT after restoring the terminal.
 */
export function inputSecret(prompt: string): Promise<string> {
  process.stdout.write(prompt)
  const stdin = process.stdin
  const wasRaw = stdin.isRaw
  stdin.setRawMode?.(true)
  stdin.resume()
  return new Promise((resolve) => {
    let value = ""
    const finish = (result: string, signal?: NodeJS.Signals) => {
      stdin.off("data", onData)
      stdin.setRawMode?.(wasRaw ?? false)
      stdin.pause()
      process.stdout.write(EOL)
      if (signal) process.kill(process.pid, signal)
      resolve(result)
    }
    const onData = (chunk: Buffer) => {
      for (const char of chunk.toString("utf8")) {
        if (char === "\r" || char === "\n") return finish(value)
        if (char === "\u0003") return finish("", "SIGINT")
        if (char === "\u007f" || char === "\b") {
          value = value.slice(0, -1)
          continue
        }
        if (char >= " ") value += char
      }
    }
    stdin.on("data", onData)
  })
}

export function error(message: string) {
  if (message.startsWith("Error: ")) {
    message = message.slice("Error: ".length)
  }
  println(Style.TEXT_DANGER_BOLD + "Error: " + Style.TEXT_NORMAL + message)
}

export function markdown(text: string): string {
  return text
}

export * as UI from "./ui"
