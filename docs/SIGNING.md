# Signing and SmartScreen

Status: the public 0.5.0 release is **unsigned**. The release workflow in
`.github/workflows/release.yml` already contains the signing step, but it is
disabled until the repository variable `SIGNPATH_ENABLED` is set to the string
`true`. This document explains what signing does and does not fix, which
certificate route fits ProjectNeb, and exactly how to switch the workflow on.

No secret values belong in this file. Only variable and secret *names* are
documented.

## What SmartScreen actually checks

Microsoft Defender SmartScreen evaluates two independent signals for a
downloaded file:

1. **Publisher reputation** — is the file signed, and does the signing
   certificate have a known, trusted publisher identity?
2. **File-hash reputation** — has this exact file been downloaded and run by
   many users without reports of malicious behaviour?

A file that is new, or whose certificate is new, has no reputation yet. That is
why a perfectly clean installer can still show **"Windows protected your PC"**.
The two signals are documented in
[SmartScreen reputation for developers](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation).

### Unsigned releases

Without a signature the publisher is shown as **Unknown publisher**. There is no
certificate that can accumulate reputation, so every new version starts from
zero. Windows also stores the **Mark of the Web** on files downloaded from a
browser; a ZIP extracted with Explorer can propagate that mark to the contents,
which is why both the installer and the portable archive can warn.

On Windows 11, **Smart App Control** is stricter than SmartScreen: it can block
unsigned executables outright, including files that were not downloaded from the
internet. A signature is a prerequisite for those users, not a polish item.

### What signing changes

| | Unsigned / self-signed | Signed with a trusted certificate |
| --- | --- | --- |
| Publisher shown | Unknown publisher | Verified publisher name |
| First download of a new version | Warning, always | Warning possible, but publisher reputation can carry over |
| Reputation across versions | None — resets every release | Accumulates for the certificate and publisher |
| Enterprise / Smart App Control | Often blocked | Allowed when policy trusts the publisher |

Signing does **not** make the first download warning-free. Since 2024 EV
certificates no longer bypass SmartScreen; OV and EV are equivalent for
SmartScreen purposes. Microsoft's own comparison is in
[Code signing options for Windows app developers](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options).

Reputation is earned by shipping **every** release with the **same** signing
identity. There is no consumer submission form that grants reputation; Microsoft
states it can take several weeks and hundreds of clean installs. Changing
certificate (even renewing it with a new key) can reset the publisher signal.
Do not modify a signed file after signing: that breaks the signature.

### What ProjectNeb should expect

- The first signed release will still warn for most users. That is normal, not a
  failed signature.
- Warn users in the release notes and the README until reputation accumulates.
- Keep the beta channel and the same SignPath project/certificate for every
  release; do not switch signing routes between releases.
- Publish checksums (`SHA256SUMS.txt`) so users can verify a download even while
  SmartScreen is still learning.

## Options compared

Prices and eligibility change; check the linked pages before committing money.
Figures below are from Microsoft's and Certum's public pages in November 2026.

| Route | Cost | Who can use it | CI friendly | SmartScreen |
| --- | --- | --- | --- | --- |
| **SignPath Foundation** | Free for qualifying OSS | OSI-licensed, public OSS projects | Yes — GitHub Actions is a supported trusted build system | OV-level signature; reputation builds over time |
| **Certum Open Source Code Signing** | €69 for the set (certificate + crypto card + reader), 1 year | Individuals only, for a public OSS project | No — private key lives on a USB smart card | OV-level signature; reputation builds over time |
| **Azure Artifact Signing** (formerly Trusted Signing) | From about $9.99/month (Basic, 5,000 signatures/month); Premium adds 100,000 signatures at a higher monthly price | Organizations in the US, Canada, EU, UK, Australia, NZ, Japan, South Korea, Singapore, Switzerland, Norway, Israel; individuals in the US or Canada only | Yes — managed certificates, no hardware token | OV-level signature; reputation builds over time |
| **Traditional OV certificate** | Typically $150–300/year | Organizations and individuals worldwide | Needs a cloud HSM or a self-hosted runner with the token | Same as above |
| **EV certificate** | $400+/year | Organizations (most CAs) | Same hardware problem | **No advantage over OV since 2024** |
| **Microsoft Store (MSIX)** | Free | Any Store developer | Store handles re-signing | No warnings at all, but requires packaging as MSIX and Store review |
| **Self-signed** | Free | Development only | Easy | Same warning as unsigned for public users |

### Recommendation

1. **Apply to SignPath Foundation first.** ProjectNeb is AGPL-3.0, public, and
   built entirely in GitHub Actions on GitHub-hosted runners, which is exactly
   the target profile. It is free, CI-native, and avoids a USB token.
2. If SignPath declines or the project stops qualifying, use **Azure Artifact
   Signing** when the maintainer is eligible (individuals: US/Canada; otherwise
   an organization in a supported region), or **Certum OSS** as a cheap manual
   fallback.
3. Do **not** buy EV for SmartScreen. It costs more and behaves like OV.

## SignPath Foundation: application guide

SignPath Foundation signs with a certificate issued to **SignPath Foundation**,
not to the individual maintainer. The publisher shown by Windows is
`SignPath Foundation`; SignPath vouches that the binary was built from this
public repository by this CI workflow. SignPath performs the identity and
reputation check on the project, and the private key stays in SignPath's HSM.

The full conditions are on [signpath.org/terms](https://signpath.org/terms).
The practical requirements for ProjectNeb are:

- **OSI-approved license without commercial dual-licensing.** AGPL-3.0 qualifies.
  The Pro subscription described in `docs/repo/PRO.md` sells a *server service*
  (sync, theme store, schedules), not a proprietary build of the client. Keep it
  that way: no closed-source code, licence key check, or paid-only binary in the
  signed installer.
- **Public, maintained, already released, and documented.** The repository is
  public, the 0.5.0 release exists, and the README describes the product. A
  project with no releases is not eligible.
- **Built from CI on GitHub-hosted agents.** The signed artifact must be produced
  by a GitHub Actions workflow, uploaded with `actions/upload-artifact`, and
  submitted from that same run. Local builds cannot be signed.
- **Manual approval per release.** Every signing request is approved by a
  maintainer in SignPath; this is not a fully automatic rubber stamp.
- **Team roles and MFA.** The project must name authors/committers, reviewers and
  approvers, and everyone with SignPath or repository access must use MFA.
- **A code signing policy on the project page.** The README (or a linked page)
  must use the phrase **Code signing policy** and state the required wording,
  the team roles, and the privacy policy.
- **Stable PE metadata.** The artifact configuration enforces product name and
  product version. The installer template now sets
  `VersionInfoProductName=ProjectNeb`,
  `VersionInfoProductVersion={#AppVersion}` and `VersionInfoVersion`; keep these
  directives when editing `installer/ProjectNeb.iss`.

### 1. Prepare the repository

Before applying:

1. Make sure `LICENSE` with the AGPL-3.0 text is at the repository root (GitHub
   detects the licence from there). This branch keeps a copy at
   `docs/repo/LICENSE`; the live `main` branch already has a root `LICENSE`.
2. Add a README section like this, replacing the role links with the real ones:

   ```markdown
   ## Code signing policy

   Free code signing provided by [SignPath.io](https://about.signpath.io),
   certificate by [SignPath Foundation](https://signpath.org).

   - Committers and reviewers: [maintainers](https://github.com/temajmix)
   - Approvers: [owners](https://github.com/temajmix)

   Privacy policy: this program does not transfer any information to other
   networked systems unless specifically requested by the user or the person
   installing or operating it. The optional Pro service is described in
   `docs/repo/PRO.md` and is not part of the installer.
   ```

3. Confirm that the release workflow is the only source of release binaries.
   Do not upload a locally built installer to the same release.
4. Keep all build jobs on `windows-latest` (GitHub-hosted). SignPath's OSS
   connector checks this.
5. Tag a release (or use the existing `v0.5.0`) so the reviewers can see the
   download page and the release process.

### 2. Apply

Open <https://signpath.org/apply> and provide, in English:

- Project name: `ProjectNeb`.
- Repository: `https://github.com/temajmix/ProjectNeb`.
- Licence: `AGPL-3.0`.
- Home/download page: the GitHub repository and its Releases page.
- One or two sentences on what it is: a Windows 11 desktop dashboard (panel,
  dock, launcher, live wallpapers) written in Rust with a WebView2/Vite UI.
- Build description: Rust `stable-x86_64-pc-windows-gnu` with WinLibs MinGW-w64,
  Node 22 for the dashboard, Inno Setup 6 for the installer, all on
  `windows-latest`.
- Link to the workflow file: `.github/workflows/release.yml` on the default
  branch.
- Maintainer identity and the GitHub account that will approve signing requests.
- Team roles: who commits, who reviews, who approves releases.
- Privacy statement (the wording above is sufficient for the client; mention the
  optional Pro service separately).

Be precise and short. Reviewers reject projects they cannot verify, and they
will ask follow-up questions if the repository looks like a fork, a wrapper
around someone else's binaries, or a commercial product with an OSS veneer.

### Eligibility risks specific to ProjectNeb

SignPath reviews what the signed binary actually does. Three parts of ProjectNeb
deserve an explicit answer in the application:

- **Zapret / DPI bypass.** The module changes how traffic to selected services is
  routed to work around ISP-level throttling. SignPath prohibits tools that
  "circumvent security measures of their execution environment". Explain that
  the module is optional, off by default, user-initiated, and aimed at ISP
  throttling rather than at the security of the user's machine or network. If
  reviewers are uncomfortable, ship the core installer without `neb-zapret.exe`
  or ask SignPath before applying.
- **Windhawk and system tweaks.** Any module that changes Windows behaviour must
  be opt-in, show a clear warning, and be reversible. The terms require projects
  to announce system changes and to provide uninstallation.
- **Discord Rich Presence and the Pro service.** The client talks to Discord only
  when the user enables it, and the Pro service stores a minimal account record.
  State this in the privacy policy linked from the README. Do not put any
  proprietary Pro component in the installer.

If any of these cannot be explained cleanly, fix the product or the packaging
before applying; a rejected application is harder to reopen than a delayed one.

### 3. Configure SignPath after acceptance

1. Install the [SignPath GitHub App](https://github.com/apps/signpath) on the
   repository. This is required for origin verification and audit-log checks.
2. In SignPath, add **GitHub.com** as a trusted build system for the
   organization and link it to the project.
3. Create an **artifact configuration**. The uploaded artifact is a ZIP created
   by `actions/upload-artifact`; its root contains one setup executable. This
   configuration signs that executable and enforces the version parameter that
   the workflow passes:

   ```xml
   <artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
     <parameters>
       <parameter name="version" required="true"/>
     </parameters>
     <zip-file>
       <pe-file-set product-name="ProjectNeb" product-version="${version}">
         <include path="ProjectNeb-*-setup.exe"/>
         <for-each>
           <authenticode-sign/>
         </for-each>
       </pe-file-set>
     </zip-file>
   </artifact-configuration>
   ```

   If SignPath reports a metadata mismatch, inspect the built installer with
   `(Get-Item .\dist\ProjectNeb-*-setup.exe).VersionInfo` and align the
   attributes in `installer/ProjectNeb.iss`; do not relax the check silently.

4. Create a **signing policy** (for example `release`) that requires manual
   approval by an approver.
5. Create an API token for a user with submitter permission on that project and
   policy. Store it only as the GitHub secret `SIGNPATH_API_TOKEN`.
6. Note the organization ID, project slug, signing policy slug and artifact
   configuration slug; they become repository variables.

### 4. Switch the workflow on

Add these in **Settings → Secrets and variables → Actions**:

| Kind | Name | Value |
| --- | --- | --- |
| Variable | `SIGNPATH_ENABLED` | `true` (exactly this string; anything else leaves signing off) |
| Variable | `SIGNPATH_ORGANIZATION_ID` | Organization ID from SignPath |
| Variable | `SIGNPATH_PROJECT_SLUG` | Project slug, for example `projectneb` |
| Variable | `SIGNPATH_SIGNING_POLICY_SLUG` | Signing policy slug, for example `release` |
| Variable | `SIGNPATH_ARTIFACT_CONFIGURATION_SLUG` | Artifact configuration slug |
| Secret | `SIGNPATH_API_TOKEN` | API token with submitter permission |

Then run the workflow with `workflow_dispatch` and a version that matches
`rust/Cargo.toml`, approve the request in SignPath, and verify the result:

```powershell
Get-AuthenticodeSignature .\dist\ProjectNeb-<version>-beta-setup.exe | Format-List Status, StatusMessage, SignerCertificate
```

`Status` must be `Valid`. The signer subject will be `SignPath Foundation`, not
`ProjectNeb`; that is expected.

When `SIGNPATH_ENABLED` is unset or `false`, the upload and signing steps are
skipped and the job still publishes the unsigned installer. The release job does
not reference the token at all.

## Certum Open Source Code Signing

Certum (Asseco) sells an **Open Source Code Signing** set for **€69** (certificate
plus a crypto card and reader, one year). It is the cheapest traditional
certificate route, but it has hard constraints:

- Issued to an **individual**; the certificate subject is
  `Open Source Developer <name>`, organisation `Open Source Developer`.
- The project must be publicly available and clearly connected to the
  subscriber; Certum verifies the project URL and identity documents.
- The private key is on a **physical smart card**. Signing requires the card and
  reader on the machine, so it does not work on a hosted CI runner. You would
  sign on a self-hosted Windows runner (or manually) and upload the result.
- Certum revokes the certificate if it is used to sign commercially distributed
  software. ProjectNeb's Pro service must remain a server service, not a paid
  binary, for this route to stay valid.

Useful pages:
[product](https://shop.certum.eu/open-source-code-signing.html) and
[required documents](https://support.certum.eu/en/code-signing-required-documents/).

If you choose Certum, the workflow must be extended with a self-hosted runner or
a manual signing step; the SignPath step cannot use a Certum card.

## Azure Artifact Signing (formerly Trusted Signing)

Microsoft's managed signing service. It is the practical paid alternative to
SignPath and needs no USB token.

**Cost.** Basic starts at about **$9.99/month** and includes **5,000 signatures
per month**; Premium includes **100,000 signatures per month** at a higher
monthly price. Billing starts when the account is created, even if nothing is
signed. Check the
[current pricing page](https://azure.microsoft.com/en-us/pricing/details/artifact-signing/)
before creating resources.

**Requirements.**

- An Azure subscription and a Microsoft Entra tenant.
- An Artifact Signing account (Basic or Premium) in a supported region.
- **Identity validation** in the Azure portal: organisation legal name, address,
  business identifier, verified domain email, and identity verification of a
  representative. Individuals must use an Azure billing account of type
  Individual whose legal name and address match their government ID.
- Public Trust availability: organisations in the US, Canada, EU, UK, Australia,
  New Zealand, Japan, South Korea, Singapore, Switzerland, Norway and Israel;
  individual developers only in the US or Canada. Private Trust has different
  rules.
- Identity validation takes roughly **1–30 business days**, sometimes longer if
  documents are requested. Plan it well before a release.
- Signing is done with `signtool` against the certificate profile endpoint
  (`https://<region>.codesigning.azure.net`), or with a GitHub Action that wraps
  it. No certificate or private key ever lands on the runner.

Signing with Azure in this workflow would replace the SignPath step with an
Azure login plus `signtool sign /tr <timestamp> /td sha256 /fd sha256 ...`.
That is intentionally not implemented here because the repository must not
carry Azure credentials until the account exists.

## Microsoft Store (the only guaranteed no-warning route)

Microsoft re-signs MSIX packages submitted to the Store; Store-installed apps do
not show SmartScreen warnings. This would require packaging ProjectNeb as MSIX
(and dealing with the shell/wallpaper components, which are not a good fit for
MSIX today). It is listed for completeness, not as the next step.

## What to do for each signed release

1. Bump `rust/Cargo.toml`, commit the release notes, and tag `v<version>` only
   after the version matches. The workflow refuses a mismatch.
2. Approve the signing request in SignPath when it appears.
3. Download `SHA256SUMS.txt` from the release and compare it with the local
   manifest. The hash in
   `packaging/winget/manifests/ProjectNeb.ProjectNeb.installer.yaml`
   must be the hash of the **signed** installer, so update it after enabling
   signing.
4. If the release was already published unsigned and then re-signed, do not
   replace the asset under the same version in a winget manifest that has already
   been merged; publish the signed build as a new version instead.
5. Announce that the release is signed but that SmartScreen may still warn until
   reputation accumulates.
