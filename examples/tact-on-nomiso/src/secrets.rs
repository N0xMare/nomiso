//! Best-effort secret rejection (Tact-style defense in depth).

/// Returns true if content looks secret-shaped and should be rejected.
pub fn looks_like_secret(content: &str) -> bool {
    let c = content.trim();
    if c.is_empty() {
        return false;
    }
    let lower = c.to_ascii_lowercase();

    // Private key blocks
    if lower.contains("-----begin") && lower.contains("private key") {
        return true;
    }
    // Auth headers / bearer
    if lower.contains("authorization:") || lower.contains("bearer ") {
        return true;
    }
    // Common token prefixes
    for p in [
        "sk-",
        "sk_live_",
        "sk_test_",
        "ghp_",
        "gho_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "AKIA",
    ] {
        if c.contains(p) {
            return true;
        }
    }
    // JWT-ish three base64 segments
    if looks_like_jwt(c) {
        return true;
    }
    // credential assignments
    for key in [
        "password=",
        "password:",
        "api_key=",
        "api_key:",
        "apikey=",
        "secret=",
        "secret:",
        "token=",
        "token:",
    ] {
        if lower.contains(key) {
            return true;
        }
    }
    // credential-bearing URLs
    if lower.contains("://")
        && (lower.contains("@") && (lower.contains("password") || lower.contains("token")))
    {
        return true;
    }
    false
}

fn looks_like_jwt(s: &str) -> bool {
    let parts: Vec<_> = s.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts.iter().all(|p| {
        !p.is_empty()
            && p.len() >= 8
            && p.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_sk_prefix() {
        assert!(looks_like_secret("sk-abc1234567890secret"));
    }

    #[test]
    fn allows_normal_fact() {
        assert!(!looks_like_secret(
            "Prefer TypeScript for agent tooling in this repo."
        ));
    }
}
