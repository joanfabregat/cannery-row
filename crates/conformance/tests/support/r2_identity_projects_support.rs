//! R2 preparation through real OIDC and public identity/project HTTP APIs only.
#![forbid(unsafe_code)]

use super::identity::{Context, Session, claims, opaque, string};
use conformance::Result;
use reqwest::Method;
use serde_json::{Value, json};

pub struct Human {
    pub session: Session,
    pub token: String,
}

pub struct World {
    pub ctx: Context,
    pub admin: Human,
    pub researcher: Human,
    pub viewer: Human,
    pub outsider: Human,
    pub readonly: Value,
    pub project: Value,
    pub slug: String,
}

impl World {
    pub async fn new() -> Result<Self> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let slug = format!("r2-project-{nonce}");
        let mut ctx = Context::new()?;
        let admin = human(
            &mut ctx,
            "conformance-admin",
            "admin@conformance.test",
            "r2-admin",
        )
        .await?;
        assert_eq!(admin.session.me["user"]["is_admin"], true);
        let researcher = human(
            &mut ctx,
            &format!("{slug}-researcher"),
            &format!("{slug}-researcher@conformance.test"),
            "r2-researcher",
        )
        .await?;
        let viewer = human(
            &mut ctx,
            &format!("{slug}-viewer"),
            &format!("{slug}-viewer@conformance.test"),
            "r2-viewer",
        )
        .await?;
        let outsider = human(
            &mut ctx,
            &format!("{slug}-outsider"),
            &format!("{slug}-outsider@conformance.test"),
            "r2-outsider",
        )
        .await?;
        let readonly = ctx
            .mint(&researcher.session, "r2-readonly", &["read"])
            .await?;
        let project = ctx
            .api(
                Method::POST,
                "/api/projects",
                "/api/projects",
                &admin.token,
                Some(json!({"slug":slug,"title":"  Identity project  "})),
                201,
            )
            .await?;
        assert_eq!(project["slug"], slug);
        assert_eq!(project["title"], "Identity project");
        assert_eq!(project["description"], "");
        assert_eq!(project["role"], Value::Null);
        Ok(Self {
            ctx,
            admin,
            researcher,
            viewer,
            outsider,
            readonly,
            project,
            slug,
        })
    }
    pub fn base(&self) -> String {
        format!("/api/projects/{}", self.slug)
    }
    pub async fn memberships(&mut self) -> Result<()> {
        let base = self.base();
        for (human, role) in [(&self.researcher, "researcher"), (&self.viewer, "viewer")] {
            let member = self
                .ctx
                .api(
                    Method::PUT,
                    "/api/projects/{slug}/members/{user_id}",
                    &format!(
                        "{base}/members/{}",
                        string(&human.session.me["user"]["id"])?
                    ),
                    &self.admin.token,
                    Some(json!({"role":role})),
                    200,
                )
                .await?;
            assert_eq!(member["user_id"], human.session.me["user"]["id"]);
            assert_eq!(member["role"], role);
        }
        let members = self
            .ctx
            .api(
                Method::GET,
                "/api/projects/{slug}/members",
                &format!("{base}/members"),
                &self.admin.token,
                None,
                200,
            )
            .await?;
        assert_eq!(
            members["items"].as_array().ok_or("members absent")?.len(),
            2
        );
        for (human, role) in [(&self.researcher, "researcher"), (&self.viewer, "viewer")] {
            assert!(
                members["items"]
                    .as_array()
                    .ok_or("members absent")?
                    .iter()
                    .any(|member| member["user_id"] == human.session.me["user"]["id"]
                        && member["role"] == role)
            );
        }
        Ok(())
    }
}

async fn human(ctx: &mut Context, subject: &str, email: &str, name: &str) -> Result<Human> {
    let session = ctx.login(claims(subject, email, true), "/").await?;
    let token = ctx.mint(&session, name, &["read", "write"]).await?;
    let token = string(&token["token"])?;
    opaque(&token, "cr_pat_")?;
    Ok(Human { session, token })
}

pub fn code(body: &Value, expected: &str) {
    assert_eq!(body["error"]["code"], expected);
}

pub fn items(body: &Value) -> Result<&[Value]> {
    Ok(body["items"].as_array().ok_or("page items absent")?)
}
