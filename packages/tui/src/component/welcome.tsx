import { RGBA } from "@opentui/core"
import { InstallationVersion } from "@opencode-ai/core/installation/version"
import { useTerminalDimensions } from "@opentui/solid"
import { For, Show } from "solid-js"
import { useTheme } from "../context/theme"
import { mascot, mascotColor } from "../logo"

// norm: what an empty chat area shows — the mascot beside "Norm Code" and
// the version, then the short how-to below. Replaces upstream's centered
// wordmark on the home route. The how-to needs ~10 rows on top of the
// prompt, tips and footer, so short terminals get just the header: a
// shrinking box would draw the lines over each other instead of clipping.
export function Welcome() {
  const { theme, mode } = useTheme()
  const dimensions = useTerminalDimensions()
  const green = () => RGBA.fromHex(mascotColor[mode()])
  const version = InstallationVersion === "local" ? "dev" : `v${InstallationVersion.replace(/^v/, "")}`
  return (
    <box>
      <box flexDirection="row" gap={2}>
        <box>
          <For each={mascot}>{(line) => <text fg={green()}>{line}</text>}</For>
        </box>
        <box paddingTop={1}>
          <text fg={theme.text}>
            <b>Norm Code</b>
            <span style={{ fg: theme.textMuted }}> {version}</span>
          </text>
          <text fg={theme.textMuted}>Pay-as-you-go coding agent</text>
        </box>
      </box>
      <Show when={dimensions().height >= 30}>
        <box paddingTop={1} gap={1}>
          <text fg={theme.textMuted}>
            Every message is a crypto micropayment powered by Overpay — no subscription, no API key. Type what you want
            done and press enter.
          </text>
          <box>
            <text>
              <span style={{ fg: theme.text }}>/budget</span>
              <span style={{ fg: theme.textMuted }}> set a spending limit for this conversation</span>
            </text>
            <text>
              <span style={{ fg: theme.text }}>/loop</span>
              <span style={{ fg: theme.textMuted }}> run a task on repeat, paced by the model</span>
            </text>
            <text>
              <span style={{ fg: theme.text }}>/help</span>
              <span style={{ fg: theme.textMuted }}> every command and shortcut</span>
            </text>
          </box>
        </box>
      </Show>
    </box>
  )
}
