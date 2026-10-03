#!/usr/bin/env node
// Mock Overpay — just enough of the Rails API for owallet's /v1, /mcp and
// /_mgmt flows, deterministic and dependency-free (Node 22 stdlib only).
//
//   PORT=4010 node server.mjs           # listens on 127.0.0.1:$PORT (default 4010)
//   MOCK_STREAM_POLLS=3                 # in-flight polls before a chat order delivers
//
// Every response (OPTIONS preflights included) carries permissive CORS
// headers, so a page on any origin can call it like the real Overpay with
// API_CORS_ORIGINS set. Test hooks: GET /__mock/state (orders, requests),
// POST /__mock/reset. Shapes follow the wiremock fixtures in
// owallet-mcp/src/openai_compat.rs.

import http from "node:http";
import { randomUUID } from "node:crypto";

const PORT = Number(process.env.PORT || process.env.MOCK_OVERPAY_PORT || 4010);
const HOST = process.env.HOST || "127.0.0.1";
const STREAM_POLLS = Number(process.env.MOCK_STREAM_POLLS || 3);

export const MODELS = ["mock/chat-small", "mock/chat-large"];
const OPENROUTER_ID = "L-OPENROUTER";
const PYTHON_ID = "L-PYTHON";
const REPLY = "Hello from the mock seller.";
const PARTIALS = ["Hello", "Hello from", "Hello from the mock"];

const openrouterIndexRow = {
  id: OPENROUTER_ID,
  title: "OpenRouter Inference",
  description: "Chat completions through OpenRouter.",
  seller: { slug: "openrouter-bot", name: "OpenRouter" },
};
const pythonIndexRow = {
  id: PYTHON_ID,
  title: "Run Python Code",
  description: "Run a Python 3.11 snippet in an isolated sandbox.",
  seller: { slug: "exec", name: "Exec" },
};

const openrouterListing = {
  ...openrouterIndexRow,
  price_usd: "$0.01",
  price_cents: 1,
  free: false,
  pricing_mode: "metered",
  min_authorization_cents: 1,
  max_authorization_cents: 500,
  variant_field: "model",
  buyer_note_schema: {
    type: "object",
    properties: { model: { type: "string", enum: MODELS } },
  },
  variants: [
    {
      key: "mock/chat-small",
      title: "Mock: Chat Small",
      active: true,
      metadata: { context_length: 400000 },
      min_authorization_cents: 1,
      rate_card: {
        input_cents_per_mtok: 25.0,
        output_cents_per_mtok: 400.0,
        cache_read_cents_per_mtok: 2.5,
        markup: 0.2,
        as_of: "2026-10-01T00:00:00Z",
      },
    },
    {
      key: "mock/chat-large",
      title: "Mock: Chat Large",
      active: true,
      metadata: { context_length: 200000 },
      rate_card: {
        input_cents_per_mtok: 100.0,
        output_cents_per_mtok: 500.0,
        markup: 0.2,
        as_of: "2026-10-01T00:00:00Z",
      },
    },
  ],
};
const pythonListing = {
  ...pythonIndexRow,
  price_usd: "$0.02",
  price_cents: 2,
  free: false,
  buyer_note_schema: {
    type: "object",
    required: ["code"],
    properties: {
      code: { type: "string" },
      stdin: { type: "string" },
    },
  },
};
const LISTINGS = { [OPENROUTER_ID]: openrouterListing, [PYTHON_ID]: pythonListing };

let state;
function reset() {
  state = { orders: new Map(), seq: 0, requests: [], creditCents: 10_000 };
}
reset();

const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET, POST, PUT, PATCH, DELETE, OPTIONS",
  "access-control-allow-headers": "Authorization, Content-Type, Accept",
  "access-control-max-age": "600",
};

function send(res, status, body, extra = {}) {
  const json = body === undefined ? "" : JSON.stringify(body);
  res.writeHead(status, {
    ...CORS,
    ...(json ? { "content-type": "application/json; charset=utf-8" } : {}),
    ...extra,
  });
  res.end(json);
}

const authed = (req) => /^(Bearer|Nostr) \S+/.test(req.headers.authorization || "");

function orderJson(o) {
  const listing = LISTINGS[o.listing_id];
  const base = {
    id: o.id,
    listing_id: o.listing_id,
    product_title: listing.title,
    seller_slug: listing.seller.slug,
    payment_status: o.paid ? "paid" : "pending",
    fulfillment_status: o.paid ? "awaiting_seller" : "pending",
    total_usd_cents: o.authorized_cents ?? listing.price_cents,
    authorized_cents: o.authorized_cents ?? listing.price_cents,
    created_at: o.created_at,
  };
  if (!o.paid) return base;
  if (o.listing_id === PYTHON_ID) {
    return {
      ...base,
      fulfillment_status: "delivered",
      delivered_at: o.created_at,
      delivered_content: JSON.stringify({
        stdout: "42\n", stderr: "", exit_code: 0, duration_ms: 12, timed_out: false,
      }),
    };
  }
  // OpenRouter: in flight with a growing partial for STREAM_POLLS polls,
  // then delivered.
  if (o.polls <= STREAM_POLLS) {
    const i = Math.min(o.polls, PARTIALS.length) - 1;
    if (i < 0) return base;
    return { ...base, partial_content: PARTIALS[i], partial_seq: i + 1 };
  }
  // A metered seller captures what the turn cost — 3¢ here — never more
  // than the buyer authorized.
  const charged = Math.min(3, o.authorized_cents ?? 3);
  let model = "mock/chat-small";
  try {
    const note = JSON.parse(o.buyer_note || "{}");
    if (typeof note.model === "string" && note.model !== "default") model = note.model;
  } catch {}
  return {
    ...base,
    fulfillment_status: "delivered",
    delivered_at: o.created_at,
    captured_cents: charged,
    delivered_content: JSON.stringify({
      description: REPLY,
      model,
      error: false,
      tool_calls: [],
      usage: { prompt_tokens: 12, completion_tokens: 6 },
      charged_cents: charged,
    }),
  };
}

function createOrder(body) {
  const listing = LISTINGS[body.listing_id];
  if (!listing) return null;
  const o = {
    id: `ord-${++state.seq}`,
    listing_id: body.listing_id,
    buyer_note: body.buyer_note ?? null,
    paid: false,
    polls: 0,
    created_at: new Date(Date.UTC(2026, 9, 1, 0, 0, state.seq)).toISOString(),
  };
  state.orders.set(o.id, o);
  return o;
}

function pay(o, cents) {
  if (o.paid) return { status: "already_paid", amount_redeemed_cents: 0 };
  o.paid = true;
  o.authorized_cents = cents;
  state.creditCents -= cents;
  return {
    status: "fully_paid",
    amount_redeemed_cents: cents,
    credit_balance_cents: state.creditCents,
  };
}

async function readBody(req) {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  const raw = Buffer.concat(chunks).toString("utf8");
  if (!raw) return {};
  if ((req.headers["content-type"] || "").includes("application/x-www-form-urlencoded")) {
    return Object.fromEntries(new URLSearchParams(raw));
  }
  try { return JSON.parse(raw); } catch { return { _raw: raw }; }
}

async function handle(req, res) {
  const url = new URL(req.url, `http://${req.headers.host || "localhost"}`);
  const p = url.pathname;
  if (req.method === "OPTIONS") return send(res, 204);
  const body = req.method === "GET" || req.method === "HEAD" ? {} : await readBody(req);
  state.requests.push({ method: req.method, path: p, query: url.search, body });

  // ---- test hooks ----
  if (p === "/__mock/state") {
    return send(res, 200, {
      orders: [...state.orders.values()],
      requests: state.requests,
      credit_cents: state.creditCents,
    });
  }
  if (p === "/__mock/reset" && req.method === "POST") { reset(); return send(res, 200, { ok: true }); }

  // ---- public listings ----
  if (p === "/api/v1/listings" && req.method === "GET") {
    const seller = url.searchParams.get("seller");
    const rows = [openrouterIndexRow, pythonIndexRow].filter(
      (l) => !seller || l.seller.slug === seller,
    );
    return send(res, 200, { data: rows, meta: { next_cursor: null } });
  }
  let m = p.match(/^\/api\/v1\/listings\/([^/]+)$/);
  if (m && req.method === "GET") {
    const l = LISTINGS[m[1]];
    return l ? send(res, 200, { data: l }) : send(res, 404, { error: "not found" });
  }

  // ---- OAuth (PKCE) ----
  if (p === "/oauth/clients" && req.method === "POST") {
    return send(res, 201, { client_id: `mock-client-${++state.seq}`, client_name: body.client_name });
  }
  if (p === "/oauth/authorize" && req.method === "GET") {
    const to = new URL(url.searchParams.get("redirect_uri"));
    to.searchParams.set("code", "mock-code");
    to.searchParams.set("state", url.searchParams.get("state") || "");
    return send(res, 302, undefined, { location: to.toString() });
  }
  if (p === "/oauth/token" && req.method === "POST") {
    if (body.code !== "mock-code" || !body.code_verifier) {
      return send(res, 400, { error: "invalid_grant" });
    }
    return send(res, 200, { access_token: "mock-oauth-token", token_type: "Bearer", scope: "wallet" });
  }

  // ---- buyer sign-up (NIP-98) ----
  if (p === "/api/v1/buyer/register" && req.method === "POST") {
    if (!/^Nostr \S+/.test(req.headers.authorization || "")) {
      return send(res, 401, { error: "Missing Nostr authorization" });
    }
    return send(res, 201, {
      data: {
        user_id: 1,
        account_number: "1234567890123456",
        nostr_pubkey: "mock",
        token: "mock-api-token",
        token_name: body.token_name || "nostr-auth",
      },
    });
  }

  // ---- everything below needs auth ----
  if (p.startsWith("/api/v1/") && !authed(req)) {
    return send(res, 401, { error: "unauthorized" });
  }

  if (p === "/api/v1/account" && req.method === "GET") {
    return send(res, 200, { data: { username: "mock-buyer", account_number: "1234567890123456" } });
  }
  if (p === "/api/v1/merchant_credits" && req.method === "GET") {
    return send(res, 200, {
      data: [
        {
          holder_type: "organization",
          organization_slug: "overpay",
          core: true,
          balance_cents: state.creditCents,
          currency: "USD",
          formatted_balance: `$${(state.creditCents / 100).toFixed(2)}`,
        },
      ],
    });
  }
  m = p.match(/^\/api\/v1\/merchant_credits\/([^/]+)\/redeem$/);
  if (m && req.method === "POST") {
    const o = state.orders.get(body.order_id);
    if (!o) return send(res, 404, { error: "order not found" });
    return send(res, 200, { data: pay(o, LISTINGS[o.listing_id].price_cents) });
  }
  if (p === "/api/v1/orders" && req.method === "POST") {
    const o = createOrder(body);
    if (!o) return send(res, 422, { error: "unknown listing" });
    if (body.pay === "merchant_credits") {
      const cents = Number(body.authorization_cents ?? LISTINGS[o.listing_id].price_cents);
      const payment = pay(o, cents);
      return send(res, 201, { data: orderJson(o), payment });
    }
    return send(res, 201, { data: orderJson(o) });
  }
  if (p === "/api/v1/orders" && req.method === "GET") {
    return send(res, 200, { data: [...state.orders.values()].map(orderJson), meta: { next_cursor: null } });
  }
  m = p.match(/^\/api\/v1\/orders\/([^/]+)$/);
  if (m && req.method === "GET") {
    const o = state.orders.get(m[1]);
    if (!o) return send(res, 404, { error: "not found" });
    if (o.paid) o.polls += 1;
    return send(res, 200, { data: orderJson(o) });
  }

  return send(res, 404, { error: `mock overpay: no route for ${req.method} ${p}` });
}

export function start(port = PORT, host = HOST) {
  const server = http.createServer((req, res) => {
    handle(req, res).catch((e) => send(res, 500, { error: String(e) }));
  });
  return new Promise((resolve) => server.listen(port, host, () => resolve(server)));
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const server = await start();
  const { port } = server.address();
  console.log(`mock overpay listening on http://${HOST}:${port}`);
}
