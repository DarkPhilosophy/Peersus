# Website

`site/` is a static, dependency-free website. Its build is separate from desktop binaries, Android packages, extension signing and publication. All local commands below stage or validate files; they do not deploy them.

## Stage a preview

For a completely local development sample, run `./website.sh` from the checkout. It stages a fresh directory under `target/site-preview/` and serves only on loopback, using an automatically selected port printed by the server. Ctrl-C stops it. No external hosting or build dependencies beyond Python are required. `./website.sh --empty-store` exercises the unpublished empty-store state instead.

The default helper uses `--test-catalog`. This marks every page as an unpublished development preview and places the existing public signed Echo fixtures under **`store-test/`**, never the production **`store/`** path. The website labels them as test fixtures and does not represent them as maintainer production approvals. For real isolated application install/remove testing, use `./store-demo.sh`; a web preview cannot enable the normal application's trust key.

The same test preview can be staged without starting a server:

```sh
python3 site/build.py --output /path/to/new-preview --test-catalog
```


```sh
python3 site/build.py --output /path/to/new-preview
```

Choose a nonexistent directory outside `site/`. The builder refuses to overwrite output. It validates the allowlisted files, internal links/fragments, basic HTML metadata, resource policy and consented supporter entries before copying anything. Serve the result through a local HTTP server for browser inspection; opening HTML through `file:` is not equivalent because JSON fetches depend on an HTTP origin.

Check wide and narrow layouts, keyboard navigation, theme changes, reduced-motion behavior, readable text and the empty/unavailable catalog states. Static validation cannot establish that the page looks correct.

## One site, no reloads

The pages are Home (`index.html`), Store (`store.html` and one `store/<id>.html` per approved extension), Download (`install.html`), Security & privacy (`privacy.html`) and Support (`support.html`). `relay.html` is the relay's own page, served by the relay alone (`noindex`).

Every page but the relay's has one header slot (`<header class="site-header"></header>`) and one footer slot (`<footer></footer>`); `build.py` fills both from `chrome()`, so the navigation, the GitHub link and the footer are written once. Those pages load the same stylesheet and scripts (`tour.js`, `store.js`, `downloads.js`, then `app.js`); the relay's page has its own stylesheet and no script.

`app.js` changes pages in place: a link to another page of the site fetches it and swaps its main part, header, footer, title and description, and pushes its address; Back and Forward do the same. Only a link that leaves the site (GitHub, Ko-fi, an archive, `peersus://`) loads another document. Each page script gives `window.peersusPages` one function, run on every page shown with an `AbortSignal` that ends its document listeners and timers when the next page comes; it does nothing where its elements are absent. Each page is still a whole document, for search engines, readers without JavaScript and links opened directly.

## Pictures of the application

The phone in the home page is `site/demo/phone.html`: an interactive model of the app (from `site/concept/`), on made-up devices and data with random readings, in its own frame so that its styles and script stay its own. `tour.js` drives it through `postMessage` on the same origin: it asks for a screen, the model moves its pointer to that tab and presses it, and says which screen it shows and when a person pressed something in it; the card of that screen marks it, its top edge filling while it stays, and the tour rests after a person's choice. The desktop window in Download is the application's own screen: `scripts/site-shots.py` runs the headless demo's `site_shots` test (the real window on the demo's made-up data, dark, at twice a phone's density) and encodes `site/shots/*.webp` with `cwebp`; the home page no longer uses the phone pictures it also writes.

The home page shows what only Peersus does instead of a generic download page: the transfer portal, consent before anything lands, and pairing by a number both people read, drawn as the two screens show it (the same three numbers, turned on one side; `core::pairing`). Other file-sharing projects are named only on the Related tools line of the marketing plan, never imitated in copy or layout.

The typeface is the application's own (Quicksand, SIL OFL 1.1): `build.py` stages `crates/peersus-ui/assets/fonts/Quicksand[wght].ttf` and its `OFL.txt` under `fonts/`, and the footer links the licence.

## Search engines and LLMs

Every staged page gets its canonical address, a description, Open Graph and card tags (`head_metadata`); the home page and every extension page carry JSON-LD. `sitemap.xml` lists every page that may be indexed (a page whose robots tag says `noindex` stays out) and every extension page; `robots.txt` names it; `llms.txt` describes the site, its pages, each approved extension and the source.

## Include the store's approvals and the prerelease

```sh
python3 scripts/fetch-store-approvals.py --out /path/to/approvals
python3 site/build.py --output /path/to/new-preview --approvals /path/to/approvals
```

`fetch-store-approvals.py` makes read-only, anonymous GitHub requests: for each extension source on the `extension` branch, the signed approval of the version it names (`<id>/approval.json`, `approval.sig`) and its store pictures (`<id>/media/`); the application's signed development prerelease manifest (`_app/update.json`, `update.sig`); and the repository's star count (`_repository.json`), so that the header shows it without the visitor's browser asking GitHub. `build.py` checks every document's shape, stages them under `store/` and `app/`, and builds `store/<id>.html`; without `_repository.json` the header links to GitHub without a count.

**The builder does not cryptographically verify any signature.** It has no verification key. The application independently verifies the signature, sizes and digests before installation or update. The website's lists are display-only and cannot approve an extension.

## Where the site is served

The site is served by the project's relay under its own name (`SITE_URL` in `build.py`), from a staged folder: the stock
`iroh-relay` with one patch that reads the folder named by `PEERSUS_SITE_DIR` at start. Building that relay, trying it
locally and updating the server are in [packaging/relay/README.md](../packaging/relay/README.md). The relay serves only
known kinds of file (the site's HTML, CSS, scripts, JSON, pictures, typeface, `.txt`, `.xml` and `.sig`) under its own
Content-Security-Policy. GitHub Pages is not used. Staging never deploys; copying a staged folder to the server and
restarting the relay is a separate, explicitly authorized step.

## Store activation checklist

1. Create and protect the signing seed through an approved maintainer procedure; never commit it.
2. Compile the matching public verification key into the application.
3. Review and approve the exact extension artifact digests through the protected workflow (`extension-approve.yml`); it signs one approval record per plugin version.
4. Publish the release `extension/<id>/v<version>` with its archives, `approval.json`, `approval.sig` and `review.json`, as a separate authorized action.
5. Gather the approvals (`fetch-store-approvals.py`) and stage the website with them (`build.py --approvals`).
6. Review the staged site, then explicitly authorize its publication.
7. Verify the deployed approval bytes and that the download links reach the release archives, then use a matching application build to refresh and install a test extension.

Without a configured trust key and published approvals, the production store remains unavailable. Do not populate the store with fabricated packages, a throwaway trust key or an unsigned fallback to hide that condition. See [the extension-store contract](extension-store.md) for the actual security and platform limits.
