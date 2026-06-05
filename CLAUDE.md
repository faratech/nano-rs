# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

GNU nano — a small terminal text editor. Two independent implementations live here:

- **`src/`** — the canonical C99 / GNU Autotools codebase (upstream: Savannah git)
- **`rust/`** — a 1:1 Rust transliteration on branch `rust-port` (see below)

Contributions to the C codebase are submitted as patches to `nano-devel@gnu.org` (see `README.hacking`), not via GitHub PRs.

---

## C build

From-git checkout — no generated `configure` yet:

```sh
./autogen.sh      # needs autoconf/automake/autopoint/gettext
./configure       # add --sysconfdir=/etc to use /etc/nanorc
make              # produces src/nano
```

Requires `libncurses-dev`. The bundled gnulib tree in `lib/` compiles into `libgnu.a`.

### C feature flags

nano is heavily `#ifdef`-gated. Key `./configure` switches:

| Flag | Effect |
|---|---|
| `--enable-tiny` | defines `NANO_TINY`, strips most optional code |
| `--disable-color` / `--disable-nanorc` / etc. | disables individual features |
| `--enable-debug` | debug build |

Code guarded by `#ifndef NANO_TINY` and `ENABLE_*` macros must compile cleanly when that feature is disabled.

### C testing

No unit-test suite. The regression check is a **build matrix**:

```sh
./nano-regress   # configures + make clean all across all flag combos
```

Run after touching any `#ifdef`/`#ifndef` block. Behavioral testing is manual: `src/nano scratchfile`.

---

## Rust build (`rust/`)

```sh
cd rust
cargo build                          # debug binary → rust/target/debug/nano
cargo build --release                # release binary → rust/target/release/nano
cargo build --no-default-features    # bare build (verify feature-flag hygiene)
cargo check                          # fast type-check without linking
```

The current binary is already compiled at `rust/target/debug/nano`.

### Rust feature flags

Cargo features mirror the C `./configure` switches exactly:

| Cargo feature | C equivalent |
|---|---|
| `tiny` | `--enable-tiny` / `NANO_TINY` |
| `color` | `ENABLE_COLOR` |
| `nanorc` | `ENABLE_NANORC` |
| `utf8` | `ENABLE_UTF8` |
| `multibuffer`, `wrapping`, `justify`, `browser`, `help`, `histories`, `mouse`, `linenumbers`, `linter`, `formatter`, `speller`, `tabcomp`, `wordcomp`, `comment`, `libmagic`, `operatingdir` | corresponding `ENABLE_*` |

`#ifdef ENABLE_X` → `#[cfg(feature = "x")]`; `#ifndef NANO_TINY` → `#[cfg(not(feature = "tiny"))]`.

---

## C source architecture (`src/`)

No central event dispatcher; all modules share global state declared in `prototypes.h` and defined in `global.c`. Read `definitions.h` first — it holds every struct, enum, and feature `#ifdef` (`linestruct`, `openfilestruct`, `keystruct`/`funcstruct`, `colortype`/`syntaxtype`).

Key files and their roles:

- **`nano.c`** — `main()`, option parsing, main input loop, signal handling, terminal setup/teardown.
- **`global.c`** — keybinding system. `shortcut_init()` builds `allfuncs` / `sclist` linked lists via `add_to_funcs` / `add_to_sclist`. To add a new command: wire it here, implement in the relevant file.
- **`winio.c`** — all terminal I/O: keystroke decoding (escape sequences, mouse, UTF-8), screen painting (edit window, title bar, status bar, prompt bar). Largest file.
- **`rcfile.c`** — parses nanorc: `bind`/`set`/`color`/`syntax` directives. Must stay in sync with `global.c` and `doc/`.
- **`text.c`** — text modification: insert/delete, undo/redo (`undostruct`), wrapping, justify, spell/lint/formatter invocation, indent, comment toggle.
- **`files.c`** — file open/read/write/lock, buffer list management, backups.
- **`search.c`** — search, replace, regex, go-to-line, bracket matching.
- **`move.c`** — cursor movement and scrolling.
- **`cut.c`** — cut/copy/paste (cutbuffer), zap.
- **`prompt.c`** — statusbar prompt / answer-line editing (used by all interactive prompts).

A typical change spans several files: new option → `rcfile.c` (parse) + `nano.c` (flag) + `global.c`/`definitions.h` (bit or binding) + implementing file + `doc/`.

---

## Rust source architecture (`rust/nano/src/`)

A direct module-by-module transliteration of the C source. The mapping is 1:1:

| C file | Rust module |
|---|---|
| `definitions.h` | `definitions.rs` — all types, enums, consts (`LinePtr`, `OpenFileStruct`, `UndoStruct`, flag consts) |
| `global.c` + `prototypes.h` | `global.rs` — `AppState` struct, `STATE` thread-local, `with_state` / `with_state_mut`, `shortcut_init`, flag macros |
| `nano.c` | `nano.rs` + `main.rs` — `nano_main()`, main event loop, `process_a_keystroke`, `inject` |
| `winio.c` | `winio.rs` — crossterm replaces ncurses; `NanoWindow` replaces `WINDOW*` |
| `move.c` | `move_.rs` — renamed (Rust keyword) |
| all others | same name, `.rs` extension |

### Key Rust-specific design decisions

**Global state**: `NanoCell(UnsafeCell<AppState>)` in `global.rs` — allows re-entrant access matching C global semantics. `RefCell` was replaced because C functions freely call each other while holding "borrows" (e.g. `with_state_mut` closure calling `ensure_firstcolumn_is_aligned` which also reads state).

```rust
// Read state
with_state(|s| s.field)
// Mutate state
with_state_mut(|s| { s.field = val; })
// Also usable directly:
STATE.with(|s| s.borrow().field)
STATE.with(|s| { s.borrow_mut().field = val; })
```

**Linked lists**: `pub type LinePtr = Rc<RefCell<LineNode>>` with `Weak` back-pointers. The `LineNode` RefCells are standard (not `NanoCell`) since list nodes are never accessed re-entrantly.

**Flag macros**: defined in `global.rs` with `#[macro_export]`, usable anywhere as `ISSET!(FLAG)`, `SET!(FLAG)`, `UNSET!(FLAG)`, `TOGGLE!(FLAG)`.

**gettext**: `tr!("string")` macro (defined in `main.rs`) is a pass-through; `tr!("fmt", args...)` expands to `format!(...)`.

**ncurses → crossterm**: `NanoWindow { rows, cols, y, x }` replaces `WINDOW*`. Drawing uses `queue!(stdout(), MoveTo(...), Print(...))` etc. Key events come from `crossterm::event::read()` and are translated to nano integer keycodes in `winio::translate_key_event`.

**Local stubs in modules**: each `.rs` file may have a small block of private delegation functions at the top (e.g. `fn statusbar(msg: &str) { crate::winio::statusbar(msg) }`) to avoid fully-qualified paths throughout. These are not no-ops — they forward to the real implementation in the appropriate module.

---

## Other directories

- **`syntax/`** — `*.nanorc` syntax-highlighting definitions.
- **`doc/`** — `nano.1`, `nanorc.5`, `nano.texi`. Update when changing user-visible behavior.
- **`po/`** — gettext catalogs. User-visible strings use `_(...)` in C, `tr!(...)` in Rust.

## C conventions

- GNU style, tabs at 4 columns. View diffs with: `git config --local core.pager "less -x1,5"`
- Booleans: `TRUE`/`FALSE` from `definitions.h`.
- User-facing changes: record in `NEWS`, `ChangeLog`, `IMPROVEMENTS`.
- Commits: signed off (`git commit -as`); message body explains rationale.
