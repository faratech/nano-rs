# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

GNU nano — a small terminal text editor (ncurses-based, C99, GNU Autotools build).
It is an official GNU package; the canonical upstream is the Savannah git repo, and
contributions are submitted as patches to the `nano-devel@gnu.org` mailing list (see
README.hacking), not via GitHub PRs.

## Building

This is a from-git checkout (no generated `configure` yet). Build with:

```sh
./autogen.sh          # regenerate configure + Makefiles (needs autoconf/automake/autopoint/gettext)
./configure           # add --sysconfdir=/etc to read /etc/nanorc instead of the prefix's etc
make                  # build src/nano
make install          # installs nano, rnano, man/info pages, and syntax/ files (root for /usr/local)
```

Requires ncurses headers (`libncurses-dev` / `ncurses-devel`). A bundled gnulib tree
lives in `lib/` and is compiled into `libgnu.a`, linked by the `nano` binary.

The build environment here is Windows, but nano targets POSIX/ncurses. Use the Bash
tool for the Autotools build; it will not build natively under PowerShell.

### Build-time feature flags

nano is heavily `#ifdef`-gated. Key configure switches: `--enable-tiny` (defines
`NANO_TINY`, strips most optional code), `--enable-debug`, `--disable-wrapping`,
`--disable-justify`, `--disable-extra`, `--disable-utf8`, `--disable-multibuffer`,
`--disable-nanorc`, `--disable-color`, `--with-slang`. When editing, code guarded by
`#ifndef NANO_TINY` (and feature macros like `ENABLE_COLOR`, `ENABLE_NANORC`,
`ENABLE_MULTIBUFFER`, `ENABLE_UTF8`) must still compile cleanly when that feature is
disabled.

## Testing

There is no unit-test suite. The regression check is a **build matrix**, not behavioral
tests: `./nano-regress` (a Perl script) configures and `make clean all` across every
combination of the feature flags above, failing on the first combo that doesn't compile.
Run it after touching anything inside `#ifdef`/`#ifndef` blocks to catch flag breakage.
Behavioral verification is manual: run `src/nano` against a scratch file.

## Source architecture (`src/`)

There is no central event dispatcher object; nano is a set of files sharing global state
declared in `prototypes.h` and defined in `global.c`. `definitions.h` holds all structs,
enums, macros, and the feature `#ifdef` scaffolding — read it first to understand the
data model (`linestruct` for buffer lines, `openfilestruct` for buffers, `keystruct`/
`funcstruct` for the bind system, the `colortype`/`syntaxtype` chain for highlighting).

- **`nano.c`** — `main()`, startup/option parsing, the main input loop (`do_input`),
  signal handling, terminal setup/teardown.
- **`global.c`** — the heart of the keybinding system. `shortcut_init()` builds the
  global linked lists of **functions** (`funcstruct`/`allfuncs`) and **bindings**
  (`keystruct`/`sclist`) via the `add_to_funcs` / `add_to_sclist` helpers. To add a new
  editor command you wire it up here, then implement it in the relevant file.
- **`winio.c`** — all terminal I/O: reading/decoding keystrokes (including escape
  sequences, mouse, UTF-8) and painting every part of the screen (edit window, title
  bar, status bar, prompt bar). Largest and most intricate file.
- **`rcfile.c`** — parses nanorc config files and the `bind`/`set`/`color`/`syntax`
  directives. The set of options and bindable function names here must stay in sync with
  `global.c` and the docs in `doc/`.
- **`text.c`** — text modification: insert/delete, undo/redo (`undostruct`), wrapping,
  justify, spell/lint/formatter tool invocation, indentation, comment toggling.
- **`files.c`** — opening, reading, writing, locking files; buffer (multi-file) list
  management; backups; the insert-file prompt.
- **`search.c`** — search, replace, regex, go-to-line, bracket matching.
- **`move.c`** — cursor movement and scrolling.
- **`cut.c`** — cut/copy/paste (the cutbuffer) and zapping.
- **`browser.c`** — the built-in file browser.
- **`prompt.c`** — the statusbar prompt / answer-line editing used by all interactive
  prompts.
- **`history.c`** — search/replace/position history and anchors (the `.nano/` state files).
- **`chars.c`** — character classification and UTF-8/multibyte handling.
- **`color.c`** — applies syntax-highlighting regexes to lines.
- **`help.c`** — the built-in help viewer; help text is generated from the binding list.
- **`utils.c`** — generic helpers (string, memory, number parsing).

A typical change spans several of these: e.g. a new option means touching `rcfile.c`
(parse it), `nano.c` (command-line flag), `global.c`/`definitions.h` (a flag bit or
binding), the implementing `.c` file, plus docs.

## Other directories

- **`syntax/`** — `*.nanorc` syntax-highlighting definitions, installed when color is
  enabled. Add new language highlighting here.
- **`doc/`** — `nano.1`, `nanorc.5`, `rnano.1` man pages and `nano.texi` info manual.
  User-facing behavior changes (new option, new binding, new default) require updating
  the man pages **and** `nano.texi`, and often `doc/sample.nanorc.in`.
- **`po/`** — gettext translation catalogs. User-visible strings go through `_()`.
- **`lib/`** — bundled gnulib portability modules (generally not edited by hand).
- **`m4/`** — autoconf macros.

## Conventions

- C with GNU style. Indentation is **tabs sized to 4 columns**; lines kept within ~80
  columns at that tab size. To view diffs correctly: `git config --local core.pager "less -x1,5"`.
- Booleans use the `TRUE`/`FALSE` and `bool` defined in `definitions.h`.
- Wrap user-visible strings in `_(...)` for translation.
- Record notable user-facing changes in `NEWS`, `ChangeLog`, and `IMPROVEMENTS`.
- Commits are signed off (`git commit -as`); the message body should explain the rationale.
