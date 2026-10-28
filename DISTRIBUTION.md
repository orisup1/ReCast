# Distributing ReCast

Use GitHub Releases for the current app. The existing
[Binaries workflow](.github/workflows/binaries.yml) tests all three platforms,
builds the downloads, and uploads a draft release with SHA-256 checksums.
Users do not need Rust or separate dictionary files.

## Release steps

1. Set the new version in `Cargo.toml` and update `Cargo.lock` with `cargo check`.
   Commit and push the release candidate. Use a new version for each release;
   rerunning an existing tag overwrites its assets.
2. In GitHub, open **Actions > Binaries > Run workflow**. Select the candidate
   branch, leave **tag** empty to use `v<Cargo.toml version>`, and keep
   **Publish as a draft** enabled. An explicit tag must match the crate version;
   an existing tag must point to the commit being built.
3. Wait for the quality, build, and publish jobs. Open the resulting draft in
   **Releases**, add release notes, and test the downloaded assets on clean
   machines or accounts. Follow the macOS smoke test in the README and verify
   correction, undo, completion, and start at login on each supported OS.
4. Publish the reviewed draft and share the release page. GitHub documents
   [creating and publishing releases](https://docs.github.com/en/repositories/releasing-projects-on-github/managing-releases-in-a-repository).

The workflow can also be started with an authenticated GitHub CLI:

```sh
gh workflow run binaries.yml --ref main -F draft=true
```

Use the branch containing the candidate if it is not `main`. No release is
published by a normal push: this workflow runs only when manually dispatched.
The draft input applies when creating a release; it does not hide an already
published release when uploading replacement assets.

## Downloads and setup

| Download | Users |
| --- | --- |
| `ReCast.app.zip` | macOS, Intel and Apple Silicon; extract and move to `/Applications` |
| `recastMac` | macOS command-line or service use, both architectures |
| `ReCast.exe` | Windows x86-64; portable tray app |
| `recastLinux` | Linux x86-64; executable download |
| `SHA256SUMS` | Checksums for all four downloads |

All platforms need English and Hebrew keyboards. macOS users grant Accessibility
access through setup. Linux users need `evdev`/`uinput` access; the README describes
the input group and source-based service installation. The Linux binary is built
on `ubuntu-latest`, so test the oldest distribution you intend to support rather
than assuming it works on every glibc version. There is no packaged installer or
automatic updater in this release path.

## Signing credentials

Configure repository **Settings > Secrets and variables > Actions** before a
public release. The workflow falls back to an ad-hoc macOS signature and an
unsigned Windows executable when signing credentials are absent.

For macOS, use a Developer ID Application certificate and notarize the app.
Apple documents [notarization before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).
The existing workflow reads:

- `MACOS_CERTIFICATE`: Base64-encoded `.p12` certificate and private key.
- `MACOS_CERTIFICATE_PASSWORD`: Password protecting that export.
- `KEYCHAIN_PASSWORD`: Password for the temporary CI signing keychain.
- `MACOS_SIGNING_IDENTITY`: Full Developer ID Application signing identity.
- `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD`: Notarization credentials;
  the password is an Apple app-specific password.

For Windows, the current workflow imports an exportable PFX and applies an
Authenticode signature with a timestamp:

- `WINDOWS_CERTIFICATE`: Base64-encoded `.pfx` certificate and private key.
- `WINDOWS_CERTIFICATE_PASSWORD`: Password protecting that export.

Hardware-backed or hosted signing services need their own workflow integration.
Signing identifies the publisher; a new signed Windows release can still trigger
SmartScreen until it gains reputation. See Microsoft's
[SmartScreen guidance](https://learn.microsoft.com/windows/apps/package-and-deploy/smartscreen-reputation).

Installers and package-manager listings can follow once this download-and-setup
path is verified. They are not required to distribute the existing app.
