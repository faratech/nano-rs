# Contributing to nano-rs

Thank you for your interest in contributing to this Rust port of GNU nano!

**⚠️ Important**: This is an **independent, unofficial educational project**, not affiliated with the official GNU nano project. If you're looking to contribute to the official nano, visit https://www.nano-editor.org/

## Code of Conduct

This project follows the Rust community's [Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct). Please ensure all interactions are respectful and constructive.

## Getting Started

### Prerequisites

- Rust 1.70 or later
- Familiarity with the C nano codebase (helpful but not required)
- Git for version control

### Setting Up Development Environment

```bash
# Clone the repository
git clone https://github.com/yourusername/nano-rs.git
cd nano-rs/rust

# Run tests to verify setup
cargo test

# Run clippy to check for common mistakes
cargo clippy

# Format code
cargo fmt
```

## Development Workflow

1. **Create a feature branch** from `rust-port`:
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make your changes**:
   - Keep commits focused and atomic
   - Write descriptive commit messages
   - Reference issues when applicable

3. **Test your changes**:
   ```bash
   # Run full test suite
   cargo test
   
   # Check code quality
   cargo clippy
   
   # Format code
   cargo fmt
   
   # Type check only
   cargo check
   ```

4. **Test on multiple platforms** (if possible):
   ```bash
   # Linux
   cargo test --release
   
   # Windows (via WSL or native)
   cargo build --target x86_64-pc-windows-msvc
   ```

5. **Push and create a Pull Request**

## Coding Guidelines

### Rust Style

- Follow [Rust naming conventions](https://doc.rust-lang.org/1.0.0/style/naming/README.html)
- Use `cargo fmt` for formatting (enforced via CI)
- Use `cargo clippy` and fix all warnings
- Write self-documenting code with minimal comments

### Comments

Only comment the "why", not the "what":

```rust
// ✅ Good: explains intent
// Skip validation if running in test mode to allow rapid iteration
if !cfg!(test) {
    validate_input()?;
}

// ❌ Bad: obvious from code
// Check if running in test mode
if !cfg!(test) {
    validate_input()?;
}
```

### Module Organization

Each module should mirror the C source structure:
- `definitions.rs` - Types and constants
- `global.rs` - Global state
- `nano.rs` - Main event loop
- Feature-specific modules (`files.rs`, `search.rs`, etc.)

### Platform-Specific Code

Use `#[cfg(unix)]` and `#[cfg(not(unix))]` for platform differences:

```rust
#[cfg(unix)]
{
    // Unix-only code
    unsafe { libc::signal(SIGINT, handler) }
}

#[cfg(not(unix))]
{
    // Windows fallback
    // ...
}
```

## Commit Message Guidelines

Write clear, descriptive commit messages:

```
type: short summary (under 70 characters)

Longer explanation of what changed and why. Wrap at 72 characters.
Include any relevant context or issue references.

If this fixes an issue, reference it:
Fixes #123

Co-Authored-By: Your Name <your.email@example.com>
```

Commit types:
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting, naming)
- `refactor`: Code reorganization without behavior change
- `perf`: Performance improvement
- `test`: Test additions or modifications
- `chore`: Build system, dependencies, tooling

## Testing

### Unit Tests

Add unit tests for new functions:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_my_function() {
        let result = my_function(5);
        assert_eq!(result, 10);
    }
}
```

### Integration Tests

For features that require multiple modules working together.

### Manual Testing

Test the UI and behavior:
```bash
cargo build --release
./target/release/nano test_file.txt
```

Test with various flags:
```bash
./target/release/nano --help
./target/release/nano -S
./target/release/nano --binary
```

## Feature Development

### Adding a New Feature

1. **Review the C implementation** in `../src/` to understand the logic
2. **Create a feature flag** in `Cargo.toml` if appropriate
3. **Implement in Rust**, preserving the C behavior
4. **Add tests** for the new functionality
5. **Update documentation** if needed
6. **Test on multiple platforms**

### Feature Flags

Keep feature flags aligned with the C build system. Test both with and without features:

```bash
# Test with all features
cargo test --all-features

# Test minimal
cargo test --no-default-features --features=tiny

# Test specific feature combination
cargo test --features=color,nanorc
```

## Bug Fixes

1. **Write a test** that reproduces the bug
2. **Fix the bug** - minimal change
3. **Verify the test passes** - commit with fix
4. **Don't refactor** while fixing bugs

## Documentation

- Update `README.md` for user-facing changes
- Update `CONTRIBUTING.md` (this file) for development changes
- Add doc comments for public APIs
- Keep C code comments in Rust where applicable

## Performance

- Profile before optimizing
- Don't sacrifice clarity for micro-optimizations
- Document performance-critical sections
- Compare against C nano for feature parity

## Platform Support

All changes must:
- ✅ Compile on Linux (aarch64 and x86_64)
- ✅ Compile on Windows MSVC (x86_64 and aarch64)
- ✅ Pass tests on multiple platforms
- ✅ Properly gate platform-specific code

## CI/CD

Submissions will be checked by:
- `cargo fmt --check` - Code formatting
- `cargo clippy` - Linting
- `cargo test` - Unit and integration tests
- Cross-platform compilation

All checks must pass before merging.

## Reporting Issues

- Check existing issues first
- Provide minimal reproducible example
- Include platform (Linux/Windows/macOS)
- Include output of `cargo --version` and `rustc --version`

## Getting Help

- Check the README for architectural overview
- Review the C source for reference implementation
- Ask in pull request comments for guidance
- Reach out to maintainers for major changes

## License

By contributing, you agree that your contributions will be licensed under the GPL-3.0+ license (same as nano).

Thank you for contributing to nano-rs! 🦀
