//! `po-link` — the link between a phone and the user's PCs, with no server of ours in between.
//!
//! - [`identity`]: each device's keys; the public key is the identity.
//! - [`directory`]: the account's secret gist, where devices find each other (signed entries,
//!   addresses encrypted under a key sealed to each device).
//! - [`net`], [`mdns`], [`portmap`]: the addresses (local network, IPv6, the router's), raced in
//!   parallel, and the punching through firewalls.
//! - [`tls`], [`link`]: QUIC with the keys pinned on both sides, and the conversation over it.
//! - [`pc`], [`phone`]: each side put together.
//!
//! Shared by Po's service (Windows, Linux, macOS) and the mobile app (Android, iOS): nothing here
//! is specific to one of them.

pub mod directory;
pub mod github;
pub mod identity;
pub mod link;
pub mod mdns;
pub mod net;
pub mod pc;
pub mod phone;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod portmap;
pub mod seal;
pub mod tls;

#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use identity::Identity;
pub use link::{Backend, Link, LinkError};
pub use po_wire as wire;

/// Writes a file only its owner reads (keys, tokens).
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests;
