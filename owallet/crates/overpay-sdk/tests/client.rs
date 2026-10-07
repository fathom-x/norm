//! The transport and the flows, against a fake marketplace.

use std::time::Duration;

use futures_util::StreamExt;
use overpay_sdk::flows::{
    discover_provider_tools, find_listing_id, ListingResolver, OrderWaiter, PayError, PkceFlow,
    Until, WaitEvent, WaitState,
};
use overpay_sdk::testing::{fixture, listing_with, order_with, FakeOverpay};
use overpay_sdk::{
    Auth, Client, CreateOrder, Error, ErrorCode, FulfillmentStatus, ListingQuery, Nip98Signer,
    OrderQuery, RetryPolicy, SecretKey,
};
use serde_json::{json, Value};
use wiremock::matchers::{body_json, header, header_exists, method, path, query_param};
use wiremock::{Mock, Request, ResponseTemplate};

fn retrying(fake: &FakeOverpay) -> Client {
    Client::builder(fake.uri())
        .retry(RetryPolicy::new(
            3,
            Duration::from_millis(1),
            Duration::from_millis(5),
        ))
        .build()
        .unwrap()
}

fn decode_nip98(header: &str) -> Value {
    use base64::Engine;
    let b64 = header.strip_prefix("Nostr ").unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap(),
    )
    .unwrap()
}

// ---- transport ----

#[tokio::test]
async fn bearer_and_accept_headers() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .and(header("authorization", "Bearer tok"))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("account_show")))
        .expect(1)
        .mount(fake.server())
        .await;
    let account = fake
        .client()
        .account()
        .get(Auth::Bearer("tok"))
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(account.formatted_account_number.len(), 19);
}

#[tokio::test]
async fn nip98_signs_the_final_url_including_the_query() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orders"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": [], "next_cursor": null})),
        )
        .mount(fake.server())
        .await;
    let key = SecretKey::from_bytes([7; 32]).unwrap();
    let mut query = OrderQuery::default();
    query.payer_address = Some("0xabc".into());
    query.limit = Some(5);
    fake.client()
        .orders()
        .list(&query, Auth::Nip98(&key))
        .await
        .unwrap();

    let req = &fake.server().received_requests().await.unwrap()[0];
    let event = decode_nip98(req.headers["authorization"].to_str().unwrap());
    // wiremock reports the URL with its host normalized; the signature
    // names the URL the client actually requested.
    assert_eq!(
        event["tags"][0][1],
        format!("{}/api/v1/orders?limit=5&payer_address=0xabc", fake.uri())
    );
    assert_eq!(event["tags"][1][1], "GET");
    assert_eq!(event["pubkey"], key.public_key_hex());
}

/// A tag's value from a decoded NIP-98 event.
fn tag(event: &Value, name: &str) -> Option<String> {
    event["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t[0] == name)
        .map(|t| t[1].as_str().unwrap().to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
async fn bearer_signed_post_signs_the_exact_body_it_sends() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("orders_create_unpaid")))
        .expect(1)
        .mount(fake.server())
        .await;
    let key = SecretKey::from_bytes([5; 32]).unwrap();
    fake.client()
        .orders()
        .create(
            &CreateOrder::new("l1").buyer_note("a note"),
            Auth::BearerSigned("tok", &key),
        )
        .await
        .unwrap();

    let req = &fake.server().received_requests().await.unwrap()[0];
    let event = decode_nip98(req.headers["x-nostr-signature"].to_str().unwrap());
    assert_eq!(
        tag(&event, "u").unwrap(),
        format!("{}/api/v1/orders", fake.uri())
    );
    assert_eq!(tag(&event, "method").unwrap(), "POST");
    // The hash of the bytes the server received: signed and sent are one.
    assert_eq!(tag(&event, "payload").unwrap(), sha256_hex(&req.body));
    assert_eq!(event["pubkey"], key.public_key_hex());
    assert_eq!(req.headers["content-type"], "application/json");
}

#[tokio::test]
async fn bearer_signed_get_is_signed_over_url_and_method_only() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("account_show")))
        .mount(fake.server())
        .await;
    let key = SecretKey::from_bytes([5; 32]).unwrap();
    fake.client()
        .account()
        .get(Auth::BearerSigned("tok", &key))
        .await
        .unwrap();

    let req = &fake.server().received_requests().await.unwrap()[0];
    assert_eq!(req.headers["authorization"], "Bearer tok");
    let event = decode_nip98(req.headers["x-nostr-signature"].to_str().unwrap());
    assert_eq!(tag(&event, "method").unwrap(), "GET");
    assert_eq!(tag(&event, "payload"), None);
}

#[tokio::test]
async fn nip98_post_carries_the_payload_tag_in_authorization() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/merchant_credits/alice/redeem"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("merchant_credits_redeem")))
        .mount(fake.server())
        .await;
    let key = SecretKey::from_bytes([6; 32]).unwrap();
    fake.client()
        .credits()
        .redeem("alice", "ord-1", Auth::Nip98(&key))
        .await
        .unwrap();

    let req = &fake.server().received_requests().await.unwrap()[0];
    assert!(req.headers.get("x-nostr-signature").is_none());
    let event = decode_nip98(req.headers["authorization"].to_str().unwrap());
    assert_eq!(tag(&event, "payload").unwrap(), sha256_hex(&req.body));
}

#[tokio::test]
async fn a_retried_create_is_signed_afresh_over_the_same_body() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(fake.server())
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("orders_create_unpaid")))
        .mount(fake.server())
        .await;
    let key = SecretKey::from_bytes([8; 32]).unwrap();
    retrying(&fake)
        .orders()
        .create(&CreateOrder::new("l1"), Auth::BearerSigned("tok", &key))
        .await
        .unwrap();

    let requests = fake.server().received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let events: Vec<Value> = requests
        .iter()
        .map(|r| decode_nip98(r.headers["x-nostr-signature"].to_str().unwrap()))
        .collect();
    // Each attempt is signed afresh over the identical body. (The NIP-98
    // event id hashes key, time, kind and tags only, so two attempts in
    // the same second share an id; only the signature differs.)
    assert_ne!(events[0]["sig"], events[1]["sig"]);
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(tag(&events[0], "payload"), tag(&events[1], "payload"));
    assert_eq!(
        tag(&events[1], "payload").unwrap(),
        sha256_hex(&requests[1].body)
    );
}

#[tokio::test]
async fn a_form_body_is_signed_as_sent() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(fake.server())
        .await;
    let key = SecretKey::from_bytes([4; 32]).unwrap();
    let request = overpay_sdk::Request::post("/oauth/token").form(vec![
        ("grant_type".into(), "authorization_code".into()),
        ("code".into(), "a b&c".into()),
    ]);
    fake.client()
        .execute(request, Auth::Nip98(&key))
        .await
        .unwrap();

    let req = &fake.server().received_requests().await.unwrap()[0];
    assert_eq!(req.body, b"grant_type=authorization_code&code=a+b%26c");
    assert_eq!(
        req.headers["content-type"],
        "application/x-www-form-urlencoded"
    );
    let event = decode_nip98(req.headers["authorization"].to_str().unwrap());
    assert_eq!(tag(&event, "payload").unwrap(), sha256_hex(&req.body));
}

#[tokio::test]
async fn an_order_carries_its_session_label_and_lists_by_it() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("orders_create_unpaid")))
        .mount(fake.server())
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orders"))
        .and(query_param("client_session_id", "conv-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [order_with(json!({"id": "o1", "client_session_id": "conv-1"}))],
            "next_cursor": null,
        })))
        .mount(fake.server())
        .await;
    let client = fake.client();

    // Labelled, then unlabelled: the key is only sent when set.
    for order in [
        CreateOrder::new("l1").client_session_id("conv-1"),
        CreateOrder::new("l1"),
    ] {
        client
            .orders()
            .create(&order, Auth::Bearer("t"))
            .await
            .unwrap();
    }
    let bodies: Vec<Value> = fake
        .server()
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(bodies[0]["client_session_id"], "conv-1");
    assert!(bodies[1].get("client_session_id").is_none());

    let mut query = OrderQuery::default();
    query.client_session_id = Some("conv-1".into());
    let page = client
        .orders()
        .list(&query, Auth::Bearer("t"))
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(page.data[0].client_session_id.as_deref(), Some("conv-1"));
}

#[tokio::test]
async fn the_pay_flows_keep_the_session_label() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("orders_create_paid")))
        .mount(fake.server())
        .await;
    let order = CreateOrder::new("l1")
        .buyer_note("hi")
        .client_session_id("conv-2");
    fake.client()
        .create_paid_order(&order, 50, Auth::Bearer("t"))
        .await
        .unwrap();
    let request = &fake.server().received_requests().await.unwrap()[0];
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["client_session_id"], "conv-2");
    assert_eq!(body["authorization_cents"], 50);
    assert_eq!(body["pay"], "merchant_credits");
}

#[tokio::test]
async fn api_errors_carry_code_message_and_details() {
    let fake = FakeOverpay::start().await;
    fake.mount(
        "POST",
        "/api/v1/orders",
        402,
        fixture("error_insufficient_credits"),
    )
    .await;
    let err = fake
        .client()
        .orders()
        .create(
            &CreateOrder::new("l").authorization_cents(50),
            Auth::Bearer("t"),
        )
        .await
        .unwrap_err();
    assert_eq!(err.status(), Some(402));
    assert_eq!(err.code(), Some(&ErrorCode::InsufficientCredits));
    let api = err.api().unwrap();
    assert_eq!(
        api.message.as_deref(),
        Some("Not enough merchant credits to cover the authorization")
    );
    assert!(api.detail_f64("authorization_cents").is_some());
    assert!(err.to_string().starts_with("HTTP 402: {"), "{err}");
}

#[tokio::test]
async fn a_non_json_error_body_still_reads() {
    let fake = FakeOverpay::start().await;
    Mock::given(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>bad gateway</html>"))
        .mount(fake.server())
        .await;
    let err = fake
        .client()
        .account()
        .get(Auth::Bearer("t"))
        .await
        .unwrap_err();
    // A web page isn't relayed as the message; the body stays whole.
    assert_eq!(
        err.to_string(),
        "HTTP 502: the server answered with a web page instead of a reply."
    );
    assert_eq!(err.api().unwrap().body, "<html>bad gateway</html>");
    assert_eq!(err.code(), None);
}

#[tokio::test]
async fn gets_retry_transient_failures_and_resign_each_attempt() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .mount(fake.server())
        .await;
    fake.mount("GET", "/api/v1/account", 200, fixture("account_show"))
        .await;
    let key = SecretKey::from_bytes([9; 32]).unwrap();
    retrying(&fake)
        .account()
        .get(Auth::Nip98(&key))
        .await
        .unwrap();

    let requests = fake.server().received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let sigs: Vec<Value> = requests
        .iter()
        .map(|r| decode_nip98(r.headers["authorization"].to_str().unwrap())["sig"].clone())
        .collect();
    assert_ne!(sigs[0], sigs[2], "each attempt is signed afresh");
}

#[tokio::test]
async fn retries_give_up_after_max_attempts() {
    let fake = FakeOverpay::start().await;
    Mock::given(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(503))
        .mount(fake.server())
        .await;
    let err = retrying(&fake)
        .account()
        .get(Auth::Bearer("t"))
        .await
        .unwrap_err();
    assert_eq!(err.status(), Some(503));
    assert_eq!(fake.server().received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn order_creation_retries_with_one_idempotency_key() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(fake.server())
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .and(header_exists("idempotency-key"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(fixture("orders_create_paid"))
                .insert_header("Idempotent-Replayed", "true"),
        )
        .mount(fake.server())
        .await;
    let created = retrying(&fake)
        .orders()
        .create(
            &CreateOrder::new("l").authorization_cents(50),
            Auth::Bearer("t"),
        )
        .await
        .unwrap();
    assert!(created.replayed());

    let keys: Vec<String> = fake
        .server()
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.headers["idempotency-key"].to_str().unwrap().to_string())
        .collect();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1], "a retry reuses its key");
}

#[tokio::test]
async fn posts_without_a_key_are_not_retried() {
    let fake = FakeOverpay::start().await;
    Mock::given(path("/api/v1/merchant_credits/s/redeem"))
        .respond_with(ResponseTemplate::new(503))
        .mount(fake.server())
        .await;
    let client = Client::builder(fake.uri())
        .retry(RetryPolicy::new(3, Duration::ZERO, Duration::ZERO))
        .build()
        .unwrap();
    assert!(client
        .credits()
        .redeem("s", "o", Auth::Bearer("t"))
        .await
        .is_err());
    assert_eq!(fake.server().received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn automatic_idempotency_keys_can_be_turned_off() {
    let fake = FakeOverpay::start().await;
    fake.mount(
        "POST",
        "/api/v1/orders",
        201,
        fixture("orders_create_unpaid"),
    )
    .await;
    let client = Client::builder(fake.uri())
        .idempotency_keys(false)
        .build()
        .unwrap();
    client
        .orders()
        .create(&CreateOrder::new("l"), Auth::Bearer("t"))
        .await
        .unwrap();
    let req = &fake.server().received_requests().await.unwrap()[0];
    assert!(!req.headers.contains_key("idempotency-key"));
    assert_eq!(
        req.body_json::<Value>().unwrap(),
        json!({"listing_id": "l"})
    );
}

#[tokio::test]
async fn create_order_sends_only_what_is_set() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .and(body_json(json!({
            "listing_id": "l", "buyer_note": "{\"model\":\"m\"}", "variant": "cheap",
            "pay": "merchant_credits", "spend_authorization_id": "sa"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("orders_create_paid")))
        .expect(1)
        .mount(fake.server())
        .await;
    let order = CreateOrder::new("l")
        .buyer_note_json(&json!({"model": "m"}))
        .variant("cheap")
        .spend_authorization("sa");
    fake.client()
        .orders()
        .create(&order, Auth::Bearer("t"))
        .await
        .unwrap();
}

// ---- waiting ----

#[tokio::test]
async fn the_waiter_streams_partial_output_then_finishes() {
    let fake = FakeOverpay::start().await;
    let snapshots = [
        order_with(
            json!({"id": "o1", "fulfillment_status": "processing", "partial_content": "Hel", "partial_seq": 1}),
        ),
        order_with(
            json!({"id": "o1", "fulfillment_status": "processing", "partial_content": "Hello", "partial_seq": 2}),
        ),
        order_with(
            json!({"id": "o1", "fulfillment_status": "delivered", "delivered_content": "Hello!"}),
        ),
    ];
    let calls = std::sync::atomic::AtomicUsize::new(0);
    Mock::given(path("/api/v1/orders/o1"))
        .respond_with(move |_: &Request| {
            let i = calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                .min(2);
            ResponseTemplate::new(200).set_body_json(json!({"data": snapshots[i]}))
        })
        .mount(fake.server())
        .await;

    let client = fake.client();
    let waiter =
        OrderWaiter::new(&client, "o1", Auth::Bearer("t")).poll_interval(Duration::from_millis(1));
    let events: Vec<WaitEvent> = waiter.events().map(Result::unwrap).collect().await;
    let deltas: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match e {
            WaitEvent {
                state: WaitState::Pending,
                delta,
                ..
            } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        vec![Some("Hel".to_string()), Some("lo".to_string())]
    );
    assert!(
        matches!(events.last(), Some(WaitEvent { state: WaitState::Done, snapshot, .. }) if snapshot["data"]["delivered_content"] == "Hello!")
    );
}

#[tokio::test]
async fn the_waiter_polls_conditionally_on_the_seq_it_has_seen() {
    // Poll 1: text arrives (seq 1). Poll 2: nothing new — the marketplace,
    // told `since_seq=1`, leaves the buffer out but still reports the seq.
    // Poll 3: delivered.
    let fake = FakeOverpay::start().await;
    let snapshots = [
        order_with(
            json!({"id": "o5", "fulfillment_status": "shipping", "delivered_content": null,
            "partial_content": "Hel", "partial_seq": 1}),
        ),
        order_with(
            json!({"id": "o5", "fulfillment_status": "shipping", "delivered_content": null,
            "partial_seq": 1}),
        ),
        order_with(
            json!({"id": "o5", "fulfillment_status": "delivered", "delivered_content": "Hello!"}),
        ),
    ];
    let calls = std::sync::atomic::AtomicUsize::new(0);
    Mock::given(path("/api/v1/orders/o5"))
        .respond_with(move |_: &Request| {
            let i = calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                .min(2);
            ResponseTemplate::new(200).set_body_json(json!({"data": snapshots[i]}))
        })
        .mount(fake.server())
        .await;

    let client = fake.client();
    let waiter =
        OrderWaiter::new(&client, "o5", Auth::Bearer("t")).poll_interval(Duration::from_millis(1));
    let events: Vec<WaitEvent> = waiter.events().map(Result::unwrap).collect().await;
    let deltas: Vec<Option<String>> = events.iter().map(|e| e.delta.clone()).collect();
    assert_eq!(deltas, vec![Some("Hel".to_string()), None, None]);
    assert_eq!(
        events[1].streamed, 3,
        "an omitted buffer doesn't reset the offset"
    );
    assert_eq!(
        events[1].snapshot["data"]["partial_content"], "Hel",
        "the omitted buffer is filled back in"
    );
    assert_eq!(events.last().unwrap().state, WaitState::Done);
    assert!(
        events[2].snapshot["data"].get("partial_content").is_none(),
        "a delivered order's buffer is gone"
    );

    let queries: Vec<Option<String>> = fake
        .server()
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.query().map(str::to_string))
        .collect();
    assert_eq!(
        queries,
        vec![None, Some("since_seq=1".into()), Some("since_seq=1".into())]
    );
}

#[tokio::test]
async fn the_waiter_inlines_offloaded_text() {
    let fake = FakeOverpay::start().await;
    let url = format!("{}/rails/blob/1", fake.uri());
    fake.mount_order(order_with(json!({
        "id": "o2", "delivered_content": null, "delivered_content_url": url,
        "delivered_content_byte_size": 5, "delivered_content_filename": "r.txt"
    })))
    .await;
    Mock::given(path("/rails/blob/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("hello")
                .insert_header("content-type", "text/plain"),
        )
        .mount(fake.server())
        .await;
    let client = fake.client();
    let done = OrderWaiter::new(&client, "o2", Auth::Bearer("t"))
        .wait()
        .await
        .unwrap();
    assert_eq!(done.snapshot["data"]["delivered_content"], "hello");
}

#[tokio::test]
async fn the_waiter_times_out_without_erroring() {
    let fake = FakeOverpay::start().await;
    fake.mount_order(order_with(
        json!({"id": "o3", "fulfillment_status": "processing", "delivered_content": null}),
    ))
    .await;
    let client = fake.client();
    let last = OrderWaiter::new(&client, "o3", Auth::Bearer("t"))
        .poll_interval(Duration::from_millis(5))
        .timeout(Duration::from_millis(20))
        .wait()
        .await
        .unwrap();
    assert_eq!(last.state, WaitState::TimedOut);
}

#[tokio::test]
async fn until_a_status_also_stops_on_failure() {
    let fake = FakeOverpay::start().await;
    fake.mount_order(order_with(
        json!({"id": "o4", "fulfillment_status": "failed"}),
    ))
    .await;
    let client = fake.client();
    let last = OrderWaiter::new(&client, "o4", Auth::Bearer("t"))
        .until(Until::Status("shipping".into()))
        .resolve_delivered(false)
        .wait()
        .await
        .unwrap();
    assert_eq!(last.state, WaitState::Done);
}

/// A poll that hangs past the deadline times the wait out with the
/// previous snapshot instead of holding it open.
#[tokio::test]
async fn a_hung_poll_times_the_wait_out() {
    let fake = FakeOverpay::start().await;
    let calls = std::sync::atomic::AtomicUsize::new(0);
    Mock::given(path("/api/v1/orders/o7"))
        .respond_with(move |_: &Request| {
            let first = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            let body = json!({"data": order_with(json!({"id": "o7", "fulfillment_status": "processing"}))});
            let response = ResponseTemplate::new(200).set_body_json(body);
            if first {
                response
            } else {
                response.set_delay(Duration::from_secs(2))
            }
        })
        .mount(fake.server())
        .await;
    let client = fake.client();
    let started = std::time::Instant::now();
    let last = OrderWaiter::new(&client, "o7", Auth::Bearer("t"))
        .resolve_delivered(false)
        .poll_interval(Duration::from_millis(10))
        .timeout(Duration::from_millis(50))
        .wait()
        .await
        .unwrap();
    assert_eq!(last.state, WaitState::TimedOut);
    assert_eq!(last.snapshot["data"]["id"], "o7");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
}

/// An order can skip the status waited for and go straight to delivered.
#[tokio::test]
async fn until_a_status_also_stops_on_delivery() {
    let fake = FakeOverpay::start().await;
    fake.mount_order(order_with(
        json!({"id": "o6", "fulfillment_status": "delivered", "delivered_content": "x"}),
    ))
    .await;
    let client = fake.client();
    let last = OrderWaiter::new(&client, "o6", Auth::Bearer("t"))
        .until(Until::Status(FulfillmentStatus::Shipping))
        .resolve_delivered(false)
        .timeout(Duration::from_millis(200))
        .poll_interval(Duration::from_millis(5))
        .wait()
        .await
        .unwrap();
    assert_eq!(last.state, WaitState::Done);
    assert!(last.elapsed < Duration::from_millis(200));
}

// ---- paying ----

#[tokio::test]
async fn create_paid_order_reports_what_was_redeemed() {
    let fake = FakeOverpay::start().await;
    fake.mount("POST", "/api/v1/orders", 201, fixture("orders_create_paid"))
        .await;
    let settled = fake
        .client()
        .create_paid_order(
            &CreateOrder::new("l").buyer_note("hi"),
            50,
            Auth::Bearer("t"),
        )
        .await
        .unwrap();
    assert_eq!(settled.status, "fully_paid");
    assert_eq!(
        settled.amount_redeemed_cents.as_ref().map(|c| c.as_f64()),
        Some(50.0)
    );
    assert_eq!(
        settled.order_id,
        fixture("orders_create_paid")["data"]["id"]
    );
}

#[tokio::test]
async fn create_paid_order_turns_402_into_insufficient_credits() {
    let fake = FakeOverpay::start().await;
    fake.mount(
        "POST",
        "/api/v1/orders",
        402,
        fixture("error_insufficient_credits"),
    )
    .await;
    match fake
        .client()
        .create_paid_order(&CreateOrder::new("l"), 50, Auth::Bearer("t"))
        .await
    {
        Err(PayError::InsufficientCredits {
            authorization_cents: 50,
            body,
        }) => assert!(body.contains("insufficient_credits")),
        other => panic!("{other:?}"),
    }
}

/// A 402 for some other reason isn't a shortfall of credits.
#[tokio::test]
async fn create_paid_order_tells_402s_apart_by_code() {
    let fake = FakeOverpay::start().await;
    fake.mount(
        "POST",
        "/api/v1/orders",
        402,
        json!({"error": "Order can't be paid", "code": "order_not_payable"}),
    )
    .await;
    match fake
        .client()
        .create_paid_order(&CreateOrder::new("l"), 50, Auth::Bearer("t"))
        .await
    {
        Err(PayError::Api(e)) => assert_eq!(e.code(), Some(&ErrorCode::OrderNotPayable)),
        other => panic!("{other:?}"),
    }
}

/// A created order without an id is an error, not an order called "".
#[tokio::test]
async fn an_order_without_an_id_is_an_error() {
    let fake = FakeOverpay::start().await;
    fake.mount(
        "POST",
        "/api/v1/orders",
        201,
        json!({"data": {"payment_status": "paid"}, "payment": {"status": "fully_paid"}}),
    )
    .await;
    let client = fake.client();
    let paid = client
        .create_paid_order(&CreateOrder::new("l"), 50, Auth::Bearer("t"))
        .await;
    assert!(
        matches!(paid, Err(PayError::Api(Error::Json(_)))),
        "{paid:?}"
    );
    let placed = client
        .place_and_pay(&CreateOrder::new("l"), "s", Auth::Bearer("t"))
        .await;
    assert!(
        matches!(placed, Err(PayError::Api(Error::Json(_)))),
        "{placed:?}"
    );
    // Nothing was redeemed against a missing order.
    assert!(fake
        .server()
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| !r.url.path().ends_with("/redeem")));
}

#[tokio::test]
async fn place_and_pay_fails_when_the_redemption_comes_up_short() {
    let fake = FakeOverpay::start().await;
    fake.mount(
        "POST",
        "/api/v1/orders",
        201,
        fixture("orders_create_unpaid"),
    )
    .await;
    fake.mount(
        "POST",
        "/api/v1/merchant_credits/s/redeem",
        200,
        json!({"data": {"status": "partial", "amount_redeemed_cents": 10, "message": "Credits ran out"}}),
    )
    .await;
    match fake
        .client()
        .place_and_pay(&CreateOrder::new("l"), "s", Auth::Bearer("t"))
        .await
    {
        Err(PayError::NotSettled {
            status, message, ..
        }) => {
            assert_eq!(status, "partial");
            assert_eq!(message.as_deref(), Some("Credits ran out"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn place_and_pay_happy_path() {
    let fake = FakeOverpay::start().await;
    fake.mount(
        "POST",
        "/api/v1/orders",
        201,
        fixture("orders_create_unpaid"),
    )
    .await;
    fake.mount(
        "POST",
        "/api/v1/merchant_credits/s/redeem",
        200,
        fixture("merchant_credits_redeem"),
    )
    .await;
    let settled = fake
        .client()
        .place_and_pay(
            &CreateOrder::new("l").buyer_note("note"),
            "s",
            Auth::Bearer("t"),
        )
        .await
        .unwrap();
    assert_eq!(settled.status, "fully_paid");
    let redeem = &fake.server().received_requests().await.unwrap()[1];
    assert_eq!(
        redeem.body_json::<Value>().unwrap()["order_id"],
        settled.order_id
    );
}

// ---- discovery and pagination ----

#[tokio::test]
async fn list_all_walks_every_page() {
    let fake = FakeOverpay::start().await;
    let first = listing_with(json!({"id": "a"}));
    let second = listing_with(json!({"id": "b"}));
    Mock::given(path("/api/v1/listings"))
        .and(query_param("cursor", "next"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": [second], "next_cursor": null})),
        )
        .mount(fake.server())
        .await;
    fake.mount(
        "GET",
        "/api/v1/listings",
        200,
        json!({"data": [first], "next_cursor": "next"}),
    )
    .await;
    let client = fake.client();
    let ids: Vec<String> = client
        .listings()
        .list_all(ListingQuery::default())
        .map(|l| l.unwrap().id)
        .collect()
        .await;
    assert_eq!(ids, vec!["a", "b"]);
}

#[tokio::test]
async fn provider_tools_are_discovered_with_their_schemas() {
    let fake = FakeOverpay::start().await;
    fake.mount_listings(vec![
        listing_with(json!({"id": "tool", "provider_tool": {"name": "forecast"}})),
        listing_with(json!({"id": "plain", "provider_tool": null})),
    ])
    .await;
    let tools = discover_provider_tools(&fake.client()).await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "forecast");
    assert_eq!(tools[0].seller_slug.as_deref(), Some("alice-shop"));
    assert!(tools[0].listing().unwrap().buyer_note_schema().is_some());
}

/// A server that keeps handing out cursors it already gave: the walk ends
/// instead of looping forever, having yielded each page once.
#[tokio::test]
async fn a_repeated_cursor_ends_the_walk() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/listings"))
        .respond_with(|req: &Request| {
            // Page 1 points at A; A points at B; B points back at A.
            let (id, next) = match req.url.query_pairs().find(|(k, _)| k == "cursor") {
                None => ("p1", "A"),
                Some((_, c)) if c == "A" => ("pA", "B"),
                Some(_) => ("pB", "A"),
            };
            ResponseTemplate::new(200).set_body_json(json!({
                "data": [listing_with(json!({"id": id, "title": id, "provider_tool": {"name": id}}))],
                "next_cursor": next,
            }))
        })
        .mount(fake.server())
        .await;
    for id in ["p1", "pA", "pB"] {
        fake.mount(
            "GET",
            &format!("/api/v1/listings/{id}"),
            200,
            json!({"data": listing_with(json!({"id": id}))}),
        )
        .await;
    }
    let client = fake.client();
    let ids: Vec<String> = client
        .listings()
        .list_all(ListingQuery::default())
        .map(|l| l.unwrap().id)
        .collect()
        .await;
    assert_eq!(ids, vec!["p1", "pA", "pB"]);
    let tools = discover_provider_tools(&client).await.unwrap();
    assert_eq!(tools.len(), 3);
    assert_eq!(
        find_listing_id(&client, "s", "missing").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn find_listing_id_by_seller_and_title() {
    let fake = FakeOverpay::start().await;
    fake.mount_listings(vec![
        listing_with(json!({"id": "x", "title": "Run Python Code"})),
        listing_with(json!({"id": "y", "title": "Run JavaScript Code"})),
    ])
    .await;
    let client = fake.client();
    assert_eq!(
        find_listing_id(&client, "exec", "Run JavaScript Code")
            .await
            .unwrap()
            .as_deref(),
        Some("y")
    );
    assert_eq!(find_listing_id(&client, "exec", "Run").await.unwrap(), None);
    let query = &fake.server().received_requests().await.unwrap()[0];
    assert!(query.url.query().unwrap().contains("seller=exec"));
}

#[tokio::test]
async fn the_resolver_finds_by_seller_and_title_and_caches_hits() {
    let fake = FakeOverpay::start().await;
    fake.mount_listings(vec![listing_with(
        json!({"id": "x", "title": "Run Python Code"}),
    )])
    .await;
    let client = fake.client();
    let resolver = ListingResolver::default();
    for _ in 0..2 {
        assert_eq!(
            resolver
                .resolve(&client, "exec", "Run Python Code")
                .await
                .unwrap()
                .as_deref(),
            Some("x")
        );
    }
    assert_eq!(
        resolver.resolve(&client, "exec", "Nope").await.unwrap(),
        None
    );
    let index_hits = fake
        .server()
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/v1/listings")
        .count();
    assert_eq!(index_hits, 2, "one lookup per miss, none for a cached hit");
}

// ---- auth flows ----

#[tokio::test]
async fn the_pkce_flow_registers_checks_state_and_exchanges() {
    let fake = FakeOverpay::start().await;
    fake.mount("POST", "/oauth/clients", 201, fixture("oauth_client"))
        .await;
    fake.mount("POST", "/oauth/token", 200, fixture("oauth_token"))
        .await;
    let client = fake.client();
    let flow = PkceFlow::start(&client, "owallet", "http://127.0.0.1:9/cb", "mcp")
        .await
        .unwrap();
    assert!(flow
        .authorize_url
        .as_str()
        .contains("code_challenge_method=S256"));
    assert!(flow.finish(&client, "code", "wrong-state").await.is_err());
    let token = flow
        .finish(&client, "code", &flow.pkce.state.clone())
        .await
        .unwrap();
    assert_eq!(token.token_type.as_deref(), Some("bearer"));
    let exchange = fake
        .server()
        .received_requests()
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(String::from_utf8_lossy(&exchange.body)
        .contains(&format!("code_verifier={}", flow.pkce.verifier)));
}

#[tokio::test]
async fn register_buyer_signs_with_the_wallet_key() {
    let fake = FakeOverpay::start().await;
    fake.mount(
        "POST",
        "/api/v1/buyer/register",
        201,
        fixture("buyer_register"),
    )
    .await;
    let key = SecretKey::from_bytes([3; 32]).unwrap();
    let reg = fake
        .client()
        .buyer()
        .register(&Default::default(), Auth::Nip98(&key))
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(reg.token, "test-token-placeholder");
    let req = &fake.server().received_requests().await.unwrap()[0];
    assert_eq!(
        decode_nip98(req.headers["authorization"].to_str().unwrap())["tags"][1][1],
        "POST"
    );
    let _ = key.sign_nip98("http://x", "GET");
}

/// A signer that can't sign fails the call before anything is sent.
#[tokio::test]
async fn an_invalid_signing_key_fails_without_sending() {
    struct Broken;
    impl Nip98Signer for Broken {
        fn sign_nip98_payload(
            &self,
            _url: &str,
            _method: &str,
            _payload: Option<&[u8]>,
        ) -> Result<String, overpay_sdk::InvalidKey> {
            Err(overpay_sdk::InvalidKey)
        }
    }
    let fake = FakeOverpay::start().await;
    let err = fake
        .client()
        .account()
        .get(Auth::BearerSigned("tok", &Broken))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Sign(_)), "{err:?}");
    assert!(fake.server().received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn delivered_files_must_be_on_the_marketplace() {
    let fake = FakeOverpay::start().await;
    let err = fake
        .client()
        .fetch_delivered_content("https://evil.example/f")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Delivery(_)));
}

#[tokio::test]
async fn a_keyed_create_waits_out_a_request_still_in_progress() {
    // The first attempt's response was lost; the retry arrives while the
    // server is still running it and is told so. Asking again gets the
    // replayed order, not a 409.
    let fake = FakeOverpay::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": "A request with this Idempotency-Key is still being processed",
            "code": "idempotency_request_in_progress",
        })))
        .up_to_n_times(1)
        .mount(fake.server())
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("Idempotent-Replayed", "true")
                .set_body_json(fixture("orders_create_unpaid")),
        )
        .mount(fake.server())
        .await;
    let order = CreateOrder::new("l");
    let resp = retrying(&fake)
        .orders()
        .create(&order, Auth::Bearer("t"))
        .await
        .unwrap();
    assert!(resp.replayed());
    let requests = fake.server().received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].headers["idempotency-key"],
        requests[1].headers["idempotency-key"]
    );
}

#[tokio::test]
async fn a_409_that_is_not_in_progress_is_an_error_at_once() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": "Send authorization_cents or spend_authorization_id, not both",
            "code": "conflicting_authorization",
        })))
        .mount(fake.server())
        .await;
    let err = retrying(&fake)
        .orders()
        .create(&CreateOrder::new("l"), Auth::Bearer("t"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(&ErrorCode::ConflictingAuthorization));
    assert_eq!(fake.server().received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_create_is_not_retried_after_an_ambiguous_502_by_default() {
    let fake = FakeOverpay::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(502))
        .mount(fake.server())
        .await;
    let order = CreateOrder::new("l");
    assert!(retrying(&fake)
        .orders()
        .create(&order, Auth::Bearer("t"))
        .await
        .is_err());
    assert_eq!(fake.server().received_requests().await.unwrap().len(), 1);

    // Against a server known to honor Idempotency-Key, it is.
    let vouched = Client::builder(fake.uri())
        .retry(RetryPolicy::new(2, Duration::ZERO, Duration::ZERO))
        .retry_ambiguous_writes(true)
        .build()
        .unwrap();
    assert!(vouched
        .orders()
        .create(&order, Auth::Bearer("t"))
        .await
        .is_err());
    assert_eq!(fake.server().received_requests().await.unwrap().len(), 3);
}
