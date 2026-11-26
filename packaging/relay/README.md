# Project relay and website

The project's relay is the stock `iroh-relay` at the version the application uses (`Cargo.lock`), with one patch:
`iroh-relay-<version>-peersus-site.patch` serves the website under the relay's own name. Nothing else of the relay changes.

- The site's files are read once, at start, from the folder named by the environment variable `PEERSUS_SITE_DIR`. Without
  it, or with a folder that cannot be read, the relay serves only its own pages.
- `index.html` is also served as `/`. The relay's endpoints (`/relay`, `/ping`, `/generate_204`, `/healthz`) are not
  files and stay the relay's.
- With a site folder named, a page asked for over plain HTTP (the relay's port 80, which otherwise answers only the
  captive-portal check `/generate_204`) is sent to the same path over HTTPS with `301`; a `Host` header that is not just
  a name and a port gets `404` instead of being repeated.
- Symbolic links are not followed; only known kinds of file are served (HTML, CSS, JS, JSON, SVG, PNG, WebP, ICO, TXT,
  XML, `.sig`, TTF for the site's typeface); at most 512 files, 32 MiB in total, 8 folders deep.
- The Content-Security-Policy sent with every response allows the site's own scripts, styles, pictures and typeface. Beyond
  the site it allows one address to be read: `https://raw.githubusercontent.com`, where the project's CI publishes the
  architecture map's data. Each page's own policy (its `meta` tag) narrows that again: only `architecture.html` names that
  address, every other page stays at the site alone.

## Build

```sh
packaging/relay/build.sh
```

Fetches the published crate, applies the patch and builds it in a `rust:1-bookworm` container, so the program runs on the
server's older glibc. It only builds: nothing is copied or restarted. It prints the path of the program.

## Stage the site

```sh
python3 scripts/fetch-store-approvals.py --out target/site-approvals
python3 site/build.py --output target/site-stage --approvals target/site-approvals
```

`fetch-store-approvals.py` gathers, for each extension source on the `extension` branch, the signed approval of the
version that source names. `site/build.py` checks every approval's shape and stages it under `store/<id>/` with
`store/index.json`. The archives stay on the project's release of that version and the store page links there. Neither
script verifies a signature: the application does that with the maintainer's key.

`python3 site/build.py --output DIR --test-catalog` stages a development preview with the public test fixture instead,
under `store-test/`, and marks every page as a preview. The two options cannot be combined.

The same commands stage `/architecture.html`, its fixed renderer, the map snapshot and licences. Staging needs a Git checkout, but no Rust build or network access for the map. The architecture workflow publishes new `map.json` data independently; there is no site redeploy for data-only changes. Without the publication or its page-specific response-header allowance, the page still shows its bundled or cached copy.

## Try it locally

```sh
PEERSUS_SITE_DIR=$PWD/target/site-stage target/relay-build/iroh-relay-<version>/target/release/iroh-relay --dev
```

`--dev` serves plain HTTP on port 3340. Check `/`, `/store.html`, `/store/index.json`, `/architecture.html`, `/ping` (200) and `/relay` (400
without an upgrade). On the server, also `http://<name>/` (301 to HTTPS) and `http://<name>/generate_204` (204).

## Update the server

On the server the relay runs as the systemd unit `iroh-relay` with its configuration in `/etc/iroh-relay/`. Back up the
program and the unit before changing either, then:

1. copy the staged site to the site folder and the new program next to the old one;
2. give the unit `Environment=PEERSUS_SITE_DIR=<site folder>` (a drop-in), `systemctl daemon-reload`;
3. replace the program, restart the unit, and check the pages and endpoints above over HTTPS.

A new site alone needs only steps 1 and a restart. Going back is restoring the backed-up program and unit and restarting.
