# nano-rs Crossterm patches

This directory is based on the exact `crossterm` 0.29.0 crate published on
crates.io. The source archive used for vendoring has SHA-256
`d8b9f2e4c67f833b660cdb0a3523065869fb35570177239812ed4c905aeff87b` and
records upstream Git commit `36d95b26a26e64b0f8c12edfe11f410a6d56a812`.
The upstream MIT license remains in `LICENSE`.

nano-rs carries three local fixes:

1. Unix bracketed-paste parsing emits `Event::PasteBytes(Vec<u8>)`, preserving
   the terminal payload exactly instead of passing it through
   `String::from_utf8_lossy`. `Event::Paste(String)` is retained for source and
   serialization compatibility with code that constructs that variant.
2. Both Unix event sources report a zero-byte terminal read as `UnexpectedEof`,
   retry `Interrupted`, stop normally on `WouldBlock`, and return every other
   read error. The `use-dev-tty` poll backend also handles terminal hangup and
   poll-error flags explicitly. This prevents a closed PTY from becoming a busy
   loop.
3. Unix event parsers keep incomplete ESC/CSI sequences across arbitrary short
   PTY reads. Ambiguous sequences use a 100 ms idle timeout so a standalone
   Escape key remains responsive, while a bracketed paste in progress has no
   ambiguity timeout and can safely span large or delayed input streams.

The parser and event-source regression tests live beside the patched code.
