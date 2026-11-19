# ProjectNeb — Rust workspace

Rewrite of the ProjectNeb runtime without Chromium or the .NET runtime in the resident path.
Phase 1 covers the core crate (config, logging, IPC, metrics) and a diagnostics CLI; the C# tree in
`../src` stays in the repository as the behavioural reference until feature parity.

## Layout

```
rust/
  Cargo.toml            workspace, release profile
  rust-toolchain.toml   stable-x86_64-pc-windows-gnu + rustfmt + clippy
  crates/
    neb-core/           config, logging, paths (IPC, metrics land in later tasks)
    neb-app/            settings window: Win32 + WebView2, the `neb-app.exe` binary
    neb-supervisor/     process manager and product entry point, the `ProjectNeb.exe` binary
    neb-shell/          the bar, dock and launcher
    neb-wallpaper/      the desktop engine
    neb-discord/        Discord Rich Presence IPC client, presets and rules
    neb-zapret/         the DPI-bypass module: pack import, strategy parsing, winws helper
    neb-windhawk/       Windhawk integration: install detection, catalog, mods and UAC helper
    neb-cli/            diagnostics CLI
    neb-updater/        applies a staged update after the supervisor exits
```

## Build and test

The GNU toolchain links through WinLibs; if `cargo` reports a missing `-lgcc`, the MinGW `bin`
directory is not on `PATH`:

```powershell
$mingw = (Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Directory -Filter "BrechtSanders.WinLibs.POSIX.UCRT*" | Select-Object -First 1).FullName + "\mingw64\bin"
$env:Path = "$env:USERPROFILE\.cargo\bin;$mingw;$env:Path"
```

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```

The release binary is `target/release/ProjectNeb.exe`. It starts the shell, the wallpaper and the
settings window; a second launch focuses the running window instead of starting another copy.
`--no-settings` (or `--tray-only`) starts the children without a window and is what the installer's
autostart entry uses.

## Settings window (`neb-app`)

`neb-app.exe` is the only ProjectNeb binary that uses WebView2. It loads the Vite build from
`ui/index.html` next to itself (the installer puts it in `%LocalAppData%\ProjectNeb\app\ui`) and
falls back to an embedded "UI не собран / the UI is not built" page when that folder is absent, so
a checkout without `npm run build` still opens a window with the version on it.

Two build steps are easy to miss:

- `src/Neb.UI` has to be built and copied next to the binary before the real dashboard appears:
  `npm ci; npm run build` in `src/Neb.UI`, then copy `dist` to `target/release/ui`.
- the GNU toolchain links `neb-app.exe` against `WebView2Loader.dll`'s import library, so
  `neb-app`'s build script copies the loader the `webview2-com-sys` crate ships into
  `target/<profile>/WebView2Loader.dll`. Both the installer and the portable archive carry it; a
  hand-made copy of the exe alone will not start.

Closing the window destroys the WebView2 controller: no `msedgewebview2.exe` process started by
`neb-app` survives it. The bridge methods (`config.get`, `config.set`, `themes.list`, `themes.save`,
`shell.reload`, `shell.getState`, `app.version`, `updates.check`, `window.*`) are documented in
`crates/neb-app/src/bridge.rs`.

## Zapret (`neb-zapret`)

The "обход блокировок" module imports the user's own zapret pack (it never bundles
`winws.exe`, `WinDivert64.sys` or the lists) and runs one of its `general*.bat` strategies.
`neb-zapret.exe` is the non-elevated manager; `--run`/`--stop` are the elevated helper modes it
launches through UAC. The manager answers on the `ProjectNeb.zapret` pipe:

- `zapret.getState`, `zapret.listStrategies`, `zapret.start {strategy}`, `zapret.stop`,
  `zapret.import {path}`, `zapret.selfTest`, `zapret.ping`.

The same commands are available on the command line, which is what the tests and the diagnostics
use:

```powershell
$env:NEB_HOME = "$env:TEMP\neb-zapret-sandbox"
cargo run -p neb-zapret -- --import "D:\zapret-discord-youtube-main"
cargo run -p neb-zapret -- --list
cargo run -p neb-zapret -- --start "general"
cargo run -p neb-zapret -- --state
cargo run -p neb-zapret -- --stop
```

`config.json` gains an additive `zapret` object:

```json
"zapret": { "enabled": false, "packPath": null, "strategy": null, "autoStart": false }
```

Only the helper runs elevated. The status file it writes, `<data>\zapret\status.json`, is the one
source of truth for the pid and the last transition.

## Rules

- No async runtime and no GUI framework in `neb-core`.
- Data lives in `%LocalAppData%\ProjectNeb`; nothing needs administrator rights; nothing talks to the network.
- `config.json` keeps the C# schema: camelCase keys, string enums, unknown keys preserved across a
  load/save cycle.

## Data directory

Everything a ProjectNeb process writes lives under one root, resolved in this order:

1. `NEB_HOME` — an explicit override that replaces the root itself. `NEB_HOME=D:\NebData` puts the config
   at `D:\NebData\config.json`, not `D:\NebData\ProjectNeb\config.json`. The test suites and portable
   builds use it; leave it unset for a normal install.
2. `SHGetKnownFolderPath(FOLDERID_LocalAppData)\ProjectNeb` — what the shell says, which is what the C#
   build reads, so both builds agree even when `%LOCALAPPDATA%` is stale or a profile is redirected.
3. `%LOCALAPPDATA%\ProjectNeb` — for environments without a shell.
4. Otherwise the process reports `PathsError::NoDataRoot` instead of guessing; a wrong directory is
   worse than a stopped process. `paths::root()` is the only place that turns that into a panic, for
   callers that have no way to report it.

```powershell
$env:NEB_HOME = "$env:TEMP\neb-sandbox"
cargo run -p neb-cli -- config path   # prints $env:TEMP\neb-sandbox\config.json
```
