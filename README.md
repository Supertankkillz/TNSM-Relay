# TNSM Relay (workspace)

The rendezvous relay that lets a TNSM **host** (behind NAT) and a remote
**RC** client reach each other from anywhere  -  both dial the relay outbound
and are matched by a short code. The relay is a **byte pipe**: the host/RC
TLS session runs end-to-end *through* it, so it never sees plaintext.

## Layout

```
core/       Shared rendezvous logic (one source of truth).
headless/   Always-on service binary  -  run this on your VPS.
gui/        Tauri control panel  -  run on a desktop machine.
```

Both `headless` and `gui` use `core`, so they behave identically.

## Which do I run where?

- **VPS (production):** the **headless** binary, kept alive by systemd/NSSM.
  This is the always-on relay. No desktop needed.
- **Your PC (config + monitoring + testing):** the **GUI**. Set the port and
  optional shared secret, Start/Stop, and watch the live waiting-hosts and
  active-sessions lists.

> A typical cheap VPS is headless (no desktop), so the GUI usually runs on
> your own machine while the headless binary runs the real service on the
> VPS. If your VPS *does* have a desktop, you can run the GUI there instead
> and skip the headless binary  -  it runs the relay in-process.

The relay's **public address** (what you type into the Host/RC apps) is your
VPS's IP/DNS + the bind port. The relay itself doesn't need to know its own
public address.

## Build

Needs Rust (https://rustup.rs).

### Headless service binary

```
cargo build --release -p tnsm-relay
# -> target/release/tnsm-relay   (single executable; copy to the VPS)
./target/release/tnsm-relay relay.toml
```

`relay.toml` (optional; defaults shown):

```toml
bind = "0.0.0.0:7800"
host_ttl_secs = 600
handshake_timeout_secs = 15
shared_secret = ""
```

Keep it alive with systemd (Linux) or NSSM (Windows)  -  see the earlier
single-crate README for the exact unit/service definitions.

### GUI control panel

Needs the Tauri prerequisites (WebView2 on Windows; webkit2gtk on Linux) and
the Tauri CLI:

```
cargo install tauri-cli --version "^2"
cd gui
cargo tauri dev      # run it
cargo tauri build    # produce an installer/exe
```

> **Icons:** the bundle config has icons removed so it builds out of the box.
> Before producing a polished installer, add icons with
> `cargo tauri icon path/to/icon.png` (generates the icon set), then add the
> `bundle.icon` list back to `tauri.conf.json`.

## Security

The relay is untrusted infrastructure. All authentication (cert pinning +
username/password login) happens end-to-end between host and RC. The relay
can drop/delay traffic but cannot read or forge it without breaking TLS. The
`shared_secret` only gates *who may use your relay*, not host/RC trust.

## Access control

The relay runs on your VPS. The real gate on "who can touch the relay" is
**SSH/console access to that VPS**  -  which only you have. That's genuine
access control (it's not on a user's machine, so it can't be bypassed).

To keep stray clients off the relay without a login UX, set `shared_secret`
in `relay.toml`. Only builds that send the matching secret in their handshake
are accepted. For a single operator that's all you need.

### `gen-hash` (future per-user accounts)

If you later add per-user relay accounts, hash passwords with:

```
tnsm-relay gen-hash             # prompts for a password
tnsm-relay gen-hash "mypass"    # or pass it inline
```

It prints an Argon2 hash on stdout (the plaintext is never stored). You would
paste that hash into an accounts file on the VPS. Not needed for the
single-operator + shared_secret setup, but the tool is here when you want it.

## GUI login (survives updates)

The GUI prompts for a username + password at launch. Credentials live in a
LOCAL file next to the exe  -  `relay-auth.json`  -  so updating the app never
overwrites them:

```json
{ "username": "Administrator", "password_hash": "$argon2id$..." }
```

Set it up:
1. `tnsm-relay gen-hash "yourpassword"`  -> prints an Argon2 hash
2. Put your username + that hash in `relay-auth.json` next to the GUI exe
   (a `relay-auth.example.json` is included as a template).

If `relay-auth.json` is missing, the GUI falls back to a built-in default
(**admin / admin**) and the login screen says so  -  so you can never be locked
out. Once the file exists, it always wins. The file is gitignored; the
password itself is never stored (only its hash). This is a launch gate, not
hard security  -  the relay's real protection is your VPS's SSH access.

## Update check (GUI)

The GUI shows a passive "Update available" badge when a newer version exists.
It does NOT auto-install (no signing keys needed)  -  it just notices and tells
you, then copies the download URL to your clipboard when clicked.

It checks a small public JSON manifest. Set its URL in
`gui/src-tauri/src/lib.rs`:

```rust
const UPDATE_MANIFEST_URL: &str = "https://example.com/tnsm-relay/latest.json";
```

The manifest is tiny and must be publicly readable (no token). Your source
repo can stay **private**  -  only this JSON needs to be public. Host it as a
public Gist raw URL, a GitHub Pages file, or an asset in a public
"releases-only" repo:

```json
{
  "version": "0.2.0",
  "url": "https://your-download-page-or-release",
  "notes": "What changed in this version."
}
```

The GUI compares `version` to its own build version and shows the badge if
the manifest's is higher. The `url` is where you point users to download the
new build (a separate distribution decision  -  can be a private release page,
a paid gate, etc.).

## Running the GUI as the always-on relay (Windows Server)

The GUI app IS the relay - it runs the relay in-process. For a server you run
one app and it does everything (control panel + the live relay).

1. Build it: `cd gui && cargo tauri build` -> installer in
   `target/release/bundle/`. Install it on the server.
2. First launch: log in, set the bind port (7800) + optional shared secret,
   Save config. Leave "Start relay automatically when this app launches" ON.
3. Open the firewall (PowerShell as admin):
   `New-NetFirewallRule -DisplayName "TNSM Relay" -Direction Inbound -Protocol TCP -LocalPort 7800 -Action Allow`
4. From then on, launching the app auto-starts the relay - no clicking.

### Start automatically on boot

A GUI app needs a desktop session, so use Task Scheduler (not a service):
- Task Scheduler -> Create Task
- General: "Run only when user is logged on" (or "logged on" + auto-login the
  server's user so it survives reboots unattended)
- Triggers: "At log on"
- Actions: Start a program -> the installed "TNSM Relay.exe"
- Save. On boot/login the app launches and auto-starts the relay.

The relay then runs on 0.0.0.0:7800, always on, with the live panel available
whenever you open the app on the server.

## Releasing (one command)

Cut a release with the bump script  -  it bumps the version, builds the exe
**locally**, then commits/tags/pushes the source to git:

```
powershell -ExecutionPolicy Bypass -File .\scripts\bump-version.ps1 0.1.1
```

What it does, in order:
1. Bumps the version in `gui/src-tauri/tauri.conf.json` and
   `gui/src-tauri/Cargo.toml` (validates semver, BOM-free writes, verifies
   both match).
2. Runs `cargo tauri build`  -  the exe/installer lands under
   `gui/src-tauri/target/release/bundle/` (path printed at the end).
3. `git commit` + `git tag vX.Y.Z` + `git push` (source backup + history).
   Skipped automatically if the folder isn't a git repo.

Flags:
- `-NoBuild`  bump + git only (skip the local build)
- `-NoGit`    bump + build only (no commit/push)
- `-Notes "..."`  release note text for the commit

The downloadable exe is the local build artifact  -  git holds the source, not
the exe.
