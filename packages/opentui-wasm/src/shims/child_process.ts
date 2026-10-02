function unavailable(): never {
  throw new Error("child_process is not available in the browser")
}
export const spawn = unavailable
export const spawnSync = () => ({ status: 1, stdout: "", stderr: "", error: new Error("unavailable") })
export const exec = unavailable
export const execSync = unavailable
export const execFile = unavailable
export const execFileSync = unavailable
export default { spawn, spawnSync, exec, execSync, execFile, execFileSync }
