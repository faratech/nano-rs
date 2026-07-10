# nano-rs

A faithful, line-by-line Rust port of [GNU nano](https://www.nano-editor.org/) — the small, friendly terminal text editor — translated module-for-module from the original C and then hardened into a real, shippable editor.

> **Unofficial.** This is an independent project, **not affiliated with or endorsed by GNU nano**. Report bugs here → [faratech/nano-rs/issues](https://github.com/faratech/nano-rs/issues), never to the upstream nano project. For the official C editor, see [nano-editor.org](https://www.nano-editor.org/).

## How it got here

It started as a mechanical 1:1 transliteration of nano's C source — same files, same functions, same control flow — so the architecture would stay legible to anyone who knows the original. From there it grew into something you can actually use:

- **Wired the skeleton up.** The first port compiled but was full of stubs. Search/replace, go-to-line, navigation, cut/copy/paste, the file browser, and the help viewer were each ported back to behavioral parity with their C originals.
- **Got the details right.** Unicode input, cursor placement, display fill, mouse-click positioning, multibuffer via a real buffer ring, full syntax-highlight painting with lazy syntax loading, reading from a pipe, and emergency-saving modified buffers on `SIGHUP`/`SIGTERM`/crash.
- **Hunted the parity bugs.** A focused audit pass eliminated **57 correctness-and-parity bugs** in one sweep — restoring C-style linked-list walks in the renderer, killing per-keystroke state lookups in hot paths, and replacing every "simplified" stub that diverged from nano's behavior.
- **Made it small and portable.** A size pass shrank the optimized release binary **4.65× (2.63 MiB → ~580 KB)** with *all* features compiled in, and CI ships static musl Linux builds and native Windows MSVC builds with a built-in self-updater.

The result is ~36k lines of Rust across 19 modules that track the C source one-to-one.

## Quick start

```bash
cargo build --release      # → target/release/nano
./target/release/nano file.txt
```

Requires **Rust 1.85+** (2024 edition). The default build enables the full feature set; a `cargo build --release` yields a stripped, LTO'd binary under 1 MB (a `build-std` size pass takes it to ~580 KB).

## Features & build flags

Cargo features mirror nano's `./configure` switches one-for-one. The default build turns on the lot; `--no-default-features` gives you a bare editor you can opt into:

```bash
cargo build --no-default-features                 # minimal core
cargo build --no-default-features \
  --features=color,nanorc,utf8                    # pick your own
cargo build --release                             # everything (default)
```

| Feature | What it adds |
|---|---|
| `color` | Syntax highlighting |
| `nanorc` | `nanorc` config-file support |
| `utf8` | Unicode text & input |
| `multibuffer` | Multiple open files (buffer ring) |
| `browser` | Built-in file browser |
| `help` | `^G` help viewer |
| `histories` | Search/replace & position history |
| `justify` · `wrapping` | Paragraph justify, hard wrapping |
| `linter` · `formatter` · `speller` | External lint/format/spell hooks |
| `mouse` · `linenumbers` · `comment` | Mouse, line numbers, comment toggle |
| `tabcomp` · `wordcomp` · `operatingdir` | Tab/word completion, `-o` confinement |
| `libmagic` | Native libmagic syntax matching (off by default; requires system libmagic) |
| `tiny` | Strip to the smallest editor (mirrors `--enable-tiny`) |

Dependencies are deliberately few: `crossterm` (terminal I/O), `regex-lite`, `unicode-width`, and `libc`.

## Platforms

| Platform | Targets | Notes |
|---|---|---|
| Linux | `x86_64`, `aarch64` (musl) | Fully static, portable |
| Windows | `x86_64`, `aarch64` (MSVC) | Native; built-in self-updater |
| macOS | standard toolchain | Builds with stock Rust |

Cross-compiling to Windows from Linux uses [`xwin`](https://github.com/Jake-Shadle/xwin) + `lld-link`:

```bash
xwin splat --output /opt/xwin                     # one-time SDK fetch
cargo build --release --target x86_64-pc-windows-msvc
```

## Architecture

The Rust modules mirror the C source file-for-file, so the [upstream nano internals](https://git.savannah.gnu.org/git/nano.git) remain the best reference:

| C file | Rust module | Role |
|---|---|---|
| `definitions.h` | `definitions.rs` | Types, enums, flag constants |
| `global.c` | `global.rs` | Global state, keybinding tables |
| `nano.c` | `nano.rs` / `main.rs` | Main loop, event dispatch |
| `winio.c` | `winio.rs` | Terminal I/O (crossterm) |
| `move.c` | `move_.rs` | Cursor movement |
| `text.c` · `cut.c` | `text.rs` · `cut.rs` | Editing, undo/redo, cut buffer |
| `files.c` · `search.c` | `files.rs` · `search.rs` | File I/O & locking, search/replace |

A few decisions that make the C semantics work in Rust:

- **Re-entrant global state** — `NanoCell(UnsafeCell<AppState>)` accessed via `with_state` / `with_state_mut`, because C functions freely call each other while "borrowing" the globals (a plain `RefCell` panics here).
- **Document as a linked list** — `Rc<RefCell<LineNode>>` with `Weak` back-pointers, walked the same way the C list is.
- **Flag macros** — `ISSET!` / `SET!` / `UNSET!` / `TOGGLE!` stand in for the C bit-flag macros.
- **crossterm replaces ncurses**, and `tr!(...)` stands in for gettext.

Where Rust offers something safer for free, the port takes it: platform code is `#[cfg]`-gated, file APIs use `std::fs`, and sentinel return values become `Result`/`Option` — without changing nano's observable behavior, keybindings, or config format.

## Contributing

PRs welcome. Keep changes faithful to nano's behavior, run `cargo fmt` and `cargo clippy` (warning-free is the default-build standard), and remember this is independent of upstream — patches for the official C editor go to [nano-editor.org](https://www.nano-editor.org/).

## License & credits

GPL-3.0-or-later, same as GNU nano. See `COPYING` / `COPYING.DOC`.

- **Rust port:** Mike Fara
- **Original GNU nano:** Chris Allegretta and the nano contributors

Not created, maintained, or endorsed by the GNU nano project.
