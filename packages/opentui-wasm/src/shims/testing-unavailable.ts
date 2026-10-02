// @opentui/core/testing pulls in the Node build of the core; @opentui/solid
// only uses it from testRender(), which browsers never call.
export function createTestRenderer(): never {
  throw new Error("@opentui/core/testing is not available in the browser build")
}
export default {}
