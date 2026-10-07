//! Finding listings: provider tools, and listings by seller and title.
//!
//! These read the raw listing JSON and rely only on the few fields they
//! need (`id`, `title`, `seller.slug`, `provider_tool.name`), so a listing
//! the typed [`Listing`] model can't parse (an older server, a partial
//! mock) is still found; parse it with [`ProviderToolListing::listing`].

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::{Stream, StreamExt};
use serde_json::Value;

use super::paginate;
use crate::error::Error;
use crate::models::{Listing, Page};
use crate::resources::ListingQuery;
use crate::response::FromBody;
use crate::Client;

/// A listing offered as a model-callable tool.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderToolListing {
    /// The function name it is offered under (`provider_tool.name`).
    pub name: String,
    pub listing_id: String,
    /// The seller whose credits pay for it.
    pub seller_slug: Option<String>,
    /// The listing in full (the detail response's `data`), with
    /// `buyer_note_schema`.
    pub detail: Value,
    /// The listing's row in the index (summary fields).
    pub summary: Value,
}

impl ProviderToolListing {
    /// The typed view of [`Self::detail`].
    pub fn listing(&self) -> Result<Listing, Error> {
        Ok(Listing::from_body(&self.detail)?)
    }
}

/// Every active listing that advertises a provider tool, with its detail.
/// Walks the whole catalog page by page and fetches each tool listing's
/// detail (the index leaves out `buyer_note_schema`). Order follows the
/// catalog (newest first); duplicate names are kept — callers decide which
/// wins.
pub async fn discover_provider_tools(client: &Client) -> Result<Vec<ProviderToolListing>, Error> {
    discover_provider_tools_where(client, |_, _| true).await
}

/// [`discover_provider_tools`], keeping only the candidates `keep` accepts
/// — called with the tool name and the index row, in catalog order, before
/// the candidate's detail is fetched (so rejected ones cost nothing).
pub async fn discover_provider_tools_where(
    client: &Client,
    mut keep: impl FnMut(&str, &Value) -> bool,
) -> Result<Vec<ProviderToolListing>, Error> {
    let mut found = Vec::new();
    let mut rows = std::pin::pin!(listing_rows(client, ListingQuery::default()));
    while let Some(summary) = rows.next().await {
        let summary = summary?;
        let Some(name) = summary
            .pointer("/provider_tool/name")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(listing_id) = summary.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !keep(name, &summary) {
            continue;
        }
        let (name, listing_id) = (name.to_string(), listing_id.to_string());
        let detail = client.listings().get(&listing_id).await?;
        found.push(ProviderToolListing {
            name,
            listing_id,
            seller_slug: summary
                .pointer("/seller/slug")
                .and_then(Value::as_str)
                .map(str::to_string),
            detail: detail.data().clone(),
            summary,
        });
    }
    Ok(found)
}

/// The id of `seller_slug`'s active listing titled `title` (exactly), if
/// any — a walk of the seller's catalog. [`ListingResolver`] caches hits.
pub async fn find_listing_id(
    client: &Client,
    seller_slug: &str,
    title: &str,
) -> Result<Option<String>, Error> {
    let query = ListingQuery {
        seller: Some(seller_slug.to_string()),
        ..ListingQuery::default()
    };
    let mut rows = std::pin::pin!(listing_rows(client, query));
    while let Some(row) = rows.next().await {
        let row = row?;
        if row.get("title").and_then(Value::as_str) == Some(title) {
            if let Some(id) = row.get("id").and_then(Value::as_str) {
                return Ok(Some(id.to_string()));
            }
        }
    }
    Ok(None)
}

/// Every row of the listing index matching `query`, raw, page by page.
fn listing_rows(
    client: &Client,
    query: ListingQuery,
) -> impl Stream<Item = Result<Value, Error>> + '_ {
    let listings = client.listings();
    paginate(move |cursor| {
        let query = ListingQuery {
            cursor,
            limit: Some(20),
            ..query.clone()
        };
        async move {
            Ok(Page::<Value>::from_body(
                &listings.list(&query).await?.into_raw(),
            )?)
        }
    })
}

/// Finds a listing's id by seller slug and exact title, caching hits for a
/// while (catalog lookups are a page walk). Misses are not cached. Hold on
/// to one resolver for the cache to help; for a one-off lookup call
/// [`find_listing_id`].
pub struct ListingResolver {
    ttl: Duration,
    cache: Mutex<HashMap<(String, String), (String, Instant)>>,
}

impl ListingResolver {
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The id of `seller_slug`'s active listing titled `title`, if any.
    pub async fn resolve(
        &self,
        client: &Client,
        seller_slug: &str,
        title: &str,
    ) -> Result<Option<String>, Error> {
        let key = (seller_slug.to_string(), title.to_string());
        {
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            cache.retain(|_, (_, at)| at.elapsed() < self.ttl);
            if let Some((id, _)) = cache.get(&key) {
                return Ok(Some(id.clone()));
            }
        }
        let found = find_listing_id(client, seller_slug, title).await?;
        if let Some(id) = &found {
            self.cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, (id.clone(), Instant::now()));
        }
        Ok(found)
    }
}

impl Default for ListingResolver {
    /// Hits cached for five minutes.
    fn default() -> Self {
        Self::new(Duration::from_secs(300))
    }
}
