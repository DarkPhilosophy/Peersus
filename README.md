# Peersus languages

This branch holds the translations of the Peersus application, one gettext PO file per language and release. Nothing
here is code and nothing here is run: the application downloads a language's file only when a person chooses that
language (or their device is set to it), keeps it in its own data folder, and shows English for anything a file does
not translate.

```
languages/<release>/languages.json   the offer: each language, the name it gives itself, how many messages it translates, its size
languages/<release>/<tag>.po         one language (ro.po, pt-br.po, zh.po, …)
```

`<release>` is the application's version (`0.1.0`). A file of an earlier release is used until the new release's file
is here, so a translation keeps working while it is brought up to date.

## Translating

1. **Get the template.** In the application: Settings → Languages → *Save the template…* (on a desktop). It is also on the default branch
   at `crates/peersus-ui/languages/messages.pot`. To improve an existing language, download its file from
   `languages/<release>/` here instead.
2. **Translate it** in any PO editor (Poedit, Lokalize, Gtranslator, a text editor). Keep every `{placeholder}` exactly as
   it is in the English text: a translation whose placeholders differ is not used. Fill the header:
   ```
   "Language: ro\n"
   "X-Language-Name: Română\n"
   "X-Translation: person\n"
   "Plural-Forms: nplurals=3; plural=(n==1 ? 0 : (n==0 || (n%100>0 && n%100<20)) ? 1 : 2);\n"
   "Content-Type: text/plain; charset=UTF-8\n"
   ```
   `Language` is the tag the file is named by (lowercase, `-` between parts: `pt-br`); `X-Language-Name` is the name the
   language gives itself, as the list of languages shows it. `X-Translation` is `ai` while no fluent speaker has checked the
   whole file (the application then tags the language "AI translated") and `person` once one has: a review that checks every
   message may change `ai` to `person`, which removes the tag.
3. **Try it.** Settings → Languages → *Import a language file…*: the application shows it at once, before anyone reviews it. An
   imported file stays on that device only.
4. **Share it.** Settings → Languages → *Share a translation* opens the upload page of this release's directory. Upload your `<tag>.po`,
   choose *Create a new branch for this commit and start a pull request*, and fill in the pull request as below. You can
   also open the pull request against `languages` yourself.

A maintainer reviews the pull request before the language is offered to everyone. Do not edit `languages.json` by hand:
it is regenerated from the files (`cargo xtask languages-offer`), and the check on every pull request
(`cargo xtask check-languages`) fails when a file does not fit the template or the offer is not the offer of its files.

## Pull request

Title: `<Language name> (<tag>) for <release>`, for example `Română (ro) for 0.1.0`.

```markdown
## Language

- Tag and name: `ro`, Română
- Release: 0.1.0
- New language / update of an existing one:

## How it was translated

- By a fluent speaker / machine-translated and reviewed by a speaker / machine-translated, not reviewed
- Header `X-Translation:` — `ai` until a fluent speaker has checked the whole file, then `person`
- Tool used:
- Messages translated: N of M (shown by Settings → Languages after importing)

## Checked in the application

- [ ] Imported in Settings → Languages and looked through the screens (Send, Receive, Devices, Transfers, Settings)
- [ ] No text cut off or broken inside a word on a phone and on a desktop
- [ ] Every `{placeholder}` kept as in the English text

## Notes

Words that were hard to translate, terms chosen, anything a reviewer should know.
```

## Licence

Translations are contributed under the licence of the project (Apache-2.0). By opening a pull request you agree that your
translation is shared under it.
