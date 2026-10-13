# HomeTunnel · 家庭隧道

An SSH Agent-only desktop tunnel manager for Windows and macOS.

Early releases are available. Publisher signing and notarization are not yet configured.

## Downloads

- [Windows x64 installer](https://github.com/DongLiStudio/HomeTunnel/releases/latest/download/HomeTunnel-Windows-setup.exe)
- [macOS Apple Silicon DMG](https://github.com/DongLiStudio/HomeTunnel/releases/latest/download/HomeTunnel-Mac.dmg)
- [Release notes and checksums](https://github.com/DongLiStudio/HomeTunnel/releases/latest)

Release assets retain these two stable filenames for portal links; versioned filenames identify a specific build. Verify the SHA256SUMS file included in each release. Windows installers are unsigned; macOS builds are ad-hoc signed and not notarized.

## Boundaries

HomeTunnel accepts structured service links, forwards only to a remote loopback address, waits for the web service, and opens it in the source browser. It does not accept terminal commands, store passwords, export private keys, or bypass host-key verification. Authentication stays with your SSH Agent. An Agent may still require confirmation for every signature.

Both platforms share per-connection open / renew / disconnect, disconnect all, expiry, duplicate-request reuse, direct-local detection, background operation, login startup preferences and installation diagnostics.

## Build

Install a supported Rust toolchain, Node.js and the [Tauri platform prerequisites](https://tauri.app/start/prerequisites/).

```sh
npm ci
npm run tauri dev
npx tauri build --bundles nsis # Windows
npx tauri build --bundles dmg  # macOS
cargo test --manifest-path src-tauri/Cargo.toml
```

## Service links

```text
hometunnel://connect?host=server.example&user=alice&sshPort=22&port=8080&remotePort=8080&name=Private%20service&path=%2F&browser=default
```

`hometunnel` is the only application URL scheme. Complete paths belong to your authenticated portal; there are no local path files. Optional `fingerprint` is the expected SHA256 SSH host public-key fingerprint. Add verified hosts to your own OpenSSH known_hosts before use. HomeTunnel refuses unknown or changed host keys.

## Distribution

Windows uses a per-user NSIS installer. macOS uses an application bundle in a DMG; launch once after installing to register login startup and check Launch Services. Apple notarization and Windows signing need publisher certificates; unsigned test builds are explicitly labeled. Do not bypass OS security warnings.

See [security model](docs/security.md) and [acceptance checklist](docs/acceptance.md).

MIT License. Copyright 2026 DongLiStudio.