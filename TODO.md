# TODO

What Minerva still misses compared with Notion, Logseq and AFFiNE (reviewed November 2026), and follow-ups found while building the last round. Ordered by what matters most for people switching to Minerva and for its learning focus. Move an item to `CHANGELOG.md` when it ships.

## Next




## Follow-ups from the last round

- [ ] **Comments**: in desktop offline copies (they are not replicated yet).
- [ ] **Storage**: read through SQL, with tables per kind and foreign keys (bootstrap note lists are now paged by workspace), so requests stop growing with the server; PostgreSQL after that, for more than one app server. Measurements and the plan are in `docs/storage.md`.
- [ ] **Importers**: extend the direct Notion API importer to database tables and attached files (page text and child pages now import directly; unsupported content stays linked to Notion).

- [ ] **Recordings**: transcribing in the browser, so audio never leaves the device (it needs a speech model of 75 MB or more downloaded to the device).
- [ ] **Calendar**: CalDAV, so calendar apps can also add and change reminders. It is a protocol of its own (discovery, ETags, `REPORT` queries, each client's quirks) and needs its own project.

## MAYBE IN FUTURE. I will tell you when
- [ ] **Plugin API in the browser**: slash commands, block renderers and sidebar panels, sandboxed; a plugin list to browse.
- [ ] **Web clipper** browser extension, saving a page or selection to an inbox note.
- [ ] **AI**: semantic retrieval (embeddings stored next to the search index), per-person provider keys and usage limits, scheduled agents, AI actions on several selected blocks, streaming in the selection assistant.
- [ ] **Identity**: SSO (OIDC), passkeys, an audit log.
- [ ] **Markdown folder copy** in the desktop app: keep a plain Markdown copy of local workspaces in a folder, as Logseq's Markdown Mirror does (read-only first).
- [ ] **Translations, what is left**: a third language.
