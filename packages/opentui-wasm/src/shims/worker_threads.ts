export const isMainThread = true
export const parentPort = null
export const workerData = null
export const threadId = 0
export class Worker {
  constructor() {
    throw new Error("worker_threads is not available in the browser")
  }
}
export default { isMainThread, parentPort, workerData, threadId, Worker }
