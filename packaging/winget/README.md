# winget manifests

Manifests for the current public release of ProjectNeb, in the layout that
`winget` and [`microsoft/winget-pkgs`](https://github.com/microsoft/winget-pkgs)
expect:

```
packaging/winget/
  README.md
  manifests/
    ProjectNeb.ProjectNeb.yaml                    # version
    ProjectNeb.ProjectNeb.locale.en-US.yaml       # default locale
    ProjectNeb.ProjectNeb.installer.yaml          # installer
```

The manifests live in `manifests/` rather than next to this README because
`winget validate --manifest <directory>` parses every file in the directory, and
Markdown is not YAML.

| Field | Current value |
| --- | --- |
| Package identifier | `ProjectNeb.ProjectNeb` |
| Package version | `0.5.0` |
| Installer | `ProjectNeb-0.5.0-beta-setup.exe` |
| Installer URL | `https://github.com/temajmix/ProjectNeb/releases/download/v0.5.0/ProjectNeb-0.5.0-beta-setup.exe` |
| SHA-256 | `7420C9BC10287A8736C4FFB26C45CDB8A23DAC0168708DB26F92F46CA277ECDC` |
| Installer type | `inno` |
| Scope | `user` |
| Silent switches | `/SILENT /SUPPRESSMSGBOXES /NORESTART` |
| Minimum OS | Windows 11 (`10.0.22000.0`), x64 |

The hash above is the SHA-256 of the installer attached to the **live**
`v0.5.0` release (4,668,781 bytes), which is also the file committed at
`dist/ProjectNeb-0.5.0-beta-setup.exe`. It is an unsigned build.

## Version naming

The GitHub tag is `v0.5.0` and the manifest `PackageVersion` is `0.5.0`, but the
asset file name carries the release channel: `ProjectNeb-0.5.0-beta-setup.exe`.
This is deliberate:

- The tag and the application version are `0.5.0`.
- `-beta` is a channel marker in the file name, not a prerelease suffix in the
  version. Inno Setup writes `DisplayVersion=0.5.0`, which is what winget and
  Programs and Features compare.
- Do not set `PackageVersion: 0.5.0-beta` unless the tag and the installed
  version actually become `0.5.0-beta`; the URL and the ARP version would then
  disagree with the manifest.

## Validate

From the repository root:

```powershell
winget validate --manifest .\packaging\winget\manifests
```

A successful run prints `Manifest validation succeeded.` The `winget` CLI must
be recent enough for the manifest schema (`ManifestVersion: 1.6.0`; current
1.29 validates it).

## Test install and uninstall

`winget install --manifest` installs straight from the local manifests and uses
the same silent switches that the submission will use. Run it in a disposable
Windows 11 VM or a test account:

```powershell
winget install --manifest .\packaging\winget\manifests --accept-package-agreements --accept-source-agreements
winget list --id ProjectNeb.ProjectNeb
winget uninstall --id ProjectNeb.ProjectNeb
```

Check that the dashboard starts, that the uninstaller removes the app and the
autostart entry, and that `%LocalAppData%\ProjectNeb` keeps the user's config
and logs (that is the documented behaviour).

## Submit to microsoft/winget-pkgs

The easiest path is WingetCreate:

```powershell
winget install --id Microsoft.WingetCreate
wingetcreate submit --token <GitHub personal access token> .\packaging\winget\manifests
```

`wingetcreate` copies the manifests to
`manifests/p/ProjectNeb/ProjectNeb/0.5.0/`, validates them, and opens a pull
request against `microsoft/winget-pkgs`. The token needs the `public_repo` scope
on a fork of `winget-pkgs` (or use `wingetcreate submit` and follow its login
flow).

Manual path:

1. Fork `microsoft/winget-pkgs`.
2. Copy the three files into
   `manifests/p/ProjectNeb/ProjectNeb/0.5.0/`.
3. Commit, push, and open a pull request. The title is conventionally
   `New package: ProjectNeb.ProjectNeb version 0.5.0`.
4. The winget-pkgs validation pipeline runs `winget validate`, installs the
   package in a sandbox, and checks the uninstaller. Fix anything it reports in
   the same PR.

## When the signing certificate is enabled

Signing changes the installer bytes, so the SHA-256 changes. Before submitting a
new version:

1. Enable SignPath (see `docs/SIGNING.md`), build the release, and confirm the
   installer signature is valid.
2. Replace `InstallerSha256` with the hash of the **signed** installer and
   recompute it from the downloaded release asset, not from an intermediate
   file.
3. If the manifest for `0.5.0` has already been merged into winget-pkgs, do not
   edit that version's hash. Publish the signed build as the next version
   (for example `0.5.1`) instead. Winget keeps an immutable history for good
   reason: users and mirroring services rely on a version identifying one set of
   bytes.

The manifest is intentionally not submitted by this repository's release
workflow. A maintainer reviews the hash and opens the winget PR after the GitHub
release exists.
