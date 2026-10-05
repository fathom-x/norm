import { describe, expect, test } from "bun:test"
import { plainDollars } from "../../src/util/norm-markdown"

describe("plainDollars", () => {
  test("escapes dollar signs in prose and tables", () => {
    expect(plainDollars("cost **$0.0012 total**; the $0.15 figure")).toBe("cost **\\$0.0012 total**; the \\$0.15 figure")
    expect(plainDollars("| $0.15 | $0.00 |")).toBe("| \\$0.15 | \\$0.00 |")
  })

  test("leaves code alone", () => {
    expect(plainDollars("run `echo $HOME` for $5")).toBe("run `echo $HOME` for \\$5")
    expect(plainDollars("``a ` $b`` $c")).toBe("``a ` $b`` \\$c")
    expect(plainDollars("```sh\necho $A $B\n```\n$1")).toBe("```sh\necho $A $B\n```\n\\$1")
    expect(plainDollars("~~~\n$A\n~~~")).toBe("~~~\n$A\n~~~")
  })

  test("leaves a code block that is still streaming alone", () => {
    expect(plainDollars("$1\n```sh\necho $A")).toBe("\\$1\n```sh\necho $A")
  })

  test("does not escape twice", () => {
    expect(plainDollars("already \\$5")).toBe("already \\$5")
    expect(plainDollars(plainDollars("$5 and $6"))).toBe("\\$5 and \\$6")
  })
})
