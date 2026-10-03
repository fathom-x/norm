import { describe, expect, test, beforeEach } from "bun:test"
import { coreBalanceCents, runSetup, type Status } from "./setup"

// A fake owallet-web /_mgmt: just enough state to walk the setup flow.
function fakeOwallet(initial: Partial<Status> & { password?: string; credits?: number } = {}) {
  const state = {
    initialized: false,
    unlocked: false,
    wallet: null as { npub: string } | null,
    overpay_linked: false,
    password: undefined as string | undefined,
    credits: 500,
    ...initial,
  }
  const calls: { path: string; body?: any }[] = []
  const owallet = async (path: string, init?: RequestInit) => {
    const body = init?.body ? JSON.parse(String(init.body)) : undefined
    calls.push({ path, body })
    const ok = (data: unknown) => Response.json(data)
    const fail = (status: number, code: string) => Response.json({ error: { code, message: code } }, { status })
    switch (path) {
      case "/_mgmt/status":
        return ok({
          initialized: state.initialized,
          unlocked: state.unlocked,
          wallet: state.wallet,
          overpay_linked: state.overpay_linked,
          rails_url: "https://overpay.example",
        })
      case "/_mgmt/init":
        state.initialized = state.unlocked = true
        state.password = body.password
        return ok({})
      case "/_mgmt/unlock":
        if (body.password !== state.password) return fail(401, "bad_password")
        state.unlocked = true
        return ok({})
      case "/_mgmt/generate":
        state.wallet = { npub: "npub1generated" }
        return ok({ npub: "npub1generated" })
      case "/_mgmt/import":
        state.wallet = { npub: "npub1imported" }
        return ok({ npub: "npub1imported" })
      case "/_mgmt/overpay/register":
        state.overpay_linked = true
        return ok({ account_number: "1234567890123456", formatted_account_number: "1234 5678 9012 3456" })
      case "/_mgmt/overpay/pkce/start":
        return ok({ authorize_url: "https://overpay.example/oauth/authorize?x=1", state: "st-1" })
      case "/_mgmt/credits":
        return ok({
          data: [
            { holder_type: "organization", organization_slug: "core", core: true, balance_cents: state.credits },
            { holder_type: "seller", seller_slug: "someone", balance_cents: 900 },
          ],
        })
      case "/_mgmt/overpay/pkce/finish":
        if (body.code !== "the-code" || body.state !== "st-1") return fail(400, "bad_code")
        state.overpay_linked = true
        return ok({})
    }
    return fail(404, "not_found")
  }
  return { owallet, calls, state }
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0))

/** Wait until the card shows `heading`, then return the root. */
async function screen(root: HTMLElement, heading: string) {
  for (let i = 0; i < 200; i++) {
    if (root.querySelector("h1")?.textContent === heading) return root
    await tick()
  }
  throw new Error(`never showed "${heading}"; last: "${root.querySelector("h1")?.textContent}" / ${root.textContent}`)
}

function fill(root: HTMLElement, values: Record<string, string>) {
  for (const [name, value] of Object.entries(values)) {
    const input = root.querySelector(`[name="${name}"]`) as HTMLInputElement
    input.value = value
  }
  ;(root.querySelector("form") as HTMLFormElement).requestSubmit()
}

function click(root: HTMLElement, label: string) {
  const button = [...root.querySelectorAll("button")].find((b) => b.textContent === label)
  if (!button) throw new Error(`no button "${label}" in: ${root.textContent}`)
  button.click()
}

describe("browser setup screen", () => {
  let root: HTMLElement
  beforeEach(() => {
    document.body.replaceChildren()
    root = document.createElement("div")
    document.body.append(root)
  })

  test("first run: password, new wallet, fresh Overpay account", async () => {
    const fake = fakeOwallet()
    const done = runSetup(root, { owallet: fake.owallet, origin: "https://norm.example" })

    await screen(root, "Set up your wallet")
    fill(root, { password: "pw1", confirm: "nope" })
    await tick()
    expect(root.querySelector("[role=alert]")?.textContent).toContain("do not match")
    fill(root, { password: "pw1", confirm: "pw1" })

    await screen(root, "Create or import a wallet")
    click(root, "Create a new wallet")

    await screen(root, "Connect to Overpay")
    expect(root.textContent).toContain("overpay.example")
    click(root, "Create a new Overpay account")

    await screen(root, "Your Overpay account")
    expect(root.textContent).toContain("1234 5678 9012 3456")
    click(root, "Continue")

    expect(await done).toEqual({ npub: "npub1generated", linked: true })
    // The admin password doubles as the wallet password, like the native auto-setup.
    expect(fake.calls.find((c) => c.path === "/_mgmt/generate")?.body).toEqual({ wallet_password: "pw1" })
    // The screen is gone so the TUI can take the page.
    expect(root.children).toHaveLength(0)
  })

  test("a later launch only asks for the password, and retries a wrong one", async () => {
    const fake = fakeOwallet({
      initialized: true,
      wallet: { npub: "npub1me" },
      overpay_linked: true,
      password: "right",
    })
    const done = runSetup(root, { owallet: fake.owallet, origin: "https://norm.example" })

    await screen(root, "Unlock your wallet")
    fill(root, { password: "wrong" })
    for (let i = 0; i < 20 && !root.querySelector("[role=alert]")?.textContent; i++) await tick()
    expect(root.querySelector("[role=alert]")?.textContent).toContain("didn't unlock")
    fill(root, { password: "right" })

    expect(await done).toEqual({ npub: "npub1me", linked: true })
    expect(fake.calls.map((c) => c.path).filter((p) => p !== "/_mgmt/status")).toEqual([
      "/_mgmt/unlock",
      "/_mgmt/unlock",
      "/_mgmt/credits",
    ])
  })

  test("importing a seed phrase normalizes whitespace", async () => {
    const fake = fakeOwallet({ initialized: true, unlocked: true, overpay_linked: true })
    const done = runSetup(root, { owallet: fake.owallet, origin: "https://norm.example" })
    await screen(root, "Create or import a wallet")
    click(root, "Import a seed phrase")
    await screen(root, "Import a seed phrase")
    fill(root, { mnemonic: "  abandon   abandon\nabout  " })
    expect(await done).toEqual({ npub: "npub1imported", linked: true })
    expect(fake.calls.find((c) => c.path === "/_mgmt/import")?.body).toEqual({ mnemonic: "abandon abandon about" })
  })

  test("linking an existing account goes through the popup and its callback message", async () => {
    const fake = fakeOwallet({ initialized: true, unlocked: true, wallet: { npub: "npub1me" } })
    let handler: ((event: MessageEvent) => void) | undefined
    const opened: string[] = []
    const done = runSetup(root, {
      owallet: fake.owallet,
      origin: "https://norm.example",
      openPopup: (url) => {
        opened.push(url)
        return {} as Window
      },
      listen: (h) => {
        handler = h
        return () => (handler = undefined)
      },
    })

    await screen(root, "Connect to Overpay")
    click(root, "Use my existing Overpay account")
    await screen(root, "Log in to Overpay")
    expect(fake.calls.find((c) => c.path === "/_mgmt/overpay/pkce/start")?.body).toEqual({
      redirect_uri: "https://norm.example/oauth/callback.html",
    })
    click(root, "Open Overpay login")
    expect(opened).toEqual(["https://overpay.example/oauth/authorize?x=1"])

    // Messages from another origin, of another type, or for another flow are ignored.
    handler!({
      origin: "https://evil.example",
      data: { type: "overpay-oauth", code: "x", state: "st-1" },
    } as MessageEvent)
    handler!({ origin: "https://norm.example", data: { type: "other", code: "x", state: "st-1" } } as MessageEvent)
    handler!({
      origin: "https://norm.example",
      data: { type: "overpay-oauth", code: "x", state: "stale" },
    } as MessageEvent)
    expect(fake.calls.some((c) => c.path === "/_mgmt/overpay/pkce/finish")).toBe(false)

    handler!({
      origin: "https://norm.example",
      data: { type: "overpay-oauth", code: "the-code", state: "st-1" },
    } as MessageEvent)
    expect(await done).toEqual({ npub: "npub1me", linked: true })
  })

  test("an empty credit balance gets a top-up hint before norm starts", async () => {
    const fake = fakeOwallet({
      initialized: true,
      unlocked: true,
      wallet: { npub: "npub1me" },
      overpay_linked: true,
      credits: 0,
    })
    const done = runSetup(root, { owallet: fake.owallet, origin: "https://norm.example" })
    await screen(root, "Add marketplace credits")
    expect(root.textContent).toContain("Lightning invoice")
    click(root, "Start norm")
    expect(await done).toEqual({ npub: "npub1me", linked: true })
  })

  test("a funded account starts without the hint, and only core credits count", async () => {
    const fake = fakeOwallet({
      initialized: true,
      unlocked: true,
      wallet: { npub: "npub1me" },
      overpay_linked: true,
      credits: 0,
    })
    // Seller-specific credits (900¢ in the fake) cannot pay for inference.
    expect(coreBalanceCents({ data: [{ core: false, balance_cents: 900 }] })).toBe(0)
    expect(
      coreBalanceCents({
        data: [
          { core: true, balance_cents: 250 },
          { core: true, balance_cents: 50 },
        ],
      }),
    ).toBe(300)
    fake.state.credits = 300
    expect(await runSetup(root, { owallet: fake.owallet, origin: "https://norm.example" })).toEqual({
      npub: "npub1me",
      linked: true,
    })
  })

  test("a forgotten password can start over, and Back returns to a working unlock form", async () => {
    const fake = fakeOwallet({
      initialized: true,
      wallet: { npub: "npub1me" },
      overpay_linked: true,
      password: "right",
    })
    let resets = 0
    const done = runSetup(root, {
      owallet: fake.owallet,
      origin: "https://norm.example",
      reset: async () => {
        resets++
      },
    })
    await screen(root, "Unlock your wallet")
    click(root, "Forgot the password?")
    await screen(root, "Start over?")
    click(root, "Back")
    await screen(root, "Unlock your wallet")
    click(root, "Forgot the password?")
    await screen(root, "Start over?")
    click(root, "Delete and start over")
    await tick()
    expect(resets).toBe(1)
    click(root, "Back")
    await screen(root, "Unlock your wallet")
    fill(root, { password: "right" })
    expect(await done).toEqual({ npub: "npub1me", linked: true })
  })

  test("without a reset hook the unlock screen offers no start-over", async () => {
    const fake = fakeOwallet({
      initialized: true,
      wallet: { npub: "npub1me" },
      overpay_linked: true,
      password: "right",
    })
    void runSetup(root, { owallet: fake.owallet, origin: "https://norm.example" })
    await screen(root, "Unlock your wallet")
    expect(root.textContent).not.toContain("Forgot the password?")
  })

  test("an owallet that cannot start is explained, and norm starts without it", async () => {
    const done = runSetup(root, {
      owallet: async () =>
        Response.json({ error: { code: "owallet_unavailable", message: "owallet-web is not in this build" } }, { status: 503 }),
      origin: "https://norm.example",
    })
    await screen(root, "The wallet could not start")
    expect(root.textContent).toContain("owallet-web is not in this build")
    click(root, "Continue without the wallet")
    expect(await done).toEqual({ linked: false })
  })

  test("Overpay can be skipped; norm still starts", async () => {
    const fake = fakeOwallet({ initialized: true, unlocked: true, wallet: { npub: "npub1me" } })
    const done = runSetup(root, { owallet: fake.owallet, origin: "https://norm.example" })
    await screen(root, "Connect to Overpay")
    click(root, "Not now")
    expect(await done).toEqual({ npub: "npub1me", linked: false })
  })
})
