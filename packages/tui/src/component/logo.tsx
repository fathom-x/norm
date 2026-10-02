import { TextAttributes } from "@opentui/core"
import { useTheme } from "../context/theme"

// norm: just the name (upstream draws the wordmark from ../logo).
export function Logo() {
  const { theme } = useTheme()
  return (
    <text fg={theme.text} attributes={TextAttributes.BOLD} selectable={false}>
      Norm
    </text>
  )
}
