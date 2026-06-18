# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with this repository.

## What This Is

This repository is `nano-rs`, a Rust port of GNU nano. The Rust implementation is the canonical code in this checkout.

- `src/` contains the editor implementation.
- `Cargo.toml` defines the build, feature flags, and crate version.
- `media/nano.rc` provides Windows executable version metadata.
- `docs/` contains release and updater documentation.
- `scripts/` contains local build and benchmark helpers.

There is no checked-in C source tree, `rust/` subdirectory, Autotools build, gettext catalog, or `doc/` manual tree in this repository.

## Build And Test

```sh
cargo build
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
```

Feature-gate hygiene matters. Run the matrix relevant to your change:

```sh
cargo check
cargo check --no-default-features
cargo check --no-default-features --features tiny
cargo check --no-default-features --features browser
cargo check --features libmagic
cargo check --all-features
```

The debug binary is `target/debug/nano`; release output is `target/release/nano`.

## Feature Flags

Cargo features mirror GNU nano's optional build areas:

| Cargo feature | Purpose |
|---|---|
| `tiny` | Strips most optional behavior for a minimal editor |
| `color` | Syntax coloring |
| `nanorc` | Runtime nanorc parsing and key rebinding |
| `utf8` | UTF-8 aware behavior |
| `browser` | File browser |
| `help` | Help viewer |
| `histories` | Search, replace, execute, and position histories |
| `justify`, `wrapping`, `multibuffer` | Editing features |
| `mouse`, `linenumbers`, `linter`, `formatter`, `speller` | Optional UI/tools |
| `tabcomp`, `wordcomp`, `comment`, `operatingdir`, `extra` | Additional interactive features |
| `libmagic` | Optional magic-byte syntax fallback via `infer` |

When porting or changing feature-gated code, map C-style conditions this way:

- `#ifdef ENABLE_X` becomes `#[cfg(feature = "x")]`.
- `#ifndef NANO_TINY` becomes `#[cfg(not(feature = "tiny"))]`.
- Shared structs and keycodes often need to remain available even when a feature is off, because additive feature combinations such as `tiny + all-features` must compile.

## Source Architecture

The implementation is a direct module-by-module Rust transliteration of GNU nano:

| Original area | Rust module |
|---|---|
| shared definitions | `definitions.rs` |
| global state and key bindings | `global.rs` |
| main loop, setup, signals | `nano.rs` and `main.rs` |
| terminal I/O and drawing | `winio.rs` |
| files, buffers, writes | `files.rs` |
| movement and scrolling | `move_.rs` |
| search and replace | `search.rs` |
| cut/copy/paste | `cut.rs` |
| prompt/statusbar editing | `prompt.rs` |
| nanorc parsing | `rcfile.rs` |
| history files and position log | `history.rs` |
| self-install and updater | `installer.rs` |

## Rust Design Notes

Global state lives in `global.rs` as `STATE`, with helpers:

```rust
with_state(|s| s.field)
with_state_mut(|s| {
    s.field = value;
})
```

This intentionally matches nano's C-style global state and re-entrant call graph.

Text buffers use linked lists:

```rust
pub type LinePtr = Rc<RefCell<LineNode>>;
```

Back-pointers are `Weak` references. Keep head/tail invariants in sync when editing buffer or cutbuffer chains.

Feature flags use exported macros from `global.rs`:

```rust
ISSET!(FLAG)
SET!(FLAG)
UNSET!(FLAG)
TOGGLE!(FLAG)
```

Terminal rendering uses crossterm. `NanoWindow` replaces ncurses `WINDOW*`, and `winio::translate_key_event` maps crossterm key events to nano integer keycodes.

Translations currently use the `tr!` macro from `main.rs`; it is a pass-through for plain strings and delegates formatting to `format!`.

## Update And Release Notes

The built-in updater tracks the crate version from `Cargo.toml` (`CARGO_PKG_VERSION`). Keep these in sync when bumping versions:

- `Cargo.toml`
- `Cargo.lock`
- `media/nano.rc`
- GitHub release tag and asset names described in `docs/RELEASING.md`

Release assets are expected to match `installer.rs::target_asset_name()`.

## Conventions

- Prefer local module helpers and existing patterns over introducing new abstractions.
- Keep feature-gated code compiling under `--no-default-features`, `tiny`, and `--all-features`.
- Use `rg` for code search.
- Add focused unit tests for parser, path, history, and byte-offset bugs.
- Do not leave partial file writes in updater/install paths; use atomic sibling-temp-file replacement.
