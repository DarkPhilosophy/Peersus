# Website on Cloudflare

The project has one name, `peersus.axxa.dev`. Its server (the relay with the staged site beside it) answers everything; with
the name's DNS record proxied by Cloudflare, a Worker (`worker.js`) stands in front of the website. Each page is asked of the
server and its answer passed on; when the server does not answer within 3 seconds or fails (5xx), Cloudflare's own copy of the
same stage answers instead. The relay's endpoints (`/relay*` with its WebSocket, `/ping*`, `/generate_204*`, `/healthz*`) have
zone routes with no Worker, made once through the API (wrangler cannot declare them), so they reach the server through the
proxy as they are; `worker.js` passes them on unchanged too should one reach it. Each page says which side answered in
`X-Peersus-Source` (`server` or `cloudflare`). The server and Cloudflare must be given the same stage, or the two copies differ.

What the proxy changes for the relay: its WebSocket passes through Cloudflare, but the QUIC address discovery it offers over
UDP does not reach it through this name; its automatic (ACME TLS-ALPN) certificate cannot renew behind the proxy, which the
zone's SSL mode Full tolerates (Cloudflare encrypts to the server without validating its certificate, and devices see
Cloudflare's); and the zone's Always Use HTTPS answers plain `http://…/generate_204` with a redirect instead of the relay's 204.

## Stage and deploy

```sh
python3 scripts/fetch-store-approvals.py --out target/site-approvals
python3 site/build.py --output target/site-stage --approvals target/site-approvals
npx wrangler deploy --config packaging/cloudflare/wrangler.jsonc --dry-run
npx wrangler deploy --config packaging/cloudflare/wrangler.jsonc
```

`site/build.py` never overwrites its output: remove or rename the previous `target/site-stage` first. The Worker sends
the relay's response headers (its Content-Security-Policy, `nosniff`, `no-referrer`) with every page, from either side.
Going back is `npx wrangler rollback --config packaging/cloudflare/wrangler.jsonc`, or turning the record's proxy off.

Deploying needs the maintainer's explicit go-ahead, like any other publication.
