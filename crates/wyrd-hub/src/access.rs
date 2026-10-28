//! Organisation access: a caller's role in an org, where an org the
//! caller does not belong to is indistinguishable from one that does not
//! exist (404, never 403).

use axum::http::StatusCode;

use crate::Problem;
use crate::auth::{Caller, Role};
use crate::db::{self, PgPool};

/// The caller's standing in one organisation.
#[derive(Debug, Clone)]
pub struct OrgAccess {
    pub org_id: String,
    pub slug: String,
    pub role: Role,
}

impl OrgAccess {
    /// 403 when the caller's role is below `min` (the org itself is
    /// visible to every member, so this is not a leak).
    pub fn require(&self, min: Role) -> Result<(), Problem> {
        if self.role >= min {
            Ok(())
        } else {
            Err(Problem::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "Your role does not allow this",
            )
            .detail(format!("requires {}", min.as_str())))
        }
    }
}

/// The caller's role in the org with `slug`, or 404.
pub async fn org(pool: &PgPool, caller: &Caller, slug: &str) -> Result<OrgAccess, Problem> {
    let row: Option<(String, String)> = db::query_as(
        "SELECT o.id, m.role FROM orgs o JOIN memberships m ON m.org_id = o.id \
         WHERE o.slug = $1 AND m.user_id = $2",
    )
    .bind(slug)
    .bind(&caller.user_id)
    .fetch_optional(pool)
    .await?;
    let (org_id, role) = row.ok_or_else(Problem::not_found)?;
    if caller.token_org.as_ref().is_some_and(|t| *t != org_id) {
        return Err(Problem::not_found());
    }
    let role = Role::parse(&role).ok_or_else(|| Problem::internal(&"unknown role in database"))?;
    Ok(OrgAccess {
        org_id,
        slug: slug.to_string(),
        role,
    })
}

/// The caller's role in the org with id `org_id`, or 404 (for resources
/// addressed by their own id, such as bundles).
pub async fn org_by_id(pool: &PgPool, caller: &Caller, org_id: &str) -> Result<OrgAccess, Problem> {
    let slug: Option<String> = db::query_scalar("SELECT slug FROM orgs WHERE id = $1")
        .bind(org_id)
        .fetch_optional(pool)
        .await?;
    org(pool, caller, &slug.ok_or_else(Problem::not_found)?).await
}
