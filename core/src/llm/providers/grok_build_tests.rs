use super::*;
use serde_json::json;

#[test]
fn finds_nested_grok_credentials() {
    let mut found = CredentialFields::default();
    find_credential_fields(
        &json!({
            "oidc": {
                "access_token": "access-token-with-enough-length",
                "refresh_token": "refresh",
                "expires_at": 2_000_000_000u64
            }
        }),
        &mut found,
    );
    assert_eq!(
        found.access_token.as_deref(),
        Some("access-token-with-enough-length")
    );
    assert_eq!(found.refresh_token.as_deref(), Some("refresh"));
    assert!(found.expires_at.is_some());
}

#[test]
fn parses_jwt_expiry() {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"exp":2000000000}"#);
    let token = format!("header.{payload}.signature");
    assert_eq!(jwt_expiry(&token), parse_expiry(&json!(2_000_000_000u64)));
}

#[test]
fn expiring_token_is_detected() {
    assert!(is_expiring(Some(SystemTime::now())));
    assert!(!is_expiring(Some(
        SystemTime::now() + Duration::from_secs(3600)
    )));
    assert!(!is_expiring(None));
}

#[test]
fn updates_nested_cli_credential_fields() {
    let mut doc = json!({
        "https://auth.x.ai::client": {
            "key": "old-access-token-with-enough-length",
            "refresh_token": "old-refresh",
            "expires_at": "2020-01-01T00:00:00Z",
            "auth_mode": "oidc",
            "email": "user@example.com"
        }
    });
    let creds = GrokCredentials {
        access_token: "new-access-token-with-enough-length".into(),
        refresh_token: Some("new-refresh".into()),
        expires_at: Some(UNIX_EPOCH + Duration::from_secs(2_000_000_000)),
        token_endpoint: Some("https://auth.x.ai/oauth/token".into()),
    };
    assert!(update_credential_fields(&mut doc, &creds));
    let entry = &doc["https://auth.x.ai::client"];
    assert_eq!(
        entry["key"].as_str(),
        Some("new-access-token-with-enough-length")
    );
    assert_eq!(entry["refresh_token"].as_str(), Some("new-refresh"));
    assert_eq!(entry["expires_at"].as_str(), Some("2033-05-18T03:33:20Z"));
    // Unrelated profile fields must survive the patch.
    assert_eq!(entry["email"].as_str(), Some("user@example.com"));
    assert_eq!(entry["auth_mode"].as_str(), Some("oidc"));
}

#[test]
fn format_unix_rfc3339_epoch() {
    assert_eq!(format_unix_rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(format_unix_rfc3339(2_000_000_000), "2033-05-18T03:33:20Z");
}

#[test]
fn parse_rfc3339_round_trips_format() {
    let secs = 2_000_000_000u64;
    let formatted = format_unix_rfc3339(secs);
    assert_eq!(
        parse_rfc3339_expiry(&formatted),
        Some(UNIX_EPOCH + Duration::from_secs(secs))
    );
    // Fractional seconds from Grok CLI are accepted and truncated.
    assert_eq!(
        parse_rfc3339_expiry("2033-05-18T03:33:20.111269811Z"),
        Some(UNIX_EPOCH + Duration::from_secs(secs))
    );
}

#[test]
fn persist_round_trips_through_temp_auth_file() {
    let dir = std::env::temp_dir().join(format!("bone-grok-auth-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("auth.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({
            "https://auth.x.ai::client": {
                "key": "old-access-token-with-enough-length",
                "refresh_token": "old-refresh",
                "expires_at": "2020-01-01T00:00:00Z"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let expires_at = UNIX_EPOCH + Duration::from_secs(1_900_000_000);
    let creds = GrokCredentials {
        access_token: "persisted-access-token-with-enough-length".into(),
        refresh_token: Some("persisted-refresh".into()),
        expires_at: Some(expires_at),
        token_endpoint: None,
    };
    persist_grok_credentials_at(&path, &creds).expect("persist");
    let loaded = load_grok_credentials_from(&path).expect("reload");
    assert_eq!(
        loaded.access_token,
        "persisted-access-token-with-enough-length"
    );
    assert_eq!(loaded.refresh_token.as_deref(), Some("persisted-refresh"));
    assert_eq!(loaded.expires_at, Some(expires_at));
    let _ = std::fs::remove_dir_all(&dir);
}
