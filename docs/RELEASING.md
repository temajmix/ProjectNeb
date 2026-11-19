# Releasing ProjectNeb

Every public release produces the same two assets:

- `ProjectNeb-<version>-<channel>-setup.exe` — Inno Setup installer.
- `ProjectNeb-<version>-<channel>-portable.zip` — portable tree, including `ui/`.

The current channel is `beta`, so the 0.5.0 assets are
`ProjectNeb-0.5.0-beta-setup.exe` and `ProjectNeb-0.5.0-beta-portable.zip`.
The GitHub tag is `v<version>` without the channel: `v0.5.0`.

There are two supported ways to release. Both build the same binaries and the
same two artifacts; pick one per release and do not mix them.

## 1. Release from the maintainer machine (`neb-cli release`)

This is the existing local pipeline. It builds, compiles the installer, creates
the GitHub release with `gh`, computes SHA-256, and updates `latest.json` and
`CHANGELOG.md` in the site repository.

Requirements:

- Windows 11 x64 with the Rust GNU toolchain and WinLibs MinGW-w64 on `PATH`.
- Node 22 and `npm ci`/`npm run build` in `src/Neb.UI`.
- Inno Setup 6 (`ISCC` on `PATH` or in
  `%LocalAppData%\Programs\Inno Setup 6\ISCC.exe`).
- An authenticated `gh` CLI (`gh auth status`).
- A working copy of the site repository with `latest.json` and `CHANGELOG.md`
  (default `A:\ProjectNeb-wt\site`, override with `--manifest-repo`).
- A clean working tree apart from the version bump the pipeline makes.

Plan first, then run:

```powershell
cd A:\ProjectNeb-wt\release\rust
cargo run -p neb-cli -- release --version 0.6.0 --notes "Release notes" --dry-run
cargo run -p neb-cli -- release --version 0.6.0 --notes "Release notes"
```

`neb-cli release` rewrites `[workspace.package] version` in `rust/Cargo.toml`,
runs `cargo test --workspace`, builds `--release`, stages the portable tree,
compiles the installer from a rendered copy of `installer/ProjectNeb.iss`,
publishes the GitHub release, and pushes the site manifest. It does **not**
commit the version bump in this repository; commit `rust/Cargo.toml`
(and `rust/Cargo.lock`) before tagging or before the next release.

## 2. Release from GitHub Actions (tag flow)

The workflow is `.github/workflows/release.yml`.

### Before tagging

1. Bump `[workspace.package] version` in `rust/Cargo.toml` and refresh
   `rust/Cargo.lock` (`cargo metadata --format-version 1 > $null` or a build).
   The workflow fails if the version in `Cargo.toml` does not match the tag.
2. Write the release notes to `dist/release-notes-<version>.md` (the workflow
   uses it when the dispatch `notes` input is empty). Keep the format of the
   existing files.
3. Run the pre-flight checklist below.
4. Commit and push. Open a pull request if the branch is protected.
5. Make sure the merged commit is what you want to ship.

### Tag and publish

```powershell
git tag -a v0.6.0 -m "ProjectNeb 0.6.0 Beta"
git push origin v0.6.0
```

The `v*` push starts the workflow:

1. `windows-latest` installs Rust GNU, WinLibs, Node 22 and Inno Setup 6.
2. It checks formatting and runs clippy with `-D warnings`, the workspace tests
   (`NEB_SKIP_DESKTOP_TESTS=1`), and the Vite build.
3. It builds `--release`, stages the portable archive, renders the installer
   script with the version and checkout path, and compiles the installer.
4. If `SIGNPATH_ENABLED` is `true`, it uploads the unsigned installer, submits a
   SignPath signing request, waits for manual approval, verifies the returned
   signature, and replaces the installer in `dist/`.
5. It writes `release-notes.md` and `SHA256SUMS.txt`, then uploads the four
   files as the `release-assets` artifact.
6. On a tag, the `release` job downloads that artifact and creates (or updates)
   the GitHub release with the two binaries, the notes file and the checksums.

A `workflow_dispatch` run with the same version builds and uploads the artifact
but does **not** create a release. Use it to test the pipeline, including
signing, before tagging.

The CI flow does **not** update `latest.json` or `CHANGELOG.md` in the site
repository. Update those separately, or keep using the local pipeline for that
part.

## Pre-flight checklist

Run every item before tagging or before `neb-cli release`.

- [ ] `rust/Cargo.toml` version is the version being released.
- [ ] `cargo fmt --all --check` passes in `rust/`.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes in `rust/`.
- [ ] `cargo test --workspace` passes locally (without
      `NEB_SKIP_DESKTOP_TESTS`, so the desktop tests actually run).
- [ ] `npm ci && npm run build` passes in `src/Neb.UI`, and
      `src/Neb.UI/dist/index.html` exists.
- [ ] `cargo build --workspace --release` produces every file in
      `COPIED_FILES` (including `WebView2Loader.dll`).
- [ ] The portable archive contains the binaries, `start-portable.cmd`,
      `README-PORTABLE.txt`, `README.md` and `ui/`.
- [ ] A clean Windows 11 x64 VM installs from the setup, starts ProjectNeb, shows
      the dashboard, and survives a reboot with autostart enabled.
- [ ] The uninstaller removes the app and autostart entry. User data in
      `%LocalAppData%\ProjectNeb` may remain by design; confirm that is the
      documented behaviour.
- [ ] The portable archive runs from a directory outside `Program Files` and
      writes only to `%LocalAppData%\ProjectNeb`.
- [ ] `dist/release-notes-<version>.md` is accurate and in the right language.
- [ ] `SHA256SUMS.txt` from the release matches the downloaded assets.
- [ ] `packaging/winget/manifests/ProjectNeb.ProjectNeb.installer.yaml` has the
      final version, URL, and the SHA-256 of the **published** installer.
- [ ] If signing is enabled: the SignPath request was approved and the downloaded
      installer reports `Status: Valid`.
- [ ] If signing is enabled: the winget manifest hash is the signed installer's
      hash, not the pre-signing build.
- [ ] The release is not expected to be silent on SmartScreen yet; the notes
      explain the warning if the certificate is new.
- [ ] `latest.json` and `CHANGELOG.md` are ready to publish if you maintain the
      site.

## After publishing

1. Open the release and check both assets, the notes, and `SHA256SUMS.txt`.
2. Download both assets on a machine that has never seen them and compare the
   hashes.
3. For a signed release, verify the signature:
   ```powershell
   Get-AuthenticodeSignature .\ProjectNeb-<version>-beta-setup.exe | Format-List *
   ```
4. Update the site manifest (`latest.json`, `CHANGELOG.md`) if the CI flow was
   used.
5. Update `packaging/winget/manifests/` and submit the manifest (see
   `packaging/winget/README.md`).
6. Announce the release. For a new signing certificate, tell users that the
   first downloads may still show SmartScreen.

## Rollback

There is no way to make users un-download a file. Prefer a fast follow-up over
rewriting history, and only delete a release when it is actively harmful.

### Broken build, release still useful

1. Edit the release notes with a warning and, if available, mark the release as
   a pre-release.
2. Publish a fixed version with a higher version number.
3. Update `latest.json` to the fixed version.
4. In winget, submit a new manifest for the fixed version. Do not silently
   change the hash of an already-merged version.

### Release must be withdrawn

1. Delete the GitHub release and the tag:
   ```powershell
   gh release delete v0.6.0 --repo temajmix/ProjectNeb --yes --cleanup-tag
   ```
2. Restore `latest.json` in the site repository to the previous version and
   push the revert. The updater polls that file, so this is the step that stops
   new installs.
3. If a winget manifest was already merged, submit a new version that points to
   the fixed installer. Winget does not support deleting a published version
   from the repository history; if the package is unsafe, open an issue with
   `microsoft/winget-pkgs` and ask the maintainers to remove it.
4. If the manifest PR is still open, close it or push a revert commit.
5. Re-run the workflow only after the cause is fixed. A tag can be recreated
   with `git tag -a`, but the workflow's release job updates an existing release
   in place, so do not recreate the tag until the old release is gone.

Deleting a release does not invalidate downloads that already happened, and it
does not remove cached copies. Treat the release notes and the site manifest as
the source of truth for what users should install.
