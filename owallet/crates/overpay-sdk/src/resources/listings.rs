use crate::auth::Auth;
use crate::client::{Client, Request};
use crate::error::Error;
use crate::models::{Listing, Page, Seller};
use crate::response::Response;

use super::segment;

/// Filters for [`ListingsApi::list`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListingQuery {
    pub category: Option<String>,
    /// Seller slug.
    pub seller: Option<String>,
    pub cursor: Option<String>,
    /// Page size (the API caps it at 20).
    pub limit: Option<u32>,
}

/// `GET /api/v1/listings[/{id}]` — public, no auth.
#[derive(Debug, Clone, Copy)]
pub struct ListingsApi<'a>(pub(crate) &'a Client);

impl ListingsApi<'_> {
    /// One page of active public listings, newest first (summary fields).
    pub async fn list(&self, query: &ListingQuery) -> Result<Response<Page<Listing>>, Error> {
        let request = Request::get("/api/v1/listings")
            .query("category", query.category.as_deref())
            .query("seller", query.seller.as_deref())
            .query("cursor", query.cursor.as_deref())
            .query("limit", query.limit);
        Ok(Response::new(self.0.execute(request, Auth::None).await?))
    }

    /// One listing in full, with the schemas needed to order it.
    pub async fn get(&self, id: &str) -> Result<Response<Listing>, Error> {
        let request = Request::get(format!("/api/v1/listings/{}", segment(id)));
        Ok(Response::new(self.0.execute(request, Auth::None).await?))
    }
}

/// `GET /api/v1/sellers/{slug}` — public, no auth.
#[derive(Debug, Clone, Copy)]
pub struct SellersApi<'a>(pub(crate) &'a Client);

impl SellersApi<'_> {
    pub async fn get(&self, slug: &str) -> Result<Response<Seller>, Error> {
        let request = Request::get(format!("/api/v1/sellers/{}", segment(slug)));
        Ok(Response::new(self.0.execute(request, Auth::None).await?))
    }
}
