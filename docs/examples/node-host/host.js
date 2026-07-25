// A complete CleverHans §14 host in dependency-free Node — the reference
// third-party integration. Four endpoints, bearer-secret + version
// discipline, optional HMAC signature verification, and idempotent execute.
// Serves the `co-buyer` demo registry's semantics (one seeded transaction
// per session), so `cleverhans host-check` passes against it — CI proves
// that on every commit.
//
//   CLEVERHANS_SECRET=s3cret [CLEVERHANS_SIGNING_KEY=k] node host.js
//   cleverhans host-check --base-url http://127.0.0.1:3000 --secret s3cret \
//     [--signing-key k]
//
// Port with `PORT`. The shape to copy into your own stack: one middleware
// (auth), four handlers delegating to your domain functions, one
// idempotency map stored with the mutation.

import http from "node:http";
import crypto from "node:crypto";

const PORT = Number(process.env.PORT ?? 3000);
const SECRET = process.env.CLEVERHANS_SECRET;
const SIGNING_KEY = process.env.CLEVERHANS_SIGNING_KEY; // optional (§14.2)
if (!SECRET) {
  console.error("CLEVERHANS_SECRET is required");
  process.exit(1);
}

// --- "domain" state ---------------------------------------------------
// One seeded transaction per session (fresh sessions see fresh state), and
// the §12.14 idempotency map. In a real host these are your database; store
// the idempotency key in the same transaction as the mutation.
const sessions = new Map(); // session_id -> { coBuyer: {id, name} | null, country }
const executed = new Map(); // idempotency_key -> first outcome

function stateFor(sessionId) {
  if (!sessions.has(sessionId)) {
    sessions.set(sessionId, {
      coBuyer: { id: "cb_112", name: "Jane Doe" },
      country: "DK",
    });
  }
  return sessions.get(sessionId);
}

// --- §14.2 signature verification (optional) --------------------------
// Verify against the RAW request bytes, before any JSON parsing.
function signatureValid(header, rawBody, skewSeconds = 300) {
  if (!header) return false;
  const parts = Object.fromEntries(
    header.split(",").map((part) => part.trim().split("=")),
  );
  if (!parts.t || !parts.v1) return false;
  if (Math.abs(Date.now() / 1000 - Number(parts.t)) > skewSeconds) return false;
  const expected = crypto
    .createHmac("sha256", SIGNING_KEY)
    .update(`${parts.t}.`)
    .update(rawBody)
    .digest();
  let got;
  try {
    got = Buffer.from(parts.v1, "hex");
  } catch {
    return false;
  }
  return got.length === expected.length && crypto.timingSafeEqual(got, expected);
}

// --- the four endpoints ------------------------------------------------
const handlers = {
  // Who is this stream? Read the forwarded auth headers, return any JSON
  // principal — it is echoed back verbatim on every later call.
  verify_session(body) {
    return { status: 200, body: { principal: { user_id: "u_demo", roles: ["editor"] } } };
  },

  // May this user do this? Called at propose AND confirm time.
  authorize(body) {
    return { status: 200, body: { decision: "allow" } };
  },

  // What WOULD this do? Side-effect-free, computed under the principal.
  dry_run(body) {
    const state = stateFor(body.session_id);
    switch (body.action_id) {
      case "transaction.coBuyer.remove": {
        if (!state.coBuyer)
          return { status: 200, body: { outcome: "rejected", reason: "no co-buyer" } };
        return {
          status: 200,
          body: {
            outcome: "preview",
            preview: {
              affected_count: 1,
              sample_ids: [state.coBuyer.id],
              summary: `Remove co-buyer ${state.coBuyer.name} from TX-581`,
            },
          },
        };
      }
      case "transaction.setCountry":
        return {
          status: 200,
          body: {
            outcome: "preview",
            preview: {
              affected_count: 1,
              summary: `Country ${state.country} → ${body.params.country}`,
            },
          },
        };
      default:
        return { status: 404, body: { error: `unknown action \`${body.action_id}\`` } };
    }
  },

  // Optional §14.9: host-authored dynamic slot content. Only wired for the
  // action that benefits; the service falls back to its declarative slot
  // tables for everything else.
  build_slots(body) {
    const state = stateFor(body.session_id);
    switch (body.action_id) {
      case "transaction.setCountry":
        return {
          status: 200,
          body: {
            slots: {
              title: "Set country",
              detail: `${state.country} → ${body.params.country}`,
            },
          },
        };
      default:
        // 404 on unconfigured actions: build_slots is per-action opt-in.
        return { status: 404, body: { error: `no build_slots for \`${body.action_id}\`` } };
    }
  },

  // Do it — idempotent on idempotency_key (§12.14, the one non-negotiable).
  execute(body) {
    const key = body.idempotency_key;
    if (executed.has(key)) return { status: 200, body: executed.get(key) };
    const state = stateFor(body.session_id);
    let outcome;
    switch (body.action_id) {
      case "transaction.coBuyer.remove":
        outcome = state.coBuyer
          ? ((state.coBuyer = null), { outcome: "executed", result: { removed: true } })
          : { outcome: "rejected", reason: "no co-buyer" };
        break;
      case "transaction.setCountry":
        state.country = body.params.country;
        outcome = { outcome: "executed", result: { updated: true } };
        break;
      default:
        return { status: 404, body: { error: `unknown action \`${body.action_id}\`` } };
    }
    executed.set(key, outcome); // same transaction as the mutation, in real life
    return { status: 200, body: outcome };
  },
};

// --- transport ---------------------------------------------------------
const server = http.createServer((req, res) => {
  const chunks = [];
  req.on("data", (chunk) => chunks.push(chunk));
  req.on("end", () => {
    const raw = Buffer.concat(chunks);
    const reply = (status, body) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(JSON.stringify(body));
    };

    const endpoint = /^\/cleverhans\/(\w+)$/.exec(req.url ?? "")?.[1];
    if (req.method !== "POST" || !handlers[endpoint]) {
      return reply(404, { error: "not found" });
    }
    // §14.2 discipline, on every endpoint: bearer secret first…
    if (req.headers.authorization !== `Bearer ${SECRET}`) {
      return reply(401, { error: "bad secret" });
    }
    // …known contract version…
    if (req.headers["x-cleverhans-webhook-version"] !== "1") {
      return reply(400, { error: "unsupported_webhook_version", supported: [1] });
    }
    // …and, when this host requires signatures, a valid one over the raw bytes.
    if (SIGNING_KEY && !signatureValid(req.headers["x-cleverhans-signature"], raw)) {
      return reply(401, { error: "bad signature" });
    }

    let body;
    try {
      body = JSON.parse(raw.toString("utf8"));
    } catch {
      return reply(400, { error: "body is not JSON" });
    }
    const { status, body: responseBody } = handlers[endpoint](body);
    reply(status, responseBody);
  });
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(
    `cleverhans example host on http://127.0.0.1:${PORT} ` +
      `(signatures ${SIGNING_KEY ? "required" : "off"})`,
  );
});
