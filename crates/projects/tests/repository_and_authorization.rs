//! Native PostgreSQL equivalents of project/access and cursor privacy intents.
//! Run only against an isolated database initialized with the shipped migrations.
#![forbid(unsafe_code)]

use cannery_core::{
    db::DatabaseOptions,
    errors::ErrorCode,
    ids::{ServiceAccountId, UserId},
    principal::{
        Channel, Principal, Role, Scope, ServicePrincipal, UserPrincipal, Via, all_scopes,
    },
};
use cannery_projects::{
    ProjectError,
    authz::{self, ALL_SERVICES},
    repo::{self, Project},
};
use sqlx::{Connection, PgConnection};
use std::{collections::BTreeSet, str::FromStr};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

async fn connect() -> Result<PgConnection> {
    let url = std::env::var("CANNERY_PROJECTS_TEST_DATABASE_URL")?;
    let options = DatabaseOptions::parse(&url)?;
    if !options
        .connect_options()
        .get_database()
        .is_some_and(|name| {
            name.strip_prefix("conformance_").is_some_and(|suffix| {
                suffix.len() == 24 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        })
    {
        return Err("project tests require a task-owned disposable database".into());
    }
    Ok(options.connect(None).await?)
}

async fn user(conn: &mut PgConnection, email: Option<&str>, name: Option<&str>) -> Result<UserId> {
    let id: String = sqlx::query_scalar("INSERT INTO users (issuer, subject, email, display_name)
        VALUES ('https://projects-fixture.invalid', gen_random_uuid()::text, $1, $2) RETURNING id::text")
        .bind(email).bind(name).fetch_one(conn).await?;
    Ok(UserId::from_str(&id)?)
}

async fn project(conn: &mut PgConnection, creator: UserId, suffix: &str) -> Result<Project> {
    let nonce: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(&mut *conn)
        .await?;
    let slug = format!("fixture-{nonce}-{suffix}");
    repo::create_project(conn, &slug, "Fixture", "Description", creator)
        .await?
        .ok_or_else(|| "fresh project unexpectedly conflicted".into())
}

fn human(id: UserId, admin: bool, write: bool) -> Principal {
    Principal::User(UserPrincipal {
        user_id: id,
        email: None,
        display_name: None,
        is_admin: admin,
        via: Via {
            channel: Channel::Api,
            client: None,
        },
        scopes: if write {
            all_scopes()
        } else {
            BTreeSet::from([Scope::Read])
        },
        session_id: None,
        csrf_token: None,
    })
}

fn service(
    project: &Project,
    kind: cannery_core::principal::ServiceKind,
    write: bool,
) -> Result<Principal> {
    Ok(Principal::Service(ServicePrincipal {
        service_account_id: ServiceAccountId::from_str("00000000-0000-0000-0000-000000000001")?,
        project_id: project.id,
        kind,
        name: "Fixture service".into(),
        via: Via {
            channel: Channel::Api,
            client: None,
        },
        scopes: if write {
            all_scopes()
        } else {
            BTreeSet::from([Scope::Read])
        },
    }))
}

fn denied<T>(
    result: std::result::Result<T, ProjectError>,
    code: ErrorCode,
    message: &str,
) -> Result<()> {
    match result {
        Err(ProjectError::Domain(error)) => {
            assert_eq!(error.code, code);
            assert_eq!(error.message, message);
            assert_eq!(error.details, serde_json::Value::Null);
            Ok(())
        }
        _ => Err("expected a project authorization refusal".into()),
    }
}

#[tokio::test]
#[ignore = "requires an isolated migrated PostgreSQL database"]
async fn project_creation_membership_prior_roles_and_transaction_ownership() -> Result<()> {
    let mut conn = connect().await?;
    let mut tx = conn.begin().await?;
    let creator = user(&mut tx, Some("creator@fixture.invalid"), None).await?;
    let member = user(&mut tx, None, Some("Member")).await?;
    let other_grantor = user(&mut tx, None, None).await?;
    let first = project(&mut tx, creator, "create").await?;
    assert_eq!(first.created_by, creator);
    assert!(first.created_at.to_string().ends_with("+00:00"));
    assert_eq!(
        repo::get_project(&mut tx, first.id).await?,
        Some(first.clone())
    );
    assert_eq!(
        repo::get_project_by_slug(&mut tx, &first.slug).await?,
        Some(first.clone())
    );
    assert!(
        repo::create_project(
            &mut tx,
            &first.slug,
            "Replacement",
            "Replacement",
            other_grantor
        )
        .await?
        .is_none()
    );
    assert_eq!(
        repo::get_project(&mut tx, first.id).await?,
        Some(first.clone())
    );
    assert!(repo::lock_project(&mut tx, first.id).await?);
    assert_eq!(
        repo::set_membership(&mut tx, first.id, member, Role::Viewer, creator).await?,
        None
    );
    assert_eq!(
        repo::set_membership(&mut tx, first.id, member, Role::Researcher, creator).await?,
        Some(Role::Viewer)
    );
    assert_eq!(
        repo::set_membership(&mut tx, first.id, member, Role::Researcher, other_grantor).await?,
        Some(Role::Researcher)
    );
    let row = repo::get_member(&mut tx, first.id, member)
        .await?
        .ok_or("member missing")?;
    assert_eq!(row.role, Role::Researcher);
    assert_eq!(row.granted_by, other_grantor);
    assert_eq!(row.granted_at, first.created_at); // PostgreSQL now() is transaction-stable.
    assert_eq!(
        repo::get_role(&mut tx, first.id, member).await?,
        Some(Role::Researcher)
    );
    assert_eq!(
        repo::delete_membership(&mut tx, first.id, member).await?,
        Some(Role::Researcher)
    );
    assert_eq!(
        repo::delete_membership(&mut tx, first.id, member).await?,
        None
    );
    assert!(repo::get_member(&mut tx, first.id, member).await?.is_none());
    tx.rollback().await?;
    assert!(repo::get_project(&mut conn, first.id).await?.is_none());
    assert!(!repo::lock_project(&mut conn, first.id).await?);
    repo::share_lock_project(&mut conn, first.id).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated migrated PostgreSQL database"]
async fn project_listing_and_member_cursor_privacy() -> Result<()> {
    let mut conn = connect().await?;
    let mut tx = conn.begin().await?;
    let creator = user(&mut tx, None, Some("Z Creator")).await?;
    let empty = user(&mut tx, None, None).await?;
    let fallback = user(&mut tx, Some("Alpha@fixture.invalid"), None).await?;
    let tie_a = user(&mut tx, Some("z@fixture.invalid"), Some("beta")).await?;
    let tie_b = user(&mut tx, Some("a@fixture.invalid"), Some("BETA")).await?;
    let foreign = user(&mut tx, Some("a@fixture.invalid"), Some("A foreign cursor")).await?;
    let first = project(&mut tx, creator, "first").await?;
    let second = project(&mut tx, creator, "second").await?;
    for member in [empty, fallback, tie_a, tie_b] {
        repo::set_membership(&mut tx, first.id, member, Role::Viewer, creator).await?;
    }
    repo::set_membership(&mut tx, second.id, foreign, Role::Member, creator).await?;
    repo::set_membership(&mut tx, second.id, tie_a, Role::Researcher, creator).await?;
    let rows = repo::list_members(&mut tx, first.id, None, None).await?;
    let mut tied = [tie_a, tie_b];
    tied.sort();
    assert_eq!(
        rows.iter().map(|m| m.user_id).collect::<Vec<_>>(),
        [empty, fallback, tied[0], tied[1]]
    );
    assert_eq!(
        repo::list_members(&mut tx, first.id, None, Some(2)).await?,
        rows[..2]
    );
    assert_eq!(
        repo::list_members(&mut tx, first.id, Some(fallback), None).await?,
        rows[2..]
    );
    assert_eq!(
        repo::list_members(&mut tx, first.id, None, Some(0)).await?,
        [] as [repo::Member; 0]
    );
    assert_eq!(
        repo::list_members(&mut tx, first.id, Some(foreign), None).await?,
        [] as [repo::Member; 0]
    );
    assert!(
        repo::get_member(&mut tx, first.id, foreign)
            .await?
            .is_none()
    );
    repo::delete_membership(&mut tx, first.id, fallback).await?;
    assert_eq!(
        repo::list_members(&mut tx, first.id, Some(fallback), None).await?,
        [] as [repo::Member; 0]
    );
    let projects = repo::list_projects_for_user(&mut tx, tie_a, false, None, None).await?;
    assert_eq!(projects.len(), 2);
    assert!(projects[0].slug < projects[1].slug);
    for entry in &projects {
        assert_eq!(
            entry.role,
            if entry.id == first.id {
                Some(Role::Viewer)
            } else {
                Some(Role::Researcher)
            }
        );
    }
    let page =
        repo::list_projects_for_user(&mut tx, tie_a, false, Some(&projects[0].slug), Some(1))
            .await?;
    assert_eq!(page, projects[1..]);
    assert_eq!(
        repo::list_projects_for_user(&mut tx, tie_a, false, None, Some(0)).await?,
        [] as [repo::ProjectWithRole; 0]
    );
    let visible_foreign = repo::list_projects_for_user(&mut tx, foreign, false, None, None).await?;
    assert_eq!(visible_foreign.len(), 1);
    assert_eq!(visible_foreign[0].id, second.id);
    let all = repo::list_projects_for_user(&mut tx, foreign, true, None, None).await?;
    assert!(all.iter().any(|p| p.id == first.id && p.role.is_none()));
    assert!(
        all.iter()
            .any(|p| p.id == second.id && p.role == Some(Role::Member))
    );
    tx.rollback().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated migrated PostgreSQL database"]
async fn authorization_hides_projects_and_checks_role_kind_before_scope() -> Result<()> {
    let mut conn = connect().await?;
    let mut tx = conn.begin().await?;
    let creator = user(&mut tx, None, None).await?;
    let member = user(&mut tx, None, None).await?;
    let first = project(&mut tx, creator, "access").await?;
    let second = project(&mut tx, creator, "foreign").await?;
    let admin = human(creator, true, false);
    assert_eq!(
        authz::project_read(&mut tx, &admin, &first.slug).await?,
        first
    );
    denied(
        authz::project_access(
            &mut tx,
            &admin,
            &first.slug,
            Some(Role::Viewer),
            &ALL_SERVICES,
            true,
        )
        .await,
        ErrorCode::NotFound,
        "project not found",
    )?;
    denied(
        authz::project_read(&mut tx, &admin, "nonexistent-project").await,
        ErrorCode::NotFound,
        "project not found",
    )?;
    let read_only = human(member, false, false);
    denied(
        authz::project_read(&mut tx, &read_only, &first.slug).await,
        ErrorCode::NotFound,
        "project not found",
    )?;
    check_human_authorization(&mut tx, &first, &second, creator, member).await?;
    check_service_authorization(&mut tx, &first, &second).await?;
    tx.rollback().await?;
    Ok(())
}

async fn check_human_authorization(
    tx: &mut PgConnection,
    first: &Project,
    second: &Project,
    creator: UserId,
    member: UserId,
) -> Result<()> {
    let read_only = human(member, false, false);
    for role in [Role::Viewer, Role::Member, Role::Researcher] {
        repo::set_membership(tx, first.id, member, role, creator).await?;
        if role != Role::Researcher {
            let admin_with_membership = human(member, true, true);
            for actor in [&read_only, &admin_with_membership] {
                denied(
                    authz::project_access(
                        tx,
                        actor,
                        &first.slug,
                        Some(Role::Researcher),
                        &[],
                        true,
                    )
                    .await,
                    ErrorCode::Forbidden,
                    "this requires the researcher role",
                )?;
            }
        }
        assert_eq!(
            &authz::project_read(tx, &read_only, &first.slug).await?,
            first
        );
        for minimum in [Role::Viewer, Role::Member, Role::Researcher] {
            let result =
                authz::project_access(tx, &read_only, &first.slug, Some(minimum), &[], false).await;
            if role >= minimum {
                assert_eq!(result?.role, Some(role));
            } else {
                denied(
                    result,
                    ErrorCode::Forbidden,
                    if minimum == Role::Member {
                        "this requires the member role"
                    } else {
                        "this requires the researcher role"
                    },
                )?;
            }
        }
        denied(
            authz::project_access(tx, &read_only, &first.slug, None, &ALL_SERVICES, true).await,
            ErrorCode::Forbidden,
            "this requires a service account",
        )?;
    }
    denied(
        authz::project_access(
            tx,
            &read_only,
            &first.slug,
            Some(Role::Researcher),
            &[],
            true,
        )
        .await,
        ErrorCode::Forbidden,
        "this token lacks the 'write' scope",
    )?;
    assert_eq!(
        authz::project_access(
            tx,
            &human(member, false, true),
            &first.slug,
            Some(Role::Researcher),
            &[],
            true
        )
        .await?
        .role,
        Some(Role::Researcher)
    );
    denied(
        authz::project_access(
            tx,
            &read_only,
            &second.slug,
            Some(Role::Viewer),
            &ALL_SERVICES,
            true,
        )
        .await,
        ErrorCode::NotFound,
        "project not found",
    )?;
    Ok(())
}

async fn check_service_authorization(
    tx: &mut PgConnection,
    first: &Project,
    second: &Project,
) -> Result<()> {
    for kind in ALL_SERVICES {
        let scoped = service(first, kind, true)?;
        assert_eq!(&authz::project_read(tx, &scoped, &first.slug).await?, first);
        let access = authz::project_access(tx, &scoped, &first.slug, None, &[kind], true).await?;
        assert_eq!(access.service_kind, Some(kind));
        assert_eq!(access.role, None);
        denied(
            authz::project_access(tx, &scoped, &second.slug, None, &ALL_SERVICES, true).await,
            ErrorCode::NotFound,
            "project not found",
        )?;
        let unscoped = service(first, kind, false)?;
        denied(
            authz::project_access(tx, &unscoped, &second.slug, None, &[], true).await,
            ErrorCode::NotFound,
            "project not found",
        )?;
        denied(
            authz::project_access(tx, &unscoped, &first.slug, None, &[kind], true).await,
            ErrorCode::Forbidden,
            "this token lacks the 'write' scope",
        )?;
        for allowed in ALL_SERVICES {
            if allowed != kind {
                let own_name = serde_json::to_value(kind)?
                    .as_str()
                    .ok_or("service kind not a string")?
                    .to_owned();
                denied(
                    authz::project_access(tx, &unscoped, &first.slug, None, &[allowed], true).await,
                    ErrorCode::Forbidden,
                    &format!("a {own_name} service account cannot do this"),
                )?;
            }
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated migrated PostgreSQL database"]
async fn project_locks_block_membership_and_configuration_until_transaction_ends() -> Result<()> {
    let mut conn = connect().await?;
    let mut competing = connect().await?;
    let creator = user(&mut conn, None, None).await?;
    let first = project(&mut conn, creator, "locks").await?;
    let mut grant_tx = conn.begin().await?;
    assert!(repo::lock_project(&mut grant_tx, first.id).await?);
    repo::set_membership(&mut grant_tx, first.id, creator, Role::Viewer, creator).await?;
    let original_grant = repo::get_member(&mut grant_tx, first.id, creator)
        .await?
        .ok_or("original grant missing")?;
    grant_tx.commit().await?;
    sqlx::query("SET lock_timeout = '100ms'")
        .execute(&mut competing)
        .await?;
    for shared in [false, true] {
        let mut tx = conn.begin().await?;
        if shared {
            repo::share_lock_project(&mut tx, first.id).await?;
        } else {
            assert!(repo::lock_project(&mut tx, first.id).await?);
        }
        let blocked = repo::lock_project(&mut competing, first.id).await;
        match blocked {
            Err(error @ ProjectError::Database(_)) => {
                assert_eq!(error.sqlstate(), Some("55P03"));
                assert!(std::error::Error::source(&error).is_none());
            }
            _ => return Err("project row lock did not block a concurrent writer".into()),
        }
        tx.rollback().await?;
        assert!(repo::lock_project(&mut competing, first.id).await?);
    }
    let mut grant_tx = conn.begin().await?;
    assert!(repo::lock_project(&mut grant_tx, first.id).await?);
    assert_eq!(
        repo::set_membership(&mut grant_tx, first.id, creator, Role::Viewer, creator).await?,
        Some(Role::Viewer)
    );
    let repeated_grant = repo::get_member(&mut grant_tx, first.id, creator)
        .await?
        .ok_or("repeated grant missing")?;
    assert!(repeated_grant.granted_at.0 > original_grant.granted_at.0);
    grant_tx.commit().await?;
    assert_eq!(
        repo::delete_membership(&mut conn, first.id, creator).await?,
        Some(Role::Viewer)
    );
    sqlx::query("DELETE FROM projects WHERE id = $1")
        .bind(first.id)
        .execute(&mut conn)
        .await?;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(creator)
        .execute(&mut conn)
        .await?;
    Ok(())
}

#[test]
fn database_error_diagnostics_redact_private_values() {
    let error = ProjectError::from(sqlx::Error::Protocol(
        "synthetic-private-database-value".into(),
    ));
    assert_eq!(error.to_string(), "project database operation failed");
    assert_eq!(format!("{error:?}"), "Database([redacted])");
    assert!(std::error::Error::source(&error).is_none());
    assert_eq!(error.sqlstate(), None);
}

#[test]
fn absent_membership_never_satisfies_a_role() {
    for minimum in [Role::Viewer, Role::Member, Role::Researcher] {
        assert!(!authz::role_at_least(None, minimum));
    }
}
