import { RGBA } from "@opentui/core"
import { InstallationVersion } from "@opencode-ai/core/installation/version"
import { useTerminalDimensions } from "@opentui/solid"
import { For } from "solid-js"
import { mascot, mascotColor } from "../logo"
import { registerOpencodeSpinner } from "./register-spinner"
import { SPINNER_FRAMES } from "./spinner"

registerOpencodeSpinner()

// norm: drawn the moment the renderer owns the terminal, before the theme,
// KV and plugin providers have resolved (the app tree stays empty until
// they do, which used to read as a black screen). Deliberately free of
// those contexts, so colors are fixed per terminal mode.
export function Splash(props: { mode: "dark" | "light" }) {
  const dimensions = useTerminalDimensions()
  const green = () => RGBA.fromHex(mascotColor[props.mode])
  const muted = () => RGBA.fromHex(props.mode === "dark" ? "#8a8a8a" : "#6a6a6a")
  const text = () => RGBA.fromHex(props.mode === "dark" ? "#e6e6e6" : "#1f1f1f")
  const version = InstallationVersion === "local" ? "dev" : `v${InstallationVersion.replace(/^v/, "")}`
  return (
    <box
      position="absolute"
      zIndex={10000}
      left={0}
      top={0}
      width={dimensions().width}
      height={dimensions().height}
      justifyContent="center"
      alignItems="center"
    >
      <box flexDirection="row" gap={2}>
        <box>
          <For each={mascot}>{(line) => <text fg={green()}>{line}</text>}</For>
        </box>
        <box paddingTop={1} gap={1}>
          <text fg={text()}>
            <b>Norm Code</b>
            <span style={{ fg: muted() }}> {version}</span>
          </text>
          <box flexDirection="row" gap={1}>
            <spinner frames={SPINNER_FRAMES} interval={80} color={muted()} />
            <text fg={muted()}>Starting…</text>
          </box>
        </box>
      </box>
    </box>
  )
}
