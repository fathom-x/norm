import { createSignal } from "solid-js"

// norm: ctrl+c exits only when pressed twice. The first press (on an empty
// prompt; with text it clears the prompt instead) arms exit for
// ARM_MS and the prompt's hints row says "ctrl+c again to exit"; a second
// press while armed exits. Wired in app.tsx.
const ARM_MS = 2000

const [armed, setArmed] = createSignal(false)
let timer: ReturnType<typeof setTimeout> | undefined

export const NormExit = {
  armed,
  /** First press arms (returns false); a press while armed returns true. */
  press(): boolean {
    if (armed()) {
      if (timer) clearTimeout(timer)
      setArmed(false)
      return true
    }
    setArmed(true)
    timer = setTimeout(() => setArmed(false), ARM_MS)
    return false
  },
}
