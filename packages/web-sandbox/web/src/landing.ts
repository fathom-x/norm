// The landing page: hide the in-browser card when this server does not serve
// /browser/ (BROWSER_DIST unset). Without JavaScript both cards stay.
async function main() {
  try {
    const response = await fetch("/api/features", { cache: "no-store" })
    if (!response.ok) return
    const features = (await response.json()) as { browser?: boolean }
    if (features.browser) return
    document.getElementById("card-browser")?.setAttribute("hidden", "")
    document.getElementById("browser-note")?.removeAttribute("hidden")
  } catch {
    // Offline or an older server: leave the page as it is.
  }
}

void main()
