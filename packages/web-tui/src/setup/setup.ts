// The browser build's first-run and launch flow, shown before the TUI takes
// the page — the counterpart of norm.ts's TTY prompts (firstRunWalletSetup,
// ensureServePassword, ensureOverpayConnected), which cannot run without a
// terminal. It talks only to owallet-web's /_mgmt routes, through whatever
// fetch the caller hands it (the page's router to the core worker).
//
// Steps, each skipped when already done:
//   1. create the wallet database (choose the admin password) or unlock it —
//      the password is never stored; every launch asks, like the native TTY
//      gate does unless OWALLET_PASSWORD is exported
//   2. generate a wallet or import a seed phrase (the phrase is never shown)
//   3. link Overpay: a fresh account (NIP-98 register), an existing one
//      (PKCE in a popup → /oauth/callback.html), or later
//   4. with an empty credit balance, say how to top up (a Lightning invoice
//      from inside norm) before starting
// Resolves once the wallet is unlocked; linking is optional for launch. A
// forgotten password can only be answered by starting over: `reset` (when the
// page provides it) deletes this browser's wallet and reloads.

export type Status = {
  version?: string
  initialized?: boolean
  unlocked?: boolean
  wallet?: { npub?: string } | null
  overpay_linked?: boolean
  env?: string
  rails_url?: string
}

export type SetupResult = { npub?: string; linked: boolean }

export type SetupDeps = {
  /** fetch against owallet-web; `path` starts with "/_mgmt/". */
  owallet: (path: string, init?: RequestInit) => Promise<Response>
  /** The page origin; the OAuth redirect is `${origin}/oauth/callback.html`. */
  origin: string
  /** Opens the Overpay login popup (must run inside the click handler). */
  openPopup?: (url: string) => Window | null
  /** Subscribes to window messages; returns an unsubscribe. */
  listen?: (handler: (event: MessageEvent) => void) => () => void
  /**
   * Deletes this browser's wallet and norm state, then reloads the page. The
   * page owns it: the worker holds the storage open, so it must be stopped
   * first. Without it the unlock screen offers no way to start over.
   */
  reset?: () => Promise<void>
}

export class OwalletError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly status: number,
  ) {
    super(message)
  }
}

const POPUP_FEATURES = "popup,width=520,height=720"

export async function runSetup(root: HTMLElement, deps: SetupDeps): Promise<SetupResult> {
  const ui = new SetupView(root)
  const api = client(deps.owallet)
  let password: string | undefined

  try {
    let status = await ui.busy("Starting the wallet…", () => api.status())

    if (!status.initialized) {
      password = await ui.form({
        title: "Set up your wallet",
        intro: [
          "norm pays for every model turn and tool call from a wallet that lives in this browser.",
          "Choose the wallet's admin password. It encrypts the wallet at rest and is asked for on every launch; it is never stored.",
        ],
        fields: [
          { name: "password", label: "Admin password", type: "password", autocomplete: "new-password" },
          { name: "confirm", label: "Confirm password", type: "password", autocomplete: "new-password" },
        ],
        submit: "Create wallet",
        run: async (v) => {
          if (!v.password) throw new Error("The password cannot be empty.")
          if (v.password !== v.confirm) throw new Error("The passwords do not match.")
          await api.post("/_mgmt/init", { password: v.password })
          return v.password
        },
      })
    } else if (!status.unlocked) {
      password = await ui.form({
        title: "Unlock your wallet",
        intro: ["Enter the wallet's admin password to start norm."],
        fields: [{ name: "password", label: "Admin password", type: "password", autocomplete: "current-password" }],
        submit: "Unlock",
        secondary: deps.reset && {
          label: "Forgot the password?",
          onClick: () => confirmReset(ui, deps.reset!),
        },
        run: async (v) => {
          if (!v.password) throw new Error("Enter the password.")
          await api.post("/_mgmt/unlock", { password: v.password }).catch((error) => {
            if (error instanceof OwalletError && error.code === "bad_password")
              throw new Error("That password didn't unlock the wallet. Try again.")
            throw error
          })
          return v.password
        },
      })
    }

    status = await api.status()
    if (!status.wallet?.npub) {
      const choice = await ui.choose({
        title: "Create or import a wallet",
        intro: ["A new wallet gets a fresh seed phrase. It is not displayed here."],
        options: [
          { value: "generate", label: "Create a new wallet", primary: true },
          { value: "import", label: "Import a seed phrase" },
        ],
      })
      if (choice === "generate") {
        await ui.busy("Creating the wallet…", () => api.post("/_mgmt/generate", walletPassword(password)))
      } else {
        await ui.form({
          title: "Import a seed phrase",
          intro: ["Paste the 12 or 24 words, separated by spaces."],
          fields: [{ name: "mnemonic", label: "Seed phrase", type: "textarea", autocomplete: "off" }],
          submit: "Import",
          run: async (v) => {
            const mnemonic = v.mnemonic.trim().split(/\s+/).join(" ")
            if (!mnemonic) throw new Error("Paste the seed phrase.")
            await api.post("/_mgmt/import", { mnemonic, ...walletPassword(password) })
          },
        })
      }
      status = await api.status()
    }

    if (!status.overpay_linked) {
      status = await linkOverpay(ui, api, deps, status)
    }
    if (status.overpay_linked) await creditsHint(ui, api)

    return { npub: status.wallet?.npub, linked: status.overpay_linked === true }
  } finally {
    ui.clear()
  }
}

/** Core credits are what norm spends; an empty balance gets one screen saying how to add some. */
async function creditsHint(ui: SetupView, api: Client) {
  const cents = await api
    .get("/_mgmt/credits")
    .then((body: any) => coreBalanceCents(body))
    .catch(() => undefined)
  if (cents === undefined || cents > 0) return
  await ui.notice({
    title: "Add marketplace credits",
    lines: [
      "Your Overpay balance is $0.00. Every model turn and tool call is paid from it.",
      "To add some, ask norm once it starts — for example “load $5 of credits”. It answers with a Lightning invoice you can pay from any Lightning wallet; the credits arrive when it settles.",
    ],
    button: "Start norm",
  })
}

/** The credits norm can spend anywhere: the core organisation's, summed. */
export function coreBalanceCents(body: any): number | undefined {
  const rows: any[] = Array.isArray(body?.data) ? body.data : Array.isArray(body) ? body : []
  if (!rows.length) return 0
  return rows
    .filter((row) => row?.core === true)
    .reduce((sum, row) => sum + (Number.isFinite(row?.balance_cents) ? row.balance_cents : 0), 0)
}

function confirmReset(ui: SetupView, reset: () => Promise<void>) {
  // The unlock form stays pending underneath; "Back" puts its own nodes (and
  // their listeners) back rather than rebuilding it.
  const unlock = ui.snapshot()
  ui.action({
    title: "Start over?",
    intro: [
      "The password cannot be recovered. Starting over deletes the wallet stored in this browser — its keys, provider key and norm's conversations here.",
      "An Overpay account stays usable: log in again with its account number. On-chain funds need the wallet's seed phrase.",
    ],
    button: "Delete and start over",
    onClick: () => {
      ui.flash("Deleting…")
      reset().catch((error) => ui.flash(`Could not delete the wallet: ${message(error)}`))
    },
    cancel: "Back",
    onCancel: () => ui.restore(unlock),
  })
}

async function linkOverpay(ui: SetupView, api: Client, deps: SetupDeps, status: Status): Promise<Status> {
  const where = status.rails_url ? new URL(status.rails_url).host : "Overpay"
  for (;;) {
    const choice = await ui.choose({
      title: "Connect to Overpay",
      intro: [
        `norm buys inference and tools on the Overpay marketplace (${where}). Connect this wallet to an account to start.`,
      ],
      options: [
        { value: "register", label: "Create a new Overpay account", primary: true },
        { value: "link", label: "Use my existing Overpay account" },
        { value: "later", label: "Not now" },
      ],
    })
    if (choice === "later") return status
    try {
      if (choice === "register") {
        const account: any = await ui.busy("Creating your Overpay account…", () =>
          api.post("/_mgmt/overpay/register", {}),
        )
        const number = account?.formatted_account_number ?? account?.account_number
        if (number) {
          await ui.notice({
            title: "Your Overpay account",
            lines: [
              `Account number: ${number}`,
              "It is your login on the Overpay website. Keep it somewhere safe; it is not shown again.",
            ],
            button: "Continue",
          })
        }
      } else {
        await linkExisting(ui, api, deps)
      }
      const next = await api.status()
      if (next.overpay_linked) return next
      ui.flash("The connection didn't complete. Try again or choose “Not now”.")
    } catch (error) {
      ui.flash(message(error))
    }
  }
}

async function linkExisting(ui: SetupView, api: Client, deps: SetupDeps): Promise<void> {
  const redirect = `${deps.origin}/oauth/callback.html`
  const start: any = await api.post("/_mgmt/overpay/pkce/start", { redirect_uri: redirect })
  const url = start?.authorize_url
  const state = start?.state
  if (typeof url !== "string") throw new Error("owallet did not return an Overpay login URL.")

  const listen =
    deps.listen ??
    ((handler) => {
      window.addEventListener("message", handler)
      return () => window.removeEventListener("message", handler)
    })
  const open = deps.openPopup ?? ((target: string) => window.open(target, "overpay-login", POPUP_FEATURES))

  const code = await new Promise<string>((resolve, reject) => {
    const stop = listen((event) => {
      if (event.origin !== deps.origin) return
      const data = event.data
      if (!data || data.type !== "overpay-oauth") return
      if (state && data.state !== state) return
      stop()
      if (data.error || !data.code)
        reject(new Error(`Overpay did not authorize the wallet (${data.error ?? "no code"}).`))
      else resolve(data.code)
    })
    // A popup must open from the click itself, or browsers block it.
    ui.action({
      title: "Log in to Overpay",
      intro: ["A window opens on Overpay: log in, then approve this wallet. This page continues by itself afterwards."],
      button: "Open Overpay login",
      onClick: () => {
        if (!open(url)) ui.flash("The browser blocked the window. Allow pop-ups for this page and try again.")
      },
      cancel: "Back",
      onCancel: () => {
        stop()
        reject(new Error("Overpay login cancelled."))
      },
    })
  })
  await ui.busy("Linking the wallet…", () => api.post("/_mgmt/overpay/pkce/finish", { code, state }))
}

function walletPassword(password: string | undefined) {
  // Same choice as the native auto-setup: the per-wallet (dashboard) password
  // defaults to the admin password the user just typed.
  return password ? { wallet_password: password } : {}
}

type Client = ReturnType<typeof client>

function client(owallet: SetupDeps["owallet"]) {
  const call = async (path: string, init?: RequestInit) => {
    const res = await owallet(path, init)
    const body: any = await res.json().catch(() => undefined)
    if (!res.ok) {
      const code = body?.error?.code ?? `http_${res.status}`
      throw new OwalletError(code, body?.error?.message ?? `owallet answered ${res.status}`, res.status)
    }
    return body
  }
  return {
    status: () => call("/_mgmt/status") as Promise<Status>,
    get: (path: string) => call(path),
    post: (path: string, body: unknown) =>
      call(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) }),
  }
}

function message(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}

// ---------------------------------------------------------------------------
// View: one card at a time, plain DOM (no framework), keyboard-first.

type Field = { name: string; label: string; type: "password" | "text" | "textarea"; autocomplete: string }

class SetupView {
  private card: HTMLElement
  private status: HTMLElement

  constructor(private root: HTMLElement) {
    root.classList.add("norm-setup")
    this.card = el("section", { class: "norm-setup-card", "aria-live": "polite" })
    this.status = el("p", { class: "norm-setup-error", role: "alert" })
    root.replaceChildren(el("div", { class: "norm-setup-brand" }, "norm"), this.card)
  }

  clear() {
    this.root.replaceChildren()
    this.root.classList.remove("norm-setup")
  }

  /** The card's current nodes, for `restore`. */
  snapshot(): Node[] {
    return [...this.card.childNodes]
  }

  restore(nodes: Node[]) {
    this.flash("")
    this.card.replaceChildren(...nodes)
    ;(this.card.querySelector("input, textarea, button") as HTMLElement | null)?.focus()
  }

  flash(text: string) {
    this.status.textContent = text
  }

  async busy<T>(text: string, run: () => Promise<T>): Promise<T> {
    this.render(text, [], [el("p", { class: "norm-setup-busy" }, "Working…")])
    return run()
  }

  form<T>(spec: {
    title: string
    intro: string[]
    fields: Field[]
    submit: string
    secondary?: { label: string; onClick: () => void }
    run: (values: Record<string, string>) => Promise<T>
  }): Promise<T> {
    return new Promise((resolve) => {
      const form = el("form", { class: "norm-setup-form", novalidate: "" }) as HTMLFormElement
      const inputs = spec.fields.map((field) => {
        const id = `norm-setup-${field.name}`
        const input =
          field.type === "textarea"
            ? (el("textarea", {
                id,
                name: field.name,
                rows: "3",
                autocomplete: field.autocomplete,
                spellcheck: "false",
              }) as HTMLTextAreaElement)
            : (el("input", {
                id,
                name: field.name,
                type: field.type,
                autocomplete: field.autocomplete,
              }) as HTMLInputElement)
        form.append(el("label", { for: id }, field.label), input)
        return input
      })
      const button = el("button", { type: "submit", class: "primary" }, spec.submit) as HTMLButtonElement
      const actions = el("div", { class: "norm-setup-actions" }, button)
      if (spec.secondary) {
        const secondary = el("button", { type: "button", class: "link" }, spec.secondary.label)
        secondary.addEventListener("click", spec.secondary.onClick)
        actions.append(secondary)
      }
      form.append(actions)
      form.addEventListener("submit", async (event) => {
        event.preventDefault()
        const values = Object.fromEntries(inputs.map((input) => [input.name, input.value]))
        button.disabled = true
        this.flash("")
        try {
          resolve(await spec.run(values))
        } catch (error) {
          this.flash(message(error))
          button.disabled = false
          inputs[0]?.focus()
        }
      })
      this.render(spec.title, spec.intro, [form])
      inputs[0]?.focus()
    })
  }

  choose(spec: {
    title: string
    intro: string[]
    options: { value: string; label: string; primary?: boolean }[]
  }): Promise<string> {
    return new Promise((resolve) => {
      const buttons = spec.options.map((option) => {
        const button = el("button", { type: "button", ...(option.primary && { class: "primary" }) }, option.label)
        button.addEventListener("click", () => resolve(option.value))
        return button
      })
      this.render(spec.title, spec.intro, [el("div", { class: "norm-setup-actions" }, ...buttons)], false)
      buttons[0]?.focus()
    })
  }

  notice(spec: { title: string; lines: string[]; button: string }): Promise<void> {
    return new Promise((resolve) => {
      const button = el("button", { type: "button", class: "primary" }, spec.button)
      button.addEventListener("click", () => resolve())
      this.render(spec.title, spec.lines, [el("div", { class: "norm-setup-actions" }, button)])
      button.focus()
    })
  }

  action(spec: {
    title: string
    intro: string[]
    button: string
    onClick: () => void
    cancel: string
    onCancel: () => void
  }) {
    const go = el("button", { type: "button", class: "primary" }, spec.button)
    go.addEventListener("click", spec.onClick)
    const back = el("button", { type: "button" }, spec.cancel)
    back.addEventListener("click", spec.onCancel)
    this.render(spec.title, spec.intro, [el("div", { class: "norm-setup-actions" }, go, back)])
    go.focus()
  }

  private render(title: string, intro: string[], body: Node[], clearError = true) {
    if (clearError) this.flash("")
    this.card.replaceChildren(el("h1", {}, title), ...intro.map((line) => el("p", {}, line)), ...body, this.status)
  }
}

function el(tag: string, attrs: Record<string, string> = {}, ...children: (Node | string)[]): HTMLElement {
  const node = document.createElement(tag)
  for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value)
  node.append(...children)
  return node
}
