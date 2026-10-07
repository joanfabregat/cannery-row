#![forbid(unsafe_code)]

use cannery_core::{
    errors::ErrorCode,
    principal::{Channel, Principal, Scope, Secret, UserPrincipal, Via},
};
use std::collections::BTreeSet;

#[test]
fn relabeling_a_principal_preserves_actor_scope_and_session_secrecy()
-> Result<(), Box<dyn std::error::Error>> {
    let user_id = "f470f74a-c11a-4b56-a9d8-1239d6eab445".parse()?;
    let session_id = "b5d93618-3347-43a8-8f15-3a932a765b4f".parse()?;
    let principal = Principal::User(UserPrincipal {
        user_id,
        email: Some("admin@example.test".to_owned()),
        display_name: None,
        is_admin: true,
        via: Via {
            channel: Channel::Api,
            client: Some("token:reader".to_owned()),
        },
        scopes: BTreeSet::from([Scope::Read]),
        session_id: Some(session_id),
        csrf_token: Some(Secret::new("synthetic-private-csrf-value".to_owned())),
    })
    .with_channel(Channel::Mcp, Some("test-client".to_owned()));
    let user = principal.require_admin(false)?;
    assert_eq!(user.user_id, user_id);
    assert_eq!(user.session_id, Some(session_id));
    assert_eq!(principal.via().channel, Channel::Mcp);
    assert_eq!(principal.via().client.as_deref(), Some("test-client"));
    assert_eq!(principal.scopes(), &BTreeSet::from([Scope::Read]));
    let error = principal
        .require_admin(true)
        .err()
        .ok_or("readonly admin gained write authority")?;
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert!(!format!("{principal:?}").contains("synthetic-private-csrf-value"));
    Ok(())
}
