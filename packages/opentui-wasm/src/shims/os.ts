// node:os for the browser.
export const EOL = "\n"
export const platform = () => "linux"
export const arch = () => "wasm32"
export const type = () => "Linux"
export const release = () => "browser"
export const hostname = () => "browser"
export const homedir = () => "/home/web"
export const tmpdir = () => "/tmp"
export const endianness = () => "LE"
export const cpus = () => [] as unknown[]
export const totalmem = () => 0
export const freemem = () => 0
export const uptime = () => 0
export const networkInterfaces = () => ({})
export const userInfo = () => ({ username: "web", uid: 1000, gid: 1000, shell: null, homedir: "/home/web" })
export const constants = { signals: {} as Record<string, number>, errno: {} as Record<string, number> }
export default {
  EOL,
  platform,
  arch,
  type,
  release,
  hostname,
  homedir,
  tmpdir,
  endianness,
  cpus,
  totalmem,
  freemem,
  uptime,
  networkInterfaces,
  userInfo,
  constants,
}
