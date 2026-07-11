//! Build script for nano-rs
//! Embeds Windows version metadata (media/nano.rc) into the executable.
//!
//! Embedding needs a Windows resource compiler (rc.exe). That is present when
//! building ON a Windows host (and on the `windows-latest` release runners),
//! but NOT during a plain `cargo check`/cross-build from a toolchain-less host
//! — where cc-rs aborts. The metadata is cosmetic, so we only attempt the
//! embed on a Windows host and skip everywhere else.

fn main() {
    println!("cargo:rerun-if-changed=media/nano.rc");

    // CARGO_CFG_TARGET_OS is the *crate* target; cfg!(windows) here is the
    // *host* running this build script (where rc.exe would live).
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    if cfg!(windows) {
        let _ = embed_resource::compile("media/nano.rc", embed_resource::NONE);
    }
}
