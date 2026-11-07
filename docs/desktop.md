# Desktop client (Windows, macOS and Linux)

`crates/minerva-desktop` builds a native Minerva application for Windows, macOS and Linux. A native window hosts the system webview (WebView2 on Windows, WKWebView on macOS, WebKitGTK 4.1 on Linux).

The app runs its own Minerva server in-process, so it works without any server or network. It holds personal notes and **offline copies** of workspaces from other servers, which sync in the background and merge edits made offline. See [sync.md](sync.md). Each registered server's own web interface is also available online through a Rust gateway (`crates/minerva-client`). That interface always matches the server's API version, while sign-in tokens stay in the operating system's credential store.

## Build and run

Linux needs the WebKitGTK 4.1 and GTK 3 development packages (Fedora: `webkit2gtk4.1-devel gtk3-devel`; Debian/Ubuntu: `libwebkit2gtk-4.1-dev libgtk-3-dev`) and a running Secret Service (GNOME Keyring or KWallet).

```sh
cargo run -p minerva-desktop                    # Linux, debug build with developer tools
scripts/package-desktop.sh                      # dist/*-linux-x86_64.tar.gz and *-windows-x86_64.zip
scripts/package-desktop.sh linux                # or one platform only
```

The Windows package is cross-compiled from Linux with the `x86_64-pc-windows-gnu` target and MinGW-w64 (`rustup target add x86_64-pc-windows-gnu`, plus `mingw64-gcc` or `gcc-mingw-w64-x86-64`). The zip contains `Minerva.exe` and `WebView2Loader.dll`, which must stay in the same folder. Windows 10 and 11 include the required Microsoft Edge WebView2 Runtime. The executable embeds its icon and version information.

The macOS package is built on a Mac with the Xcode command-line tools: `rustup target add aarch64-apple-darwin x86_64-apple-darwin`, then `scripts/package-desktop.sh macos`. It produces `Minerva.app` as a universal binary for Apple Silicon and Intel, for macOS 11 or later, with its icon and menu bar. The app has an ad-hoc signature and is not notarized. The first time, people open it with right-click → **Open**, or remove the quarantine flag with `xattr -dr com.apple.quarantine /Applications/Minerva.app`. Release builds run on GitHub's macOS runners.

The Linux archive contains the binary, a `.desktop` entry, a 256 px icon, and `install.sh` (installs into `~/.local`, or `PREFIX`). Application and web icons are generated from `favicon.png` into `crates/minerva-desktop/assets/` and `web/icons/`.

The desktop crate is not one of the workspace's default members, so `cargo build` and `cargo run -p minerva-server` work without GUI libraries. `cargo test --workspace` includes it.

## GitHub builds and releases

[Desktop release builds](https://github.com/akaped/minerva/actions/workflows/desktop.yml) builds Linux x86_64, Windows x86_64, and universal macOS packages. It runs when desktop dependencies or sources change on `main`, and can also be started from **Actions → Desktop release builds → Run workflow** on a chosen branch. Download the `minerva-desktop-linux-windows` and `minerva-desktop-macos` artifacts from the completed run; both include SHA-256 checksums. Artifacts are retained for 30 days. macOS builds are ad-hoc signed and are not notarized.

The existing **Release** workflow reuses these desktop builds and attaches the packages to the GitHub release when a version tag is pushed. Prepare the version and changelog with `scripts/release.sh VERSION`, then push the branch and generated `vVERSION` tag. The tag must match `Cargo.toml`. A manual desktop build produces downloadable artifacts without creating a version tag or publishing a GitHub release.

## Using it

The app opens the server you had open last, or **This computer** (personal notes and offline copies) the first time, or when that server is signed out or does not answer within 3 seconds. Nothing needs to be configured. The launcher (<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>O</kbd>) lists servers. **Add a server** with its address: HTTPS is assumed, and `localhost`/`127.x` may use HTTP. The client calls `/api/v1/health`, requires API version 1, and records the server's persistent UUID. **Open** loads the server's normal web interface; sign in there as usual. Use **Minerva → Servers…** or <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>O</kbd> (<kbd>⌘</kbd>+<kbd>Shift</kbd>+<kbd>O</kbd> on macOS) to return to the launcher. The launcher also offers **Offline…** (choose workspaces to keep on this computer; sign in first), **Sign out** (revokes the session on the server, and pauses syncing), **Rename**, and **Remove**.

The workspace menu (top of the sidebar) lists every place your notes live: **This computer**, then each server you added with its workspaces. Choose a workspace to open it: one with an offline copy opens right here, any other opens that server's page. The button beside a server workspace keeps or removes its offline copy. **Add server** adds one without leaving the app; signing in still happens on the server's page. On a server's page, the menu offers **This computer** and **All servers…** to come back. The server list is given only to this computer's page, never to a server's page.

Offline copies appear in This computer's workspace menu. Their top-bar badge shows the sync state: synced, offline with changes waiting, or changes to review. Click it to see every copy and resolve conflicts.

The workspace you had open last opens again the next time, on whichever server it is. On a server's page this needs a server version that remembers workspaces (newer than 0.4.0). Attachments download to the Downloads folder. Existing files are never overwritten; a numbered name is chosen instead. Links to other sites open in the system browser.

| Data                                           | Linux                                     | Windows                                               | macOS                                                    |
| ---------------------------------------------- | ----------------------------------------- | ----------------------------------------------------- | -------------------------------------------------------- |
| Server registry (no secrets)                   | `~/.config/minerva/connections.json`      | `%APPDATA%\minerva\connections.json`                  | `~/Library/Application Support/minerva/connections.json` |
| Personal notes, offline copies, unsent changes | `~/.local/share/minerva/local.db`         | `%LOCALAPPDATA%\minerva\local.db`                     | `~/Library/Application Support/minerva/local.db`         |
| Session tokens                                 | Secret Service, service `minerva-desktop` | Credential Manager, `connection:<id>.minerva-desktop` | Keychain, service `minerva-desktop`                      |
| Webview storage (theme, layout)                | `~/.local/share/minerva/webview`          | `%LOCALAPPDATA%\minerva\webview`                      | `~/Library/Application Support/minerva/webview`          |
| Server open last                               | `~/.local/share/minerva/last-server`      | `%LOCALAPPDATA%\minerva\last-server`                  | `~/Library/Application Support/minerva/last-server`      |

Set `MINERVA_CREDENTIAL_STORE=memory` to keep sign-ins only until the application exits. This is for kiosks, automated tests, or systems without a credential service.

## Security model

- **One origin per server.** The launcher is served from `minerva://app.localhost`, this computer's server from `minerva://local.localhost`, and each connection from `minerva://s<id>.localhost`. On Windows these are `https://minerva.<host>`, because WebView2 only intercepts HTTP(S). One server's pages cannot read another server's responses, and the gateway also rejects any request whose `Origin` or `Referer` belongs to a different origin.
- **Tokens never reach the page.** The gateway forwards a connection's requests only to that server, adding `Authorization: Bearer` from the credential store. It removes `token` from sign-in responses and drops `Set-Cookie`. Webview cookies and authorization headers are never forwarded, and only a short allow-list of request headers is passed through. Logout or an HTTP 401 deletes the stored token.
- **Server identity.** Tokens are attached only after `/health` confirms the server UUID recorded when it was added. If the UUID changes (for example after a reinstall or a redirected address), requests are refused and the launcher asks whether to trust the new server; trusting it discards the old sign-in.
- **TLS policy.** HTTPS uses the operating system's trust store. Plain HTTP is accepted for loopback addresses, or for an address where the user explicitly allowed it. Redirects are not followed, so a token is never forwarded to a different origin.
- **Local server.** It runs inside the app process and listens on no port. Its session token is created at each start and never reaches the page. `local.db` is not encrypted by Minerva; protect it like any other personal files, and use disk encryption.
- **Launcher commands** (add, open, sign out, remove…) are accepted only from the launcher page. Server pages can only ask to return to the launcher. Navigation is limited to the launcher and registered servers, plus the frames of YouTube players.
- **YouTube videos.** YouTube refuses to play in a page without a web address of its own, and pages here use `minerva://`. The app therefore serves one small player page per video from `http://127.0.0.1:<random port>/youtube/<id>`; it serves nothing else. Servers older than this version block that frame in note tabs: update the server too.

The web interface itself is unchanged, except that the gateway adds `<meta name="minerva-server-origin">` so that MCP configuration shows the server's real URL rather than the local origin.

## Limitations

- Responses are buffered by the webview protocol handler. Streaming AI chat answers therefore appear when complete instead of word by word.
- WebKitGTK cannot download from custom schemes. On Linux, attachment downloads are fetched by the client and saved directly, without a progress indicator.
- Windows builds are cross-compiled and statically checked, but have not yet been exercised on a Windows machine. See `verification.md`.
- Share links (`/share#…`) and the publishing **Preview** need browser cookies, which the gateway does not keep. Links to published pages and absolute share URLs open in the system browser.
- Offline copies sync about every two seconds and do not relay presence. For live co-editing with presence, use **Open** on the server. Sharing, publishing, and administration are online-only. See [sync.md](sync.md#limits).
- No notifications, auto-update, or installer (MSI or Flatpak) yet.

## Private AI providers

Open the **personal workspace**, then **AI providers**. Profiles created here are private to this computer. Credentials are encrypted in the desktop database with its adjacent `.ai-key` file; back up both together. Provider profiles and credentials are not included in workspace replication. Opening that database through the network server does not enable its private profiles.

Choose an API provider and enter your own key, or choose **OpenAI-compatible API** for a self-hosted model. For example, an Ollama installation with its compatible API enabled can use `http://127.0.0.1:11434/v1`, an installed model ID, and **No authentication (self-hosted)**. Use **API key** when your model server requires one. Requests go directly from the desktop to the selected endpoint. A local model can keep inference on this computer; a cloud endpoint receives the selected context and attachments.

Private profiles work with the personal workspace and local offline copies, including chat, writing assistance, and non-research generation. Connected remote server pages continue to use the remote server's configured providers. To use a private model with server content, open its offline copy from the personal workspace. Local copies retain their existing replication limitations.

### ChatGPT subscription sign-in

Choose **ChatGPT**, give the profile a name, and select **Continue with ChatGPT**. Follow **Open ChatGPT sign-in in your browser**, approve access, and return to Minerva. Minerva loads the account's available models and initially selects the first listed model. Use **Edit → Load available models** to choose another model and save it.

This uses OpenAI's [native open-source token-sharing flow](https://developers.openai.com/siwc/token-sharing-open-source/sign-in), with PKCE and a temporary loopback callback on `127.0.0.1`. Eligibility, available models and subscription usage limits are controlled by OpenAI. Subscription access is separate from API billing; Minerva never falls back to a paid API key when it runs out. No Codex installation or copied browser credentials are required.

Tokens stay encrypted locally, refresh automatically, and are never returned to the webview. **Sign out of ChatGPT** clears local tokens and attempts remote revocation. If remote revocation cannot be confirmed, revoke Minerva's access in ChatGPT settings. The local account registration is retained so **Edit → Continue with ChatGPT** can reconnect the same account. Removing the profile forgets that registration.

ChatGPT uses the public Responses endpoint with `store: false` and streaming enabled, including background writing requests. Partial or failed responses are rejected. Its [preview API limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations) mean this profile currently has no configurable answer-token limit, native tools, researched flashcards, or audio transcription. Other provider profiles retain those features where supported.
