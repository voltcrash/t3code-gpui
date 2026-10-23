//! Rotating desktop bootstrap token, matching `@t3tools/shared/desktopBootstrapToken`.
//!
//! The desktop hands the backend a secret; both sides derive the same token for
//! the current window, so a leaked token stops working within a day.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub const WINDOW_MS: u64 = 12 * 60 * 60 * 1000;

pub fn current_token(secret: &str, now_ms: u64) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(format!("t3-desktop-bootstrap:{}", now_ms / WINDOW_MS).as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_server_derivation() {
        // Produced by the TypeScript implementation for the same inputs.
        assert_eq!(
            current_token("test-secret", 1_800_000_000_000),
            "c0ef98cde95a5eec168b8a36b8cce561f9594a0777248fa1543959a20ce6f08c"
        );
    }
}
