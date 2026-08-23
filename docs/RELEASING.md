# Releasing nano-rs

This document describes how nano-rs releases are built and published, how the
auto-update feature works (Windows **and** Linux), and how to cross-build
binaries locally from Linux/WSL.

## How releases work

Releases are driven by git tags. Pushing a tag that matches `v*` triggers a
GitHub Actions workflow that builds four assets, one per platform/arch:

| Target triple | Runner | Release asset |
|---|---|---|
| `x86_64-pc-windows-msvc`    | `windows-latest`   | `nano-amd64.exe` |
| `aarch64-pc-windows-msvc`   | `windows-latest`   | `nano-arm64.exe` |
| `x86_64-unknown-linux-musl` | `ubuntu-latest`    | `nano-linux-amd64` |
| `aarch64-unknown-linux-musl`| `ubuntu-24.04-arm` | `nano-linux-arm64` |

The Cargo binary is named `nano` (`nano.exe` on Windows); the workflow stages it
to the per-platform asset name above, then creates a GitHub Release for the tag
and attaches all four assets. The asset names are exactly what the in-app
updater looks for, so they must not change without updating
`src/installer.rs::target_asset_name()`.

> The Linux builds use the **musl** target, producing fully static binaries with
> no glibc dependency — they run on any Linux distro and are safe to self-update
> across hosts. The `ubuntu-24.04-arm` runner is free for public repositories;
> if it is unavailable, build the arm64 Linux asset with
> [`cross`](https://github.com/cross-rs/cross) instead.

To cut a release:

```sh
python3 bump-version.py minor          # or major / patch / --set X.Y.Z
git commit -am 'Bump version to X.Y.Z'
git tag -a vX.Y.Z -m 'Release X.Y.Z'
git push && git push --tags
```

## Auto-update (Windows and Linux)

nano-rs ships with a cross-platform self-update capability:

- `nano --install` — copy the running binary to a directory on your PATH
  (`%LOCALAPPDATA%\Microsoft\WindowsApps\nano.exe` on Windows,
  `~/.local/bin/nano` on Linux/Unix).
- `nano --update` — query the GitHub Releases API and, if a newer release
  exists, download the matching asset and install it.
- `nano --force` — with `--install`/`--update`, act even if already up to date.
- **Background check on launch**:
  - **On by default on every platform.** ~3 s after startup nano-rs checks for
    a newer release and, if one is found, downloads it and shows a status-bar
    notice (`Update vX downloaded — restart nano to apply.`). The newer binary
    is swapped in via an atomic rename on the next launch (and, since 0.0.16,
    also refreshed in `~/.local/bin` when the running copy lives elsewhere).
  - **Disabling**: set `NANO_UPDATE_CHECK=0` (also accepts `false`, `no`,
    `off`, or an empty value) — this was previously opt-in on Linux and is now
    the supported kill switch there. `NANO_NO_UPDATE_CHECK=1` also disables
    and takes precedence over `NANO_UPDATE_CHECK`.
  - The check is throttled to **once per 24 h** of *completed* checks — a
    failed/offline attempt never blocks the next launch from retrying — runs
    only when at least one applicable location is user-writable (so a system
    `/usr/bin/nano` alone never triggers a download), never blocks on the
    network, and is silent on failure.
  - The manual `nano --check` probe ignores these environment variables: it
    always asks GitHub when you explicitly run it.

### Security & portability notes

- The pending-update file is stored in a **per-user, private** directory
  (`~/.cache/nano-rs/` mode `0700` on Unix, `%LOCALAPPDATA%\nano-rs\` on
  Windows) — never in world-writable `/tmp` — and on Unix its ownership is
  verified before it is ever applied, so another local user cannot trick nano
  into installing their binary.
- The published Linux assets are **musl-static** (no glibc dependency), so a
  self-update cannot leave you with a binary that won't start due to a glibc
  version mismatch — the updated binary runs on any Linux host the original did.

How downloads happen, with **no extra dependencies**:

- **Windows**: native WinHTTP (no PowerShell). The final HTTP status is checked
  after redirects, and a truncated read is treated as failure, so a partial or
  error response is never installed as the executable.
- **Linux/Unix**: shells out to `curl` (falling back to `wget`) — both follow
  redirects and fail on HTTP 4xx/5xx, giving the same safety.

The updater selects the asset matching the host OS and architecture:
`nano-{amd64,arm64}.exe` on Windows, `nano-linux-{amd64,arm64}` on Linux.

## Cross-building locally from Linux / WSL

You can produce the same Windows binaries locally using
[`cargo-xwin`](https://github.com/rust-cross/cargo-xwin), which supplies the
MSVC target without a Windows host:

```sh
cargo install cargo-xwin
rustup target add x86_64-pc-windows-msvc aarch64-pc-windows-msvc

# 64-bit Intel/AMD
cargo xwin build --release --target x86_64-pc-windows-msvc

# 64-bit ARM
cargo xwin build --release --target aarch64-pc-windows-msvc
```

Output binaries land in
`target/<triple>/release/nano.exe`. Rename them to `nano-amd64.exe` /
`nano-arm64.exe` to match the release assets.

### Static CRT tip

By default the MSVC target dynamically links the Visual C++ runtime, which
makes the resulting `nano.exe` depend on `VCRUNTIME140.dll` being present on
the target machine. To produce a fully self-contained binary, statically link
the CRT:

```sh
RUSTFLAGS="-C target-feature=+crt-static" \
    cargo xwin build --release --target x86_64-pc-windows-msvc
```

This avoids the `VCRUNTIME140.dll` dependency so the binary runs on a clean
Windows install with no redistributable.

## Two version numbers

nano-rs surfaces two distinct versions in `--version`:

```
 GNU nano, version 9.0.0                                   <- upstream compatibility
 nano-rs 0.0.1 (Rust port) — https://github.com/faratech/nano-rs   <- nano-rs release
```

- **GNU nano version** (`GNU_NANO_VERSION` in `src/definitions.rs`) is the
  upstream GNU nano release this port mirrors. It only changes when the port is
  rebased onto a newer GNU nano, and it's also what goes into lock files and the
  credits screen for compatibility.
- **nano-rs version** is the crate version (`Cargo.toml` → `CARGO_PKG_VERSION`).
  **This is the one that matters for releases**: git tags (`vX.Y.Z`) and the
  self-updater's version comparison both track it, so a release tag must match
  the crate version.

## Bumping the version

`bump-version.py` bumps the **nano-rs** release version (Cargo.toml + the Windows
resource); the upstream `GNU_NANO_VERSION` const is edited by hand only when
rebasing onto a new GNU nano. Keep the tag equal to the crate version:

```sh
python3 bump-version.py patch          # 9.0.0 -> 9.0.1
python3 bump-version.py minor          # 9.0.0 -> 9.1.0
python3 bump-version.py major          # 9.0.0 -> 10.0.0
python3 bump-version.py --set 9.2.0    # set an exact version
python3 bump-version.py minor --dry-run  # preview without writing
```

It updates:

- `Cargo.toml` — the `version` field.
- `media/nano.rc` — `FILEVERSION` / `PRODUCTVERSION` (comma form) and the
  `FileVersion` / `ProductVersion` string values, so the embedded Windows
  version metadata matches the crate version.
