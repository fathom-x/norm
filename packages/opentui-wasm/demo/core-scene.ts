import { BoxRenderable, InputRenderable, TextRenderable, createCliRenderer } from "@opentui/core"

export async function start() {
  const renderer = await createCliRenderer({
    exitOnCtrlC: false,
    useMouse: false,
    targetFps: 30,
    openConsoleOnError: false,
  })

  const box = new BoxRenderable(renderer, {
    id: "box",
    border: true,
    borderStyle: "rounded",
    title: " opentui · wasm ",
    width: "100%",
    height: 8,
    padding: 1,
    flexDirection: "column",
    gap: 1,
  })
  const text = new TextRenderable(renderer, { id: "text", content: "hello from wasm" })
  const input = new InputRenderable(renderer, {
    id: "input",
    width: 40,
    placeholder: "type here",
    backgroundColor: "#22222a",
    focusedBackgroundColor: "#2c2c38",
  })
  box.add(text)
  box.add(input)
  renderer.root.add(box)
  input.focus()
  renderer.start()

  // Frame-time probe for the test: change the text every frame for `ms`
  // and report the renderer's own per-frame timings (JS tree render +
  // native diff/ANSI generation + output).
  const measureFrames = async (ms = 1500) => {
    renderer.setGatherStats(true)
    renderer.resetStats()
    let i = 0
    const timer = setInterval(() => {
      text.content = `hello from wasm (${i++})`
    }, 16)
    await new Promise((resolve) => setTimeout(resolve, ms))
    clearInterval(timer)
    text.content = "hello from wasm"
    const stats = renderer.getStats()
    renderer.setGatherStats(false)
    return {
      frames: stats.frameTimes.length,
      averageFrameTime: stats.averageFrameTime,
      maxFrameTime: stats.maxFrameTime,
      nativeRenderTime: (stats as any).renderTime,
      nativeOutputTime: (stats as any).stdoutWriteTime,
    }
  }

  return { renderer, input, text, box, measureFrames, getInputValue: () => input.value }
}
