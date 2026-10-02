import { render } from "@opentui/solid"
import { createSignal, onCleanup } from "solid-js"

function App() {
  const [ticks, setTicks] = createSignal(0)
  const timer = setInterval(() => setTicks((t) => t + 1), 250)
  onCleanup(() => clearInterval(timer))
  return (
    <box border borderStyle="single" title=" solid " width={44} height={6} padding={1} flexDirection="column">
      <text>hello from solid on wasm</text>
      <text>ticks: {ticks()}</text>
    </box>
  )
}

export async function start() {
  await render(() => <App />, { exitOnCtrlC: false, useMouse: false, targetFps: 30, openConsoleOnError: false })
  return {}
}
