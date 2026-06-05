# nano-rs: Rust Port of GNU nano

A faithful Rust transliteration of the [GNU nano](https://www.nano-editor.org/) text editor - the canonical C implementation lives in the parent directory.

## Overview

This is a **1:1 module-by-module port** of nano from C to Rust, preserving the original architecture and behavior while leveraging Rust's type safety and memory guarantees.

### Why Rust?

- **Memory safety**: Eliminates entire classes of bugs (buffer overflows, use-after-free)
- **Fearless concurrency**: Rust's ownership system prevents data races at compile time
- **Same performance**: Direct translation maintains nano's efficient design
- **Cross-platform**: Single codebase compiles on Linux, macOS, Windows, and more

## Supported Platforms

- ✅ Linux (aarch64, x86_64)
- ✅ Windows MSVC (x86_64, aarch64)
- ✅ macOS (via standard Rust toolchain)

## Building

### Prerequisites

- Rust 1.70+ (includes Cargo)
- For MSVC targets on non-Windows: xwin and lld-link

### Quick Start

```bash
cd rust
cargo build --release
```

The binary will be at `target/release/nano` (or `target/release/nano.exe` on Windows).

### Build Options

#### Feature Flags

Control which nano features to compile:

```bash
# Minimal build (tiny mode)
cargo build --no-default-features --features=tiny

# Full-featured build
cargo build --all-features

# Custom features
cargo build --features=color,nanorc,utf8
```

Available features:
- `tiny` - Minimal feature set
- `color` - Syntax highlighting
- `nanorc` - Configuration file support
- `utf8` - Unicode support
- `browser` - File browser
- `help` - Help system
- `histories` - Search/replace history
- `justify` - Paragraph justification
- `linter` - Lint integration
- `formatter` - Code formatting
- `speller` - Spell checker
- `mouse` - Mouse support
- And more...

#### Cross-Compilation to Windows

```bash
# Download Windows SDK (one-time setup)
echo "yes" | xwin splat --output /opt/xwin

# Build for Windows x86_64
cargo build --target x86_64-pc-windows-msvc --release

# Build for Windows ARM64
cargo build --target aarch64-pc-windows-msvc --release
```

### Cargo Configuration

A `.cargo/config.toml` is included for MSVC cross-compilation setup.

## Architecture

### Module Mapping

The Rust code mirrors the C source structure:

| C File | Rust Module | Purpose |
|--------|------------|---------|
| `definitions.h` | `definitions.rs` | Type definitions, enums, constants |
| `global.c` | `global.rs` | Global state, keybindings |
| `nano.c` | `nano.rs` | Main loop, event dispatch |
| `winio.c` | `winio.rs` | Terminal I/O (crossterm) |
| `move.c` | `move_.rs` | Cursor movement |
| `files.c` | `files.rs` | File I/O, locking |
| `search.c` | `search.rs` | Search/replace |
| `text.c` | `text.rs` | Text manipulation |
| `cut.c` | `cut.rs` | Cut/copy/paste |
| ... | ... | ... |

### Key Design Decisions

**Global State**: Uses `NanoCell(UnsafeCell<AppState>)` in `global.rs` for re-entrant access matching C semantics, avoiding the limitations of `RefCell`.

**Linked Lists**: `type LinePtr = Rc<RefCell<LineNode>>` with `Weak` back-pointers for the document tree.

**Flag Macros**: `ISSET!(FLAG)`, `SET!(FLAG)`, `UNSET!(FLAG)`, `TOGGLE!(FLAG)` macros defined in `global.rs`.

**Terminal Handling**: `crossterm` crate replaces ncurses for cross-platform terminal control.

**Localization**: `tr!("string")` macro for gettext-style i18n.

## Development

### Running Tests

```bash
cargo test
```

### Type Checking Only

```bash
cargo check
```

### Linting

```bash
cargo clippy
```

### Formatting

```bash
cargo fmt
```

## Differences from C Nano

### Intentional Changes

1. **Platform-specific code is properly gated**: Unix-only features (signals, termios) are guarded with `#[cfg(unix)]`
2. **Modern file APIs**: Uses Rust's `std::fs` instead of libc where possible
3. **Structured error handling**: Results and Options instead of sentinel values
4. **No global mutable state**: State accessed through `with_state()`/`with_state_mut()` helpers

### Preserved Behavior

- Command syntax and keybindings
- File handling and locking
- Search/replace regex patterns
- Undo/redo system
- Syntax highlighting rules
- Configuration file format

## License

GNU General Public License v3 or later (GPL-3.0+). See `../COPYING` and `../COPYING.DOC`.

## Contributing

Contributions are welcome! Please ensure:

- Code follows Rust idioms and conventions
- Changes preserve nano's functionality and behavior
- All tests pass: `cargo test`
- Code is formatted: `cargo fmt`
- No clippy warnings: `cargo clippy`

## See Also

- [GNU nano](https://www.nano-editor.org/) - The original C implementation
- [nano on GitHub](https://github.com/torvalds/linux) - Development repository
- The C source in the parent directory for reference implementation details

## Status

**Stable**: The Rust port is feature-complete and suitable for general use. All core functionality works as expected.

**Platform Support**:
- Linux: ✅ Fully supported
- Windows: ✅ Fully supported (MSVC only)
- macOS: ✅ Fully supported

**Known Issues**:
- On Windows, history file initialization may require manual directory creation (`%USERPROFILE%\.nano\`)

## Authors

**Rust Port**: Claude (Anthropic)  
**Original nano**: Chris Allegretta and contributors
