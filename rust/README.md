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
    neb-cli/            diagnostics CLI
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

The release binary is `target/release/neb-cli.exe`.

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
