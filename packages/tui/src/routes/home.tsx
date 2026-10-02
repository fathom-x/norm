import { Prompt, type PromptRef } from "../component/prompt"
import { createEffect, createSignal, Match, onMount, Show, Switch } from "solid-js"
import { RGBA } from "@opentui/core"
import { useSync } from "../context/sync"
import { Toast } from "../ui/toast"
import { useArgs } from "../context/args"
import { useRouteData } from "../context/route"
import { usePromptRef } from "../context/prompt"
import { useLocal } from "../context/local"
import { usePluginRuntime } from "../plugin/runtime"
import { useEditorContext } from "../context/editor"
import { useTerminalDimensions } from "@opentui/solid"
import { useKV } from "../context/kv.tsx"
import { useDialog } from "../ui/dialog"
import { useBindings } from "../keymap"
import { Sidebar } from "./session/sidebar"
import { HomeSessionDestinationProvider } from "./home/session-destination"

let once = false
const placeholder = {
  normal: ["Fix a TODO in the codebase", "What is the tech stack of this project?", "Fix broken tests"],
  shell: ["ls -la", "git status", "pwd"],
}

export function Home() {
  const pluginRuntime = usePluginRuntime()
  const sync = useSync()
  const route = useRouteData("home")
  const promptRef = usePromptRef()
  const [ref, setRef] = createSignal<PromptRef | undefined>()
  const args = useArgs()
  const local = useLocal()
  const editor = useEditorContext()
  const dimensions = useTerminalDimensions()
  const [sidebar, setSidebar] = useKV().signal<"auto" | "hide">("sidebar", "auto")
  const dialog = useDialog()

  // norm: → on an empty prompt opens/closes the sidebar, as in a session.
  useBindings(() => ({
    enabled: () => dialog.stack.length === 0 && !ref()?.current.input,
    bindings: [
      {
        key: "right",
        desc: "Toggle sidebar",
        group: "Session",
        cmd: () => setSidebar((value) => (value === "hide" ? "auto" : "hide")),
      },
    ],
  }))
  let sent = false

  onMount(() => {
    editor.clearSelection()
  })

  const bind = (r: PromptRef | undefined) => {
    setRef(r)
    promptRef.set(r)
    if (once || !r) return
    if (route.prompt) {
      r.set(route.prompt)
      once = true
      return
    }
    if (!args.prompt) return
    r.set({ input: args.prompt, parts: [] })
    once = true
  }

  // Wait for sync and model store to be ready before auto-submitting --prompt
  createEffect(() => {
    const r = ref()
    if (sent) return
    if (!r) return
    if (!sync.ready || !local.model.ready) return
    if (!args.prompt) return
    if (r.current.input !== args.prompt) return
    sent = true
    r.submit()
  })

  // norm: the empty state is an empty session (blank transcript above the
  // input, the sidebar beside it) rather than upstream's centred logo, tips
  // and footer, so the screen doesn't change shape on the first message.
  // The layout mirrors routes/session/index.tsx.
  return (
    <HomeSessionDestinationProvider>
      <box flexDirection="row" flexGrow={1} minHeight={0}>
        <box flexGrow={1} minHeight={0} gap={1}>
          <box flexGrow={1} minHeight={0} />
          <box flexShrink={0}>
            <pluginRuntime.Slot name="home_prompt" mode="replace" ref={bind}>
              <Prompt ref={bind} right={<pluginRuntime.Slot name="home_prompt_right" />} placeholders={placeholder} />
            </pluginRuntime.Slot>
          </box>
          <Toast />
        </box>
        <Show when={sidebar() !== "hide"}>
          <Switch>
            <Match when={dimensions().width > 120}>
              <Sidebar sessionID="" />
            </Match>
            <Match when={true}>
              <box
                position="absolute"
                top={0}
                left={0}
                right={0}
                bottom={0}
                alignItems="flex-end"
                backgroundColor={RGBA.fromInts(0, 0, 0, 70)}
              >
                <Sidebar sessionID="" />
              </box>
            </Match>
          </Switch>
        </Show>
      </box>
    </HomeSessionDestinationProvider>
  )
}
