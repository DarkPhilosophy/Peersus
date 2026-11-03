# Website and Pages preparation

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

## Include an approved catalog

```sh
python3 site/build.py --output /path/to/new-preview --catalog /path/to/signed-catalog
```

The catalog directory must include `catalog.json`, its raw 64-byte `catalog.sig`, and every archive named in the catalog. The builder checks schema, entries, sizes and SHA-256 digests and stages only those files under `store/`.

**The builder does not cryptographically verify the signature.** It has no signing or verification key. It must consume output from the protected maintainer approval procedure, not an untrusted submission. The application independently verifies the signature and rollback sequence before installation. The website's extension list is display-only and cannot approve an extension.

## GitHub-side prerequisites

Publishing requires an explicit maintainer action and repository configuration. Prepare Pages with GitHub Actions as its source, review the deployment environment and select the intended branch/artifact. Preserve the repository subpath in relative resources. Confirm that the final `store/` location matches the application's compiled `STORE_BASE` before distributing a build.

The existing website-preview workflow only uploads a reviewable artifact; it does not enable repository settings or deploy. Do not interpret successful artifact upload as a live website. Signing credentials, environment protection and Pages settings must be configured separately, with explicit authorization.

A private repository does **not** imply a private Pages website. GitHub's private-site access control requires an organization using Enterprise Cloud; see [Pages visibility](https://docs.github.com/en/enterprise-cloud@latest/pages/getting-started-with-github-pages/changing-the-visibility-of-your-github-pages-site). Public Pages from a private repository also requires a supported plan. If setup is rejected because of the plan, keep the repository private and use the offline sample and workflow artifact instead. Do not change repository visibility or purchase a plan as an implicit workaround.

The manual preview workflow tests staging and includes the clearly marked `store-test/` fixtures in its artifact. It grants no Pages deployment permission and performs no publication. Downloading an artifact and serving it locally is not a deployed Pages site.


## Catalog activation checklist

1. Create and protect the signing seed through an approved maintainer procedure; never commit it.
2. Compile the matching public verification key into the application.
3. Review and approve the exact extension archive digest through the protected workflow.
4. Preserve the previous catalog and archives so future approval retains entries and increases the rollback sequence.
5. Stage the website with the produced signed catalog.
6. Review the artifact, then explicitly authorize publication.
7. Verify the deployed catalog and archive bytes, then use a matching application build to refresh and install a synthetic test extension.

Without a configured trust key and published catalog, the production store remains unavailable. Do not populate the store with fabricated packages, a throwaway trust key or an unsigned fallback to hide that condition. See [the extension-store contract](extension-store.md) for the actual security and platform limits.
