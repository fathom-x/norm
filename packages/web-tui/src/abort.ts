// Request aborts for in-process responses (owallet-web behind the fetch
// router): what a real fetch and a dropped connection would do.
/**
 * Settle with `promise`, or reject as soon as `signal` aborts — what a real
 * fetch does. (The Rust future of a non-streamed request still runs to its own
 * end; only a body can be cancelled, below.)
 */
export function abortable<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) return Promise.reject(signal.reason)
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(signal.reason)
    signal.addEventListener("abort", onAbort, { once: true })
    promise.then(
      (value) => {
        signal.removeEventListener("abort", onAbort)
        resolve(value)
      },
      (error) => {
        signal.removeEventListener("abort", onAbort)
        reject(error)
      },
    )
  })
}

/**
 * The response with its body tied to the request's signal: an abort (ESC, a
 * timeout, a consumer that stops reading and aborts) cancels owallet-web's
 * body stream, which drops the Rust stream behind it — natively, a client
 * disconnect drops the axum future the same way. Without this a streamed
 * chat completion keeps polling Overpay after norm gave up on it.
 */
export function followAbort(response: Response, signal: AbortSignal): Response {
  if (!response.body) return response
  return new Response(abortableBody(response.body, signal), {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  })
}

/** `body`, cancelled (source included) when `signal` aborts. */
export function abortableBody(body: ReadableStream<Uint8Array>, signal: AbortSignal): ReadableStream<Uint8Array> {
  const { readable, writable } = new TransformStream<Uint8Array, Uint8Array>()
  body.pipeTo(writable, { signal }).catch(() => {})
  return readable
}
