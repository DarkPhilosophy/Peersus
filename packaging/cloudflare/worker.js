// The project's one name, peersus.axxa.dev, with Cloudflare in front of it: each page is asked of the project's server
// first, which serves the staged site beside the relay, and Cloudflare's own copy of that stage answers when the server does
// not answer in time or fails. The relay's endpoints always go to the server as they are (its WebSocket included): there is
// no copy of the relay to fall back to.
const WAIT_MS = 3000;
const RELAY_PATHS = new Set(["/relay", "/ping", "/generate_204", "/healthz"]);

// A subrequest to the Worker's own name goes to the server behind it, not back through this Worker.
async function fromOrigin(request) {
  if (request.method !== "GET" && request.method !== "HEAD") {
    return null;
  }
  try {
    const response = await fetch(request, {
      redirect: "manual",
      signal: AbortSignal.timeout(WAIT_MS),
    });
    return response.status < 500 ? response : null;
  } catch {
    return null;
  }
}

// The headers the project relay sends with the site (packaging/relay/iroh-relay-*-peersus-site.patch), on every page whichever
// side gave it: with the Worker answering first, Cloudflare applies no `_headers` file of its own.
const HEADERS = {
  "Content-Security-Policy":
    "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; " +
    "connect-src 'self' https://raw.githubusercontent.com; frame-src 'self'; frame-ancestors 'self'; form-action 'none'; " +
    "base-uri 'self'; block-all-mixed-content",
  "X-Content-Type-Options": "nosniff",
  "Referrer-Policy": "no-referrer",
  "Cache-Control": "public, max-age=300",
};

function marked(response, source) {
  const copy = new Response(response.body, response);
  for (const [name, value] of Object.entries(HEADERS)) {
    copy.headers.set(name, value);
  }
  copy.headers.set("X-Peersus-Source", source);
  return copy;
}

export default {
  async fetch(request, env) {
    if (RELAY_PATHS.has(new URL(request.url).pathname)) {
      return fetch(request);
    }
    const origin = await fromOrigin(request);
    if (origin) {
      return marked(origin, "server");
    }
    return marked(await env.ASSETS.fetch(request), "cloudflare");
  },
};
