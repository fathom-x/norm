//! `oauth_clients` table — registered OAuth clients for the local AS.
//!
//! `redirect_uris` and `grant_types` are stored as JSON arrays for
//! compatibility with the Python `json.dumps(list[str])` format.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthClientRow {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub scope: Option<String>,
    pub token_endpoint_auth_method: Option<String>,
    pub registered_at: i64,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn upsert(
    conn: &Connection,
    client_id: &str,
    client_secret: Option<&str>,
    redirect_uris: &[String],
    grant_types: &[String],
    scope: Option<&str>,
    token_endpoint_auth_method: Option<&str>,
    now: i64,
) -> Result<()> {
    let redirect_uris_json = serde_json::to_string(redirect_uris)?;
    let grant_types_json = serde_json::to_string(grant_types)?;
    conn.execute(
        "INSERT INTO oauth_clients(client_id, client_secret, redirect_uris, grant_types, scope, token_endpoint_auth_method, registered_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(client_id) DO UPDATE SET
             redirect_uris             = excluded.redirect_uris,
             grant_types               = excluded.grant_types,
             scope                     = excluded.scope,
             token_endpoint_auth_method = excluded.token_endpoint_auth_method",
        params![
            client_id,
            client_secret,
            redirect_uris_json,
            grant_types_json,
            scope,
            token_endpoint_auth_method,
            now
        ],
    )?;
    Ok(())
}

pub(crate) fn read(conn: &Connection, client_id: &str) -> Result<Option<OAuthClientRow>> {
    let row = conn
        .query_row(
            "SELECT client_id, client_secret, redirect_uris, grant_types, scope, token_endpoint_auth_method, registered_at
             FROM oauth_clients WHERE client_id = ?1",
            params![client_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;

    let Some((
        client_id,
        client_secret,
        redirect_uris_json,
        grant_types_json,
        scope,
        tep_auth,
        registered_at,
    )) = row
    else {
        return Ok(None);
    };

    Ok(Some(OAuthClientRow {
        client_id,
        client_secret,
        redirect_uris: serde_json::from_str(&redirect_uris_json)?,
        grant_types: serde_json::from_str(&grant_types_json)?,
        scope,
        token_endpoint_auth_method: tep_auth,
        registered_at,
    }))
}
