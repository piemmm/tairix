//! `cargo xtask keys init`: create the build's secret signing key set
//! (`tools/signkeys`), drawn from the host's entropy source, or keep the one
//! already there.

use std::ffi::OsString;
use std::io;

use tairix_mkimage::{EntropySource as _, HostEntropy};
use tairix_signkeys::KeyError;

use crate::Context;

/// Create the key set at its location. One already there is kept, never
/// replaced, once it loads: a runner that keeps its home between runs inits
/// every run.
///
/// # Errors
///
/// A usage error, why the key set could not be created, or why the one
/// already there is unusable.
pub fn run(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    if !matches!(args, [verb] if verb == "init") {
        return Err("keys: usage: cargo xtask keys init".to_string());
    }
    let mut entropy = |out: &mut [u8]| {
        HostEntropy
            .fill(out)
            .map_err(|_| io::Error::other("the host entropy source is unavailable"))
    };
    match tairix_signkeys::create(&ctx.workspace_root, &mut entropy) {
        Ok(path) => println!(
            "keys: created {}: every kernel built with it trusts only what it signs. Keep it \
             secret, and back it up to rebuild those kernels' programs.",
            path.display()
        ),
        Err(KeyError::Exists(path)) => {
            tairix_signkeys::load(&ctx.workspace_root).map_err(|err| format!("keys: {err}"))?;
            println!("keys: kept the signing key set at {}", path.display());
        }
        Err(err) => return Err(format!("keys: {err}")),
    }
    Ok(())
}
