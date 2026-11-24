//! Embeds the originating commit in the binary: `po --version` then says exactly which
//! version is running, which makes it possible to check that an installation is up to date.

use std::process::Command;

fn main() {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    let date = git(&["log", "-1", "--format=%cs"]).unwrap_or_default();
    println!("cargo:rustc-env=PO_COMMIT={commit}{}", if dirty { "+dirty" } else { "" });
    println!("cargo:rustc-env=PO_COMMIT_DATE={date}");
    // The platform, to pick this binary in a release; `PO_RELEASE` marks the release workflow's
    // builds, the only ones that replace themselves (see `update.rs`).
    println!("cargo:rustc-env=PO_TARGET={}", std::env::var("TARGET").unwrap_or_default());
    println!("cargo:rerun-if-env-changed=PO_RELEASE");
    // Re-run on every commit or branch switch, not on every compilation.
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/index");
        if let Some(head) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={dir}/{head}");
        }
    }
}
