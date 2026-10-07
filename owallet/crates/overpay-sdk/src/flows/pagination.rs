use std::collections::HashSet;
use std::future::Future;

use futures_util::stream::{self, Stream};

use crate::auth::Auth;
use crate::error::Error;
use crate::models::{CreditTransaction, Listing, Order, Page};
use crate::resources::{
    CreditsApi, ListingQuery, ListingsApi, OrderQuery, OrdersApi, TransactionQuery,
};

/// Every item of a cursor-paginated list, fetching pages as the stream is
/// read. `fetch` gets the cursor (`None` for the first page). A cursor the
/// server already handed out ends the walk: following it again could only
/// repeat pages, forever.
pub fn paginate<T, F, Fut>(fetch: F) -> impl Stream<Item = Result<T, Error>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<Page<T>, Error>>,
{
    struct State<T, F> {
        fetch: F,
        buffer: std::vec::IntoIter<T>,
        cursor: Option<String>,
        /// Every cursor followed so far.
        seen: HashSet<String>,
        done: bool,
    }
    let state = State {
        fetch,
        buffer: Vec::new().into_iter(),
        cursor: None,
        seen: HashSet::new(),
        done: false,
    };
    stream::unfold(state, |mut st| async move {
        loop {
            if let Some(item) = st.buffer.next() {
                return Some((Ok(item), st));
            }
            if st.done {
                return None;
            }
            match (st.fetch)(st.cursor.take()).await {
                Ok(page) => {
                    st.buffer = page.data.into_iter();
                    match page.next_cursor {
                        Some(next) if !next.is_empty() && st.seen.insert(next.clone()) => {
                            st.cursor = Some(next);
                        }
                        _ => st.done = true,
                    }
                }
                Err(e) => {
                    st.done = true;
                    st.buffer = Vec::new().into_iter();
                    return Some((Err(e), st));
                }
            }
        }
    })
}

impl<'a> ListingsApi<'a> {
    /// Every listing matching `query`, page by page.
    pub fn list_all(self, query: ListingQuery) -> impl Stream<Item = Result<Listing, Error>> + 'a {
        paginate(move |cursor| {
            let query = ListingQuery {
                cursor,
                ..query.clone()
            };
            async move { self.list(&query).await?.parse() }
        })
    }
}

impl<'a> OrdersApi<'a> {
    /// Every order matching `query`, page by page.
    pub fn list_all(
        self,
        query: OrderQuery,
        auth: Auth<'a>,
    ) -> impl Stream<Item = Result<Order, Error>> + 'a {
        paginate(move |cursor| {
            let query = OrderQuery {
                cursor,
                ..query.clone()
            };
            async move { self.list(&query, auth).await?.parse() }
        })
    }
}

impl<'a> CreditsApi<'a> {
    /// The whole ledger matching `query`, page by page.
    pub fn transactions_all(
        self,
        query: TransactionQuery,
        auth: Auth<'a>,
    ) -> impl Stream<Item = Result<CreditTransaction, Error>> + 'a {
        paginate(move |cursor| {
            let query = TransactionQuery {
                cursor,
                ..query.clone()
            };
            async move { self.transactions(&query, auth).await?.parse() }
        })
    }
}
