// `os` for the browser build: a fixed, believable Linux.
const HOME = "/home/norm"

export const EOL = "\n"
export const devNull = "/dev/null"
export const constants = { signals: {}, errno: {}, priority: {} }
export const homedir = () => globalThis.process?.env?.HOME ?? HOME
export const tmpdir = () => "/tmp"
export const hostname = () => "browser"
export const platform = () => "linux"
export const type = () => "Linux"
export const release = () => "6.0.0-browser"
export const version = () => "#1 SMP browser"
export const machine = () => "wasm32"
export const arch = () => "wasm32"
export const endianness = () => "LE"
export const uptime = () => performance.now() / 1000
export const loadavg = () => [0, 0, 0]
export const totalmem = () => 4 * 1024 ** 3
export const freemem = () => 2 * 1024 ** 3
export const availableParallelism = () => globalThis.navigator?.hardwareConcurrency ?? 1
export const cpus = () =>
  Array.from({ length: availableParallelism() }, () => ({
    model: "browser",
    speed: 0,
    times: { user: 0, nice: 0, sys: 0, idle: 0, irq: 0 },
  }))
export const networkInterfaces = () => ({})
export const userInfo = () => ({ uid: 1000, gid: 1000, username: "norm", homedir: homedir(), shell: null })

export default {
  EOL,
  devNull,
  constants,
  homedir,
  tmpdir,
  hostname,
  platform,
  type,
  release,
  version,
  machine,
  arch,
  endianness,
  uptime,
  loadavg,
  totalmem,
  freemem,
  availableParallelism,
  cpus,
  networkInterfaces,
  userInfo,
}
