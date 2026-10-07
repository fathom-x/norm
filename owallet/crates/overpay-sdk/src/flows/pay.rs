//! Placing an order and paying for it with merchant credits.

use serde::Deserialize;
use serde_json::Value;

use crate::auth::Auth;
use crate::error::Error;
use crate::generated::PaymentOutcomeStatus;
use crate::models::{Cents, PaymentOutcome, Redemption};
use crate::resources::CreateOrder;
use crate::Client;

/// A paid order.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct Settled {
    pub order_id: String,
    /// `fully_paid`, or `already_paid` (e.g. an order born paid).
    pub status: PaymentOutcomeStatus,
    /// What the payment drew from credits, when the API said.
    pub amount_redeemed_cents: Option<Cents>,
    /// The order's raw body (`{"data": {...}}`), as created.
    pub order: Value,
}

/// Why an order couldn't be paid for.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum PayError {
    /// Credits don't cover a buyer-set authorization: the API cancelled
    /// the order (402 `insufficient_credits`). `body` is the raw response.
    #[error("not enough merchant credits to authorize {authorization_cents} cents: {body}")]
    InsufficientCredits {
        authorization_cents: i64,
        body: String,
    },
    /// The order exists but isn't paid (e.g. credits ran short). It stays
    /// pending and lapses on its own. `message` is the API's explanation.
    #[error("order {order_id} was not paid ({status}): {}", message.as_deref().unwrap_or("no reason given"))]
    NotSettled {
        order_id: String,
        status: PaymentOutcomeStatus,
        message: Option<String>,
    },
    #[error(transparent)]
    Api(#[from] Error),
}

/// The parts of a create-order response the pay flows need. A success
/// without them isn't an order ([`Error::Json`]).
#[derive(Deserialize)]
struct Created {
    data: CreatedId,
}

#[derive(Deserialize)]
struct CreatedPaid {
    data: CreatedId,
    payment: PaymentOutcome,
}

#[derive(Deserialize)]
struct CreatedId {
    id: String,
}

impl Client {
    /// Create an unpaid order — the first step of paying in two calls, so
    /// the caller can look at its price before [`Self::pay_with_credits`].
    /// `order` carries the listing, note, variant and session label; its
    /// payment fields are ignored. Returns the raw body; the id is at
    /// `/data/id`.
    pub async fn create_unpaid_order(
        &self,
        order: &CreateOrder,
        auth: Auth<'_>,
    ) -> Result<Value, Error> {
        let mut order = order.clone();
        order.pay_with_credits = false;
        order.authorization_cents = None;
        order.spend_authorization_id = None;
        Ok(self.orders().create(&order, auth).await?.into_raw())
    }

    /// Pay a pending order with credits held at `seller_slug` (or its
    /// organization). `Ok` only when the order ends up paid.
    pub async fn pay_with_credits(
        &self,
        seller_slug: &str,
        order_id: &str,
        auth: Auth<'_>,
    ) -> Result<Redemption, PayError> {
        let redemption = self
            .credits()
            .redeem(seller_slug, order_id, auth)
            .await?
            .parse()?;
        if !redemption.is_settled() {
            return Err(PayError::NotSettled {
                order_id: order_id.to_string(),
                status: PaymentOutcomeStatus::from(redemption.status.as_str()),
                message: redemption.message,
            });
        }
        Ok(redemption)
    }

    /// Create and pay an order with a buyer-set authorization in one call
    /// (metered listings): `order` with `authorization_cents` and paying by
    /// credits set. All-or-nothing: if credits don't cover it the API
    /// cancels the order — [`PayError::InsufficientCredits`].
    pub async fn create_paid_order(
        &self,
        order: &CreateOrder,
        authorization_cents: i64,
        auth: Auth<'_>,
    ) -> Result<Settled, PayError> {
        let order = order.clone().authorization_cents(authorization_cents);
        let body = match self.orders().create(&order, auth).await {
            Ok(response) => response.into_raw(),
            Err(Error::Api(e)) if e.is_insufficient_credits() => {
                return Err(PayError::InsufficientCredits {
                    authorization_cents,
                    body: e.body,
                })
            }
            Err(e) => return Err(e.into()),
        };
        let created = CreatedPaid::deserialize(&body).map_err(Error::from)?;
        let (order_id, payment) = (created.data.id, created.payment);
        if !payment.is_settled() {
            let message = payment.error.or_else(|| {
                payment
                    .extra
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
            return Err(PayError::NotSettled {
                order_id,
                status: payment.status,
                message,
            });
        }
        Ok(Settled {
            order_id,
            status: payment.status,
            amount_redeemed_cents: payment.amount_redeemed_cents,
            order: body,
        })
    }

    /// Create an order and pay it with credits held at `seller_slug`, in two
    /// calls ([`Self::create_unpaid_order`], then [`Self::pay_with_credits`]).
    /// The order is left pending if paying fails.
    pub async fn place_and_pay(
        &self,
        order: &CreateOrder,
        seller_slug: &str,
        auth: Auth<'_>,
    ) -> Result<Settled, PayError> {
        let order = self.create_unpaid_order(order, auth).await?;
        let order_id = Created::deserialize(&order).map_err(Error::from)?.data.id;
        let redemption = self.pay_with_credits(seller_slug, &order_id, auth).await?;
        Ok(Settled {
            order_id,
            status: PaymentOutcomeStatus::from(redemption.status.as_str()),
            amount_redeemed_cents: Some(redemption.amount_redeemed_cents),
            order,
        })
    }
}
