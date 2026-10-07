use std::time::Duration;

use futures_util::stream::{self, Stream, StreamExt};
use serde_json::Value;
use tokio::time::Instant;

use crate::auth::Auth;
use crate::error::Error;
use crate::generated::FulfillmentStatus;
use crate::Client;
use crate::GetOrder;

/// When an [`OrderWaiter`] stops.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Until {
    /// Delivered, failed, cancelled or rejected.
    Terminal,
    /// This fulfillment status — or any terminal one, which ends the wait
    /// too (an order can skip straight past the target to `delivered`).
    Status(FulfillmentStatus),
}

/// How an [`OrderWaiter`] decides it is out of time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deadline {
    /// Stop once the time has run out.
    Elapsed,
    /// Stop when the next poll would land past the timeout.
    BeforeNextPoll,
}

/// Where a wait stands after a poll. Deliberately exhaustive: a wait has
/// exactly these outcomes, and a caller should handle each one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitState {
    /// The order is still in flight; another poll follows.
    Pending,
    /// The wait's goal was reached (see [`Until`]) — which includes the
    /// order failing: read the snapshot's `fulfillment_status`.
    Done,
    /// Time ran out first. The order may still finish later.
    TimedOut,
}

/// One poll of a wait.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct WaitEvent {
    pub state: WaitState,
    /// The order as polled: the raw body (`{"data": {...}}`), with a
    /// streaming buffer the server left out as unchanged filled back in.
    pub snapshot: Value,
    /// Time since the wait started.
    pub elapsed: Duration,
    /// The streaming seller's output that is new since the previous poll.
    pub delta: Option<String>,
    pub partial_seq: Option<u64>,
    /// Bytes of the streamed output handed out so far (including `delta`).
    pub streamed: usize,
}

impl WaitEvent {
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.state == WaitState::Pending
    }
}

/// Polls an order until it finishes, as a stream of [`WaitEvent`]s, one
/// per poll: [`WaitState::Pending`] while it is in flight, then one `Done`
/// or `TimedOut`. Every event carries the output a streaming seller added
/// since the previous poll — the last one included, so a final flush is
/// never lost. Snapshots are raw response bodies (`{"data": {...}}`).
///
/// The poll interval runs start-to-start, so a slow request doesn't add
/// its own time on top of it. Each poll after the first is conditional on
/// the last `partial_seq` seen (`since_seq`): the marketplace leaves an
/// unchanged streaming buffer out of the response, so a long reply isn't
/// re-downloaded on every poll. The waiter keeps the last buffer it got and
/// puts it back, so every snapshot carries the output so far.
///
/// ```no_run
/// # async fn demo(client: overpay_sdk::Client) -> Result<(), overpay_sdk::Error> {
/// use futures_util::StreamExt;
/// use overpay_sdk::{flows::OrderWaiter, Auth};
///
/// let waiter = OrderWaiter::new(&client, "order-id", Auth::Bearer("token"));
/// let mut events = std::pin::pin!(waiter.events());
/// while let Some(event) = events.next().await {
///     if let Some(text) = event?.delta {
///         print!("{text}");
///     }
/// }
/// # Ok(()) }
/// ```
pub struct OrderWaiter<'a> {
    client: &'a Client,
    order_id: String,
    auth: Auth<'a>,
    poll: Duration,
    timeout: Duration,
    until: Until,
    resolve_delivered: bool,
    deadline: Deadline,
}

impl<'a> OrderWaiter<'a> {
    /// Wait for a terminal status, polling every second for up to two
    /// minutes, inlining an offloaded deliverable when it arrives.
    #[must_use]
    pub fn new(client: &'a Client, order_id: impl Into<String>, auth: Auth<'a>) -> Self {
        Self {
            client,
            order_id: order_id.into(),
            auth,
            poll: Duration::from_secs(1),
            timeout: Duration::from_secs(120),
            until: Until::Terminal,
            resolve_delivered: true,
            deadline: Deadline::Elapsed,
        }
    }

    #[must_use]
    pub fn poll_interval(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn until(mut self, until: Until) -> Self {
        self.until = until;
        self
    }

    /// Whether to inline an offloaded deliverable (see
    /// [`super::resolve_delivered`]). Default on.
    #[must_use]
    pub fn resolve_delivered(mut self, on: bool) -> Self {
        self.resolve_delivered = on;
        self
    }

    /// Time out as soon as another poll wouldn't fit in the timeout, rather
    /// than once the timeout has passed.
    #[must_use]
    pub fn stop_before_overrun(mut self) -> Self {
        self.deadline = Deadline::BeforeNextPoll;
        self
    }

    /// The wait as a stream of events; it ends after `Done` or `TimedOut`,
    /// or after the first error.
    ///
    /// The wait ends within about one poll interval of its timeout: a poll
    /// still unanswered by then times the wait out with the previous
    /// snapshot. The first poll is never cut short — a wait always has a
    /// snapshot to report — so only the client's own per-request timeout and
    /// retries bound it.
    pub fn events(self) -> impl Stream<Item = Result<WaitEvent, Error>> + 'a {
        struct State<'a> {
            waiter: OrderWaiter<'a>,
            start: Instant,
            tracker: PartialTracker,
            /// When the previous poll was sent; none before the first.
            last_poll: Option<Instant>,
            /// The `partial_seq` the previous poll reported.
            seen_seq: Option<u64>,
            buffer: PartialBuffer,
            /// The previous poll's snapshot, for a poll that runs out of time.
            last_snapshot: Option<Value>,
            finished: bool,
        }
        let state = State {
            waiter: self,
            start: Instant::now(),
            tracker: PartialTracker::default(),
            last_poll: None,
            seen_seq: None,
            buffer: PartialBuffer::default(),
            last_snapshot: None,
            finished: false,
        };
        stream::unfold(state, |mut st| async move {
            if st.finished {
                return None;
            }
            if let Some(last) = st.last_poll {
                let sleep = st.waiter.next_sleep(last.elapsed(), st.start.elapsed());
                tokio::time::sleep(sleep).await;
            }
            st.last_poll = Some(Instant::now());
            let w = &st.waiter;
            let fetched = match &st.last_snapshot {
                None => Some(w.fetch(st.seen_seq).await),
                Some(_) => {
                    let budget = w.fetch_deadline().saturating_sub(st.start.elapsed());
                    tokio::time::timeout(budget, w.fetch(st.seen_seq))
                        .await
                        .ok()
                }
            };
            let mut snapshot = match fetched {
                Some(Ok(snapshot)) => snapshot,
                Some(Err(e)) => {
                    st.finished = true;
                    return Some((Err(e), st));
                }
                None => {
                    // Out of time with the poll unanswered: report where
                    // the order stood at the previous one.
                    st.finished = true;
                    let snapshot = st.last_snapshot.take().unwrap_or_default();
                    let event = WaitEvent {
                        state: WaitState::TimedOut,
                        partial_seq: partial_output(&snapshot).1,
                        snapshot,
                        elapsed: st.start.elapsed(),
                        delta: None,
                        streamed: st.tracker.sent(),
                    };
                    return Some((Ok(event), st));
                }
            };
            st.buffer.fill(&mut snapshot);
            let elapsed = st.start.elapsed();
            let state = if w.reached(fulfillment_status(&snapshot)) {
                WaitState::Done
            } else if w.out_of_time(elapsed) {
                WaitState::TimedOut
            } else {
                WaitState::Pending
            };
            let (partial, partial_seq) = partial_output(&snapshot);
            if partial_seq.is_some() {
                st.seen_seq = partial_seq;
            }
            let delta = st.tracker.advance(partial).map(str::to_string);
            st.finished = state != WaitState::Pending;
            if !st.finished {
                st.last_snapshot = Some(snapshot.clone());
            }
            let streamed = st.tracker.sent();
            Some((
                Ok(WaitEvent {
                    state,
                    snapshot,
                    elapsed,
                    delta,
                    partial_seq,
                    streamed,
                }),
                st,
            ))
        })
    }

    /// Wait to the end and return the final event (`Done` or `TimedOut`).
    pub async fn wait(self) -> Result<WaitEvent, Error> {
        let mut events = std::pin::pin!(self.events());
        let mut last = None;
        while let Some(event) = events.next().await {
            last = Some(event?);
        }
        Ok(last.expect("a wait ends with Done or TimedOut"))
    }

    async fn fetch(&self, since_seq: Option<u64>) -> Result<Value, Error> {
        let options = GetOrder {
            since_seq,
            ..GetOrder::default()
        };
        let mut snapshot = self
            .client
            .orders()
            .get_with(&self.order_id, &options, self.auth)
            .await?
            .into_raw();
        if self.resolve_delivered {
            super::resolve_delivered(self.client, &mut snapshot).await?;
        }
        Ok(snapshot)
    }

    /// Whether a poll answered `elapsed` into the wait ends it as timed out.
    fn out_of_time(&self, elapsed: Duration) -> bool {
        match self.deadline {
            Deadline::Elapsed => elapsed >= self.timeout,
            Deadline::BeforeNextPoll => elapsed.saturating_add(self.poll) >= self.timeout,
        }
    }

    /// How long to wait before the next poll: the rest of the interval
    /// since the previous one started — cut short, when the wait times out
    /// once its time has passed, so the last poll lands on the timeout.
    fn next_sleep(&self, since_last_poll: Duration, elapsed: Duration) -> Duration {
        let rest = self.poll.saturating_sub(since_last_poll);
        match self.deadline {
            Deadline::Elapsed => rest.min(self.timeout.saturating_sub(elapsed)),
            // Another poll was only scheduled because it fits.
            Deadline::BeforeNextPoll => rest,
        }
    }

    /// Time into the wait by which a poll (after the first) must have been
    /// answered; past it the wait times out.
    fn fetch_deadline(&self) -> Duration {
        match self.deadline {
            Deadline::Elapsed => self.timeout.saturating_add(self.poll),
            Deadline::BeforeNextPoll => self.timeout,
        }
    }

    fn reached(&self, status: Option<&str>) -> bool {
        let Some(status) = status.map(FulfillmentStatus::from) else {
            return false;
        };
        status.is_terminal() || matches!(&self.until, Until::Status(target) if *target == status)
    }
}

/// The last streaming buffer the server sent, for the polls where it left
/// the buffer out as unchanged (`since_seq` matched its `partial_seq`).
#[derive(Debug, Default)]
struct PartialBuffer(Option<(u64, String)>);

impl PartialBuffer {
    /// Remember a buffer `snapshot` carries, or fill in the one it omits.
    fn fill(&mut self, snapshot: &mut Value) {
        let data = match snapshot.get("data") {
            Some(_) => &mut snapshot["data"],
            None => snapshot,
        };
        let Some(seq) = data.get("partial_seq").and_then(Value::as_u64) else {
            // Delivered (or never streamed): the buffer is gone.
            self.0 = None;
            return;
        };
        match data.get("partial_content").and_then(Value::as_str) {
            Some(text) => self.0 = Some((seq, text.to_string())),
            None => {
                if let (Some((held, text)), Some(obj)) = (&self.0, data.as_object_mut()) {
                    if *held == seq {
                        obj.insert("partial_content".into(), Value::String(text.clone()));
                    }
                }
            }
        }
    }
}

/// `fulfillment_status` of a snapshot (enveloped or bare).
#[must_use]
pub fn fulfillment_status(snapshot: &Value) -> Option<&str> {
    snapshot
        .get("data")
        .and_then(|d| d.get("fulfillment_status"))
        .or_else(|| snapshot.get("fulfillment_status"))
        .and_then(Value::as_str)
}

/// A streaming seller's output so far and its sequence number, from a
/// snapshot (enveloped or bare). Both absent until the seller streams.
#[must_use]
pub fn partial_output(snapshot: &Value) -> (Option<&str>, Option<u64>) {
    let data = snapshot.get("data").unwrap_or(snapshot);
    (
        data.get("partial_content").and_then(Value::as_str),
        data.get("partial_seq").and_then(Value::as_u64),
    )
}

/// Turns successive `partial_content` buffers into the new text in each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PartialTracker {
    sent: usize,
}

impl PartialTracker {
    /// Bytes of the buffer already handed out.
    #[must_use]
    pub fn sent(&self) -> usize {
        self.sent
    }

    /// The part of `partial` not yet handed out, advancing past it.
    ///
    /// The buffer is an append-only prefix, so the new text is whatever sits
    /// past the offset last handed out. Two cases break that and both
    /// resynchronize rather than emitting garbage: the buffer shrinking (the
    /// marketplace clears it on delivery) and the offset landing
    /// mid-character (the buffer is capped on a character boundary, which
    /// can move it).
    pub fn advance<'p>(&mut self, partial: Option<&'p str>) -> Option<&'p str> {
        new_output_since(partial, &mut self.sent)
    }
}

/// [`PartialTracker::advance`] over a bare offset.
pub fn new_output_since<'a>(partial: Option<&'a str>, sent: &mut usize) -> Option<&'a str> {
    let partial = partial?;
    if partial.len() <= *sent || !partial.is_char_boundary(*sent) {
        *sent = partial.len();
        return None;
    }
    let delta = &partial[*sent..];
    *sent = partial.len();
    Some(delta)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tracker_emits_only_new_text_and_resyncs() {
        let mut t = PartialTracker::default();
        assert_eq!(t.advance(Some("Hel")), Some("Hel"));
        assert_eq!(t.advance(Some("Hello")), Some("lo"));
        assert_eq!(t.advance(Some("Hello")), None);
        assert_eq!(t.advance(Some("")), None);
        assert_eq!(t.sent(), 0);
        assert_eq!(t.advance(None), None);
    }

    #[test]
    fn tracker_resyncs_off_a_char_boundary() {
        let mut sent = 1;
        assert_eq!(new_output_since(Some("é and more"), &mut sent), None);
        assert_eq!(sent, "é and more".len());
    }

    #[test]
    fn status_reads_enveloped_and_bare_snapshots() {
        let enveloped =
            serde_json::json!({"data": {"fulfillment_status": "delivered", "partial_seq": 3}});
        assert_eq!(fulfillment_status(&enveloped), Some("delivered"));
        assert_eq!(partial_output(&enveloped), (None, Some(3)));
        assert_eq!(
            fulfillment_status(&serde_json::json!({"fulfillment_status": "failed"})),
            Some("failed")
        );
    }

    #[test]
    fn buffer_fills_in_an_omitted_partial_and_forgets_a_cleared_one() {
        let mut buffer = PartialBuffer::default();
        let mut first = json!({"data": {"partial_content": "Hel", "partial_seq": 1}});
        buffer.fill(&mut first);
        assert_eq!(first["data"]["partial_content"], "Hel");

        let mut omitted = json!({"data": {"partial_seq": 1}});
        buffer.fill(&mut omitted);
        assert_eq!(omitted["data"]["partial_content"], "Hel");

        // A different seq without content isn't ours to fill.
        let mut other = json!({"data": {"partial_seq": 2}});
        buffer.fill(&mut other);
        assert!(other["data"].get("partial_content").is_none());

        let mut delivered = json!({"data": {"fulfillment_status": "delivered"}});
        buffer.fill(&mut delivered);
        let mut late = json!({"partial_seq": 1});
        buffer.fill(&mut late);
        assert!(late.get("partial_content").is_none(), "cleared on delivery");
    }

    fn waiter(client: &Client, poll_ms: u64, timeout_ms: u64) -> OrderWaiter<'_> {
        OrderWaiter::new(client, "o", Auth::None)
            .poll_interval(Duration::from_millis(poll_ms))
            .timeout(Duration::from_millis(timeout_ms))
    }

    #[test]
    fn stop_before_overrun_counts_sub_second_time() {
        let client = Client::new("http://overpay.test").unwrap();
        let w = waiter(&client, 500, 1000).stop_before_overrun();
        assert!(!w.out_of_time(Duration::from_millis(400)));
        assert!(w.out_of_time(Duration::from_millis(600)));
        // A poll as long as the timeout: the first poll is the last.
        let w = waiter(&client, 1000, 1000).stop_before_overrun();
        assert!(w.out_of_time(Duration::from_millis(1)));
        assert_eq!(w.fetch_deadline(), Duration::from_millis(1000));
    }

    #[test]
    fn the_last_sleep_ends_at_the_timeout() {
        let client = Client::new("http://overpay.test").unwrap();
        let w = waiter(&client, 60_000, 10_000);
        assert_eq!(
            w.next_sleep(Duration::ZERO, Duration::from_secs(2)),
            Duration::from_secs(8)
        );
        assert_eq!(
            w.next_sleep(Duration::from_secs(1), Duration::from_secs(20)),
            Duration::ZERO
        );
        assert_eq!(w.fetch_deadline(), Duration::from_secs(70));
        let w = waiter(&client, 1000, 120_000);
        assert_eq!(
            w.next_sleep(Duration::from_millis(300), Duration::from_secs(5)),
            Duration::from_millis(700)
        );
    }
}
