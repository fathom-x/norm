// One tab at a time. The browser build's stores — the ZenFS tree, the
// opencode database's sahpool, owallet-web's pool — are OPFS sync access
// handles, which only one context can hold. A second tab would fail deep in
// the worker with an opaque error, so the page takes a Web Lock first: the
// first tab holds it for its lifetime, a second one says why it is waiting
// and takes over by itself when the first one closes.

export const TAB_LOCK = "norm-browser-build"

type Locks = Pick<LockManager, "request">

/**
 * Resolves once this tab owns norm's stores. `onWait` runs if another tab
 * has them; the promise then resolves when that tab goes away.
 */
export function claimTab(onWait: () => void, locks: Locks | undefined = globalThis.navigator?.locks): Promise<void> {
  // No Web Locks (very old browsers): nothing to coordinate with.
  if (!locks?.request) return Promise.resolve()
  return new Promise((granted) => {
    // Held until the page unloads.
    const hold = () => {
      granted()
      return new Promise<void>(() => {})
    }
    void locks.request(TAB_LOCK, { ifAvailable: true }, (lock) => {
      if (lock) return hold()
      onWait()
      void locks.request(TAB_LOCK, hold)
      return undefined
    })
  })
}

export function renderOtherTab(root: HTMLElement) {
  root.dataset.state = "other-tab"
  const box = document.createElement("div")
  box.className = "norm-other-tab"
  box.setAttribute("role", "status")
  const title = document.createElement("h1")
  title.textContent = "norm is open in another tab"
  const body = document.createElement("p")
  body.textContent =
    "Its wallet and conversations can only be used by one tab at a time. Close the other tab and this one takes over by itself."
  box.append(title, body)
  root.replaceChildren(box)
}
