use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info};

use crate::{
    account::{Account, AccountKind},
    client::config::Config,
};

use super::{get_trading_base_url, BoursoWebClient};

impl BoursoWebClient {
    /// Place an order
    ///
    /// # Arguments
    ///
    /// * `side` - Order side (buy or sell)
    /// * `account` - Account to use. Must be a trading account
    /// * `symbol` - Symbol to trade
    /// * `quantity` - Quantity to trade
    /// * `options` - Order options (type, price limit/tolerance, validity). Any
    ///   field left to `None` falls back to the prefilled data from Bourso API.
    ///
    /// # Returns
    /// Order ID and order price limit
    #[cfg(not(tarpaulin_include))]
    pub async fn order(
        &self,
        side: OrderSide,
        account: &Account,
        symbol: &str,
        quantity: usize,
        options: OrderOptions,
    ) -> Result<(String, Option<f64>)> {
        if account.kind != AccountKind::Trading {
            return Err(anyhow::anyhow!("Account is not a trading account"));
        }

        let response = self.prepare(account, symbol).await?;

        debug!("Prepare data {:#?}", response);

        // Start from the prefilled data fetched from Bourso API, then apply the
        // user overrides (order type, price limit/tolerance, validity).
        let mut order_data = response.prefill_order_data.clone();

        let last_price = response.symbol.last_price;
        let nb_decimals = response.symbol.nb_decimals;

        // Resolve the order type: explicit override, else the prefill default (LIM).
        let order_type = options.order_type.unwrap_or(order_data.order_type);

        // The venue advertises which order types it accepts for each side. Refuse
        // up front with a clear message rather than letting the /check endpoint
        // reject the order with an opaque error.
        let allowed = match side {
            OrderSide::Buy => &response.prepare_order_data.list_ord_type.b,
            OrderSide::Sell => &response.prepare_order_data.list_ord_type.s,
        };
        if !allowed.contains(&order_type) {
            return Err(anyhow::anyhow!(
                "Order type {:?} not accepted for {:?} on {} (allowed: {:?})",
                order_type,
                side,
                symbol,
                allowed
            ));
        }

        order_data.order_type = order_type;
        order_data.order_quantity = Some(quantity);
        order_data.order_side = Some(side);
        order_data.order_price_limit = resolve_price_limit(
            side,
            order_type,
            last_price,
            nb_decimals,
            order_data.order_amount,
            &options,
        );

        // Validity / expiration date. A user-provided date lets an order posted
        // off-hours stay valid for the next session(s); otherwise keep the API
        // default (a day order for the upcoming session).
        match &options.validity {
            Some(validity) => {
                order_data.order_expiration_date = Some(validity.clone());
                order_data.order_validity = Some(validity.clone());
            }
            None => {
                order_data.order_expiration_date =
                    response.prefill_order_data.order_validity.clone();
            }
        }

        order_data.resource_id = Some(response.resource_id);

        debug!("Order data: {:#?}", order_data);

        self.check(&order_data).await?;

        let response = self
            .confirm(&order_data.resource_id.as_ref().unwrap())
            .await?;

        info!(
            quantity,
            symbol,
            order_id = response.order_id,
            order_type = ?order_type,
            order_price_limit = order_data.order_price_limit,
            "Order for {} {} ({:?}) successfully passed with ID {} at price {:?} ✅",
            quantity,
            symbol,
            order_type,
            response.order_id,
            order_data.order_price_limit
        );

        Ok((response.order_id, order_data.order_price_limit))
    }

    /// Prepare an order
    ///
    /// This will fetch trading data for the given symbol
    ///
    /// # Arguments
    ///
    /// * `account` - Account to use. Must be a trading account
    /// * `symbol` - Symbol to trade
    ///
    /// # Returns
    ///
    /// An order prepare response
    #[cfg(not(tarpaulin_include))]
    async fn prepare(&self, account: &Account, symbol: &str) -> Result<OrderPrepareResponse> {
        let url = get_order_prepare_url(&self.config, account, symbol)?;
        let response = self.client.get(url).send().await?;

        let status_code = response.status();

        let response = response.text().await?;

        if status_code != 200 {
            return Err(anyhow::anyhow!(
                "Failed to get order prepare response: {}",
                response
            ));
        }

        let response: OrderPrepareResponse = serde_json::from_str(&response).context(format!(
            "Failed to parse order prepare response. Response: {}",
            response
        ))?;

        Ok(response)
    }

    /// Check if an order is valid
    ///
    /// # Arguments
    ///
    /// * `data` - Order data to check
    ///
    /// # Returns
    ///
    /// An order check response
    #[cfg(not(tarpaulin_include))]
    async fn check(&self, data: &OrderData) -> Result<OrderCheckResponse> {
        let url = get_order_check_url(&self.config)?;
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(data)?)
            .send()
            .await?;

        let status_code = response.status();

        let response = response.text().await?;

        if status_code != 200 {
            return Err(anyhow::anyhow!(
                "Failed to get order check response: {}",
                response
            ));
        }

        let response: OrderCheckResponse = serde_json::from_str(&response).context(format!(
            "Failed to parse order check response. Response: {}",
            response
        ))?;

        Ok(response)
    }

    /// Confirm an order
    ///
    /// # Arguments
    ///
    /// * `resource_id` - Resource ID of the order to confirm
    ///
    /// # Returns
    ///
    /// An order confirm response
    #[cfg(not(tarpaulin_include))]
    async fn confirm(&self, resource_id: &str) -> Result<OrderConfirmResponse> {
        let url = get_order_confirm_url(&self.config)?;
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&serde_json::json!({
                "resourceId": resource_id
            }))?)
            .send()
            .await?;

        let status_code = response.status();

        let response = response.text().await?;

        if status_code != 201 {
            return Err(anyhow::anyhow!(
                "Failed to get order confirm response: {}",
                response
            ));
        }

        let response: OrderConfirmResponse = serde_json::from_str(&response).context(format!(
            "Failed to parse order confirm response. Response: {}",
            response
        ))?;

        Ok(response)
    }

    /// Cancel an order that has not been executed yet
    ///
    /// # Arguments
    ///
    /// * `account` - Account to use. Must be a trading account
    /// * `order_id` - ID of the order to cancel
    #[cfg(not(tarpaulin_include))]
    pub async fn cancel_order(&self, account: &Account, order_id: &str) -> Result<()> {
        let url = get_cancel_order_url(&self.config)?;
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&serde_json::json!({
                "accountKey": &account.id,
                "reference": order_id
            }))?)
            .send()
            .await?;

        let status_code = response.status();

        let response = response.text().await?;

        if status_code != 200 {
            return Err(anyhow::anyhow!(
                "Failed to get order prepare response: {}",
                response
            ));
        }

        info!("Order {} successfully cancelled", order_id);

        Ok(())
    }
}

/// User-supplied overrides for a new order.
///
/// Every field is optional; anything left to `None` falls back to the prefilled
/// data returned by the `/order/prepare` endpoint (a day LIM order at the quoted
/// price). This is what lets the CLI post a market order (ATP) or a limit order
/// with a price tolerance — typically outside market hours, where the order
/// joins the next opening auction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OrderOptions {
    /// Order type (LIM, ATP, ...). Defaults to the prefill type (LIM).
    pub order_type: Option<OrderKind>,
    /// Explicit limit price for a LIM order. Takes precedence over `price_tolerance`.
    pub price_limit: Option<f64>,
    /// Price tolerance as a fraction for a LIM order (e.g. `0.02` = 2%). The limit
    /// becomes `last_price * (1 + tol)` for a buy and `last_price * (1 - tol)` for
    /// a sell — a buffer so an order posted off-hours still fills through a
    /// reasonable opening gap without chasing a runaway price.
    pub price_tolerance: Option<f64>,
    /// Order validity / expiration date in "YYYY-MM-DD" format.
    pub validity: Option<String>,
}

/// Round a price to the venue tick precision (`nb_decimals` from the symbol).
fn round_price(price: f64, nb_decimals: i64) -> f64 {
    let factor = 10f64.powi(nb_decimals.max(0) as i32);
    (price * factor).round() / factor
}

/// Compute the `orderPriceLimit` to submit.
///
/// Returns `None` for non-limit orders (a market/ATP order carries no price).
/// For a limit order the precedence is: explicit `price_limit` → `price_tolerance`
/// buffer around `last_price` → the quoted `prefill_amount` → `last_price`.
fn resolve_price_limit(
    side: OrderSide,
    order_type: OrderKind,
    last_price: f64,
    nb_decimals: i64,
    prefill_amount: Option<f64>,
    opts: &OrderOptions,
) -> Option<f64> {
    if order_type != OrderKind::Limit {
        return None;
    }
    if let Some(px) = opts.price_limit {
        return Some(round_price(px, nb_decimals));
    }
    if let Some(tol) = opts.price_tolerance {
        let raw = match side {
            OrderSide::Buy => last_price * (1.0 + tol),
            OrderSide::Sell => last_price * (1.0 - tol),
        };
        return Some(round_price(raw, nb_decimals));
    }
    Some(round_price(prefill_amount.unwrap_or(last_price), nb_decimals))
}

fn get_order_url(config: &Config) -> Result<String> {
    let trading_url = get_trading_base_url(config)?;

    Ok(format!("{}/order", trading_url))
}

fn get_order_prepare_url(config: &Config, account: &Account, symbol: &str) -> Result<String> {
    Ok(
        format!(
            "{}/prepare?_host=tradingboard.boursobank.com&searchExtendedHours=false&selectedAccount={}&symbol={}",
            get_order_url(config)?,
            account.id,
            symbol
        )
    )
}

fn get_order_check_url(config: &Config) -> Result<String> {
    Ok(format!(
        "{}/ordersimple/check",
        get_trading_base_url(config)?
    ))
}

fn get_order_confirm_url(config: &Config) -> Result<String> {
    Ok(format!(
        "{}/ordersimple/confirm",
        get_trading_base_url(config)?
    ))
}

fn get_cancel_order_url(config: &Config) -> Result<String> {
    Ok(format!(
        "{}/orderdetail/cancel",
        get_trading_base_url(config)?
    ))
}

/// Data fetched from the `/order/prepare` endpoint
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderPrepareResponse {
    /// ID of the order, will be used to confirm the order
    pub resource_id: String,
    pub is_pcc: bool,
    pub pcc_rights: PccRights,
    pub has_right_to_assign: bool,
    pub has_right_to_force: bool,
    /// Current position
    pub position: Position,
    /// Account used to place the order informations
    pub account: PrepareOrderAccount,
    pub account_fiscality: AccountFiscality,
    pub account_fees_profile: String,
    pub pending_executed_orders: PendingExecutedOrders,
    pub acceptability_messages: Vec<Value>,
    pub symbol: Symbol,
    pub prepare_order_data: PrepareOrderData,
    pub prefill_order_data: OrderData,
    pub opcvm_message: String,
    pub dici_message: String,
    pub performance_url: String,
    pub execution_policy_url: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PccRights {
    pub allocate: bool,
    pub force: bool,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    pub cash: f64,
    pub srd_coverage: f64,
    pub quantity: i64,
    pub srd_quantity: i64,
}

/// Data fetched from the `/order/prepare` endpoint
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareOrderAccount {
    pub has_pfm: bool,
    /// The account RIB (Relevé d'Identité Bancaire)
    pub rib: String,
    /// The account IBAN (International Bank Account Number)
    pub iban: String,
    /// The account BIC (Bank Identifier Code)
    pub bic: String,
    /// The account number
    pub account_number: String,
    /// The account name
    pub name: String,
    /// The account balance in euros. More like the instant value of the account
    /// depending on the current market value of the assets and the cash balance
    pub balance: f64,
    pub internal: bool,
    /// The account currency
    pub currency: String,
    /// The account type (e.g PEA, PEA-PME, CTO, etc.)
    #[serde(rename = "type")]
    pub type_field: String,
    /// Is the account a professional account
    pub professional: bool,
    /// The account subtype (e.g ISA - Individual Savings Account, etc.)
    pub subtype: String,
    /// The account role (e.g titular)
    pub role: String,
    /// The account bank ID (e.g 1 for BoursoBank)
    pub bank_id: String,
    /// The account bank name (e.g BoursoBank)
    pub bank_name: String,
    pub cash_out: i64,
    pub cash_in: i64,
    pub account_key: String,
    pub pfm_account_key: Value,
    /// The account type category (e.g TRADING, SAVINGS, etc.)
    pub type_category: String,
    pub has_unregular_operations: bool,
    /// The account shortname
    pub short_name: String,
    /// Owned by a minor
    pub minor: bool,
    pub contact_id_owner: Value,
    /// KADOR is a special account for minors by BoursoBank
    #[serde(rename = "isKADOR")]
    pub is_kador: bool,
    pub profile_type: Value,
    pub details: Details,
    pub has_incident: bool,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Details {
    /// The first time a cash transfer was made to the account
    pub first_cash_transfer_date: String,
    /// Gain/Losses in as a float value
    pub gain_losses_percent: f64,
    pub done_gain_losses_percent: f64,
    /// Current cash balance
    pub cash: f64,
    /// Current gain/losses in euros
    pub gain_losses: f64,
    pub done_gain_losses: f64,
    pub clearance_balance: f64,
    /// The account stocks value in euros
    pub stocks: f64,
    /// Today's date in format "2022-11-01"
    pub date: String,
    pub next_liquidation_date: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountFiscality {
    #[serde(rename = "latGL")]
    pub lat_gl: f64,
    #[serde(rename = "realGL")]
    pub real_gl: f64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingExecutedOrders {
    pub pending: i64,
    pub executed: i64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Symbol {
    /// Exchange Label on which the symbol is traded (e.g Euronext Paris)
    pub exchange_label: String,
    /// Symbol ID (e.g 1rTPE500 for AMUNDI PEA S&P 500 ESG UCITS ETF)
    pub symbol: String,
    pub nb_decimals: i64,
    /// Symbol currency (e.g EUR)
    pub currency: String,
    pub label: String,
    /// ISIN (International Securities Identification Numbers) of the symbol (e.g FR0013412285)
    pub isin: String,
    /// Last price of the symbol
    pub last_price: f64,
    /// Morning Star key information document URL (e.g https://doc.morningstar.com/LatestDoc.aspx?clientid=boursorama&key=507703e53b7dec23&language=454&investmentid=F000013MGI&documenttype=299&market=1443&investmenttype=1&frame=0)
    pub fund_morning_star_pdf_url: String,
    pub direct_issuer_kid_url: Value,
    pub priips_kid_url: Value,
    pub allow_tactical_orders: bool,
    pub details: Details2,
    pub extended_hours: ExtendedHours,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Details2 {
    pub opcvm: bool,
    pub affiliated: bool,
    pub direct_issuer: bool,
    pub tracker: bool,
    pub turbo: bool,
    pub warrant: bool,
    pub euronext: bool,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtendedHours {
    pub associated_symbol: String,
    pub lox_symbol: String,
    pub is_eligible: bool,
    pub lox_exchange_id: String,
    pub is_ost: bool,
    pub is_open: bool,
}

/// Data fetched from the `/order/prepare` endpoint and used to fill the default order data
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareOrderData {
    /// Minimum expiration date in format "2022-11-01"
    pub min_expire_tm: String,
    /// Maximum expiration date in format "2022-11-01"
    pub max_expire_tm: String,
    /// Invalid dates list in format "2022-11-01"
    pub invalid_dates_list: Vec<String>,
    /// List of order types per side (buy or sell)
    pub list_ord_type: ListOrdType,
    pub list_risk_md: Vec<String>,
    /// List of possible sides (buy or sell)
    pub side_list: Vec<OrderSide>,
    // pub config_ord_type: ConfigOrdType,
}

/// Possible order types per side (buy or sell)
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListOrdType {
    /// Buy order types
    pub b: Vec<OrderKind>,
    /// Sell order types
    pub s: Vec<OrderKind>,
}

/// Type of order
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum OrderKind {
    #[default]
    #[serde(rename = "LIM")]
    Limit,
    #[serde(rename = "ATP")]
    Market,
    /// Seuil de déclenchement
    #[serde(rename = "STP")]
    StopLoss,
    /// Plage de déclenchement
    #[serde(rename = "SLM")]
    StopLossMargin,
    #[serde(rename = "TSO")]
    TrailingStopOrder,
    /// One Cancels the Other order
    #[serde(rename = "OCO")]
    OneCancelsOther,
    #[serde(rename = "TAL")]
    TradeAtLast,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default, clap::ValueEnum)]
pub enum OrderSide {
    #[default]
    #[serde(rename = "B")]
    Buy,
    #[serde(rename = "S")]
    Sell,
}

/// Order data submitted to the `/ordersimple/check` endpoint
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OrderData {
    #[serde(rename = "orderType")]
    order_type: OrderKind,
    #[serde(rename = "orderSide")]
    order_side: Option<OrderSide>,
    #[serde(rename = "orderQuantity")]
    order_quantity: Option<usize>,
    /// Expiration date in format "2022-11-01"
    #[serde(rename = "orderExpirationDate")]
    order_expiration_date: Option<String>,
    #[serde(rename = "orderRiskMode")]
    order_risk_mode: String,
    /// To use at the `/ordersimple/check` endpoint
    #[serde(rename = "orderPriceLimit")]
    order_price_limit: Option<f64>,
    /// Received at the `/order/prepare` endpoint
    #[serde(rename = "orderAmount")]
    order_amount: Option<f64>,
    #[serde(rename = "resourceId")]
    resource_id: Option<String>,
    /// Received at the `/order/prepare` endpoint
    /// Validity date in format "2022-11-01"
    #[serde(rename = "orderValidity")]
    order_validity: Option<String>,

    /// Received at the `/ordersimple/check` endpoint
    #[serde(rename = "buyingPower")]
    pub buying_power: Option<f64>,
    /// Received at the `/ordersimple/check` endpoint
    #[serde(rename = "stopPx")]
    pub stop_px: Option<Value>,
    /// Received at the `/ordersimple/check` endpoint
    #[serde(rename = "trailPct")]
    pub trail_pct: Option<Value>,
    /// Received at the `/ordersimple/check` endpoint
    #[serde(rename = "estimatedFees")]
    pub estimated_fees: Option<Vec<EstimatedFee>>,
    /// Received at the `/ordersimple/check` endpoint
    #[serde(rename = "exchangeLabel")]
    pub exchange_label: Option<String>,
    /// Received at the `/ordersimple/check` endpoint
    #[serde(rename = "feesExplanation")]
    pub fees_explanation: Option<FeesExplanation>,
    /// Received at the `/ordersimple/check` endpoint
    #[serde(rename = "estimatedBalance")]
    pub estimated_balance: Option<f64>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderCheckResponse {
    pub acceptability_messages: Option<Vec<AcceptabilityMessage>>,
    pub check_order_data: OrderData,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptabilityMessage {
    #[serde(rename = "type")]
    pub type_field: String,
    pub content: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimatedFee {
    #[serde(rename = "type")]
    pub type_field: String,
    pub label: String,
    pub amount: f64,
    pub percentage: f64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeesExplanation {
    pub start_amount: String,
    pub product_fee: String,
    pub service_fee: String,
    pub scenarios: Vec<ScenarioMessage>,
    // pub translations: Translations,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioMessage {
    pub title: String,
    pub content: Vec<Vec<String>>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderConfirmResponse {
    pub order_id: String,
    pub order_state_label: String,
    pub ord_stat: String,
    pub action_message: ActionMessage,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionMessage {
    pub id: String,
    #[serde(rename = "type")]
    pub type_field: String,
    pub detail: Value,
    pub title: Value,
    pub body: Value,
    pub params: Value,
    pub category: Value,
    pub actions: Vec<Action>,
    pub flags: Vec<Value>,
    pub targets: Vec<Value>,
    pub visual_id: Value,
    pub visual_theme: Value,
    pub medias: Vec<Value>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub label: Value,
    pub feature_id: Value,
    pub web: Value,
    pub api: ActionApi,
    pub disabled: bool,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionApi {
    pub href: Value,
    pub method: Value,
    pub params: ActionApiParams,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionApiParams {
    pub account_type: String,
    pub account_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the 2026-06-30 incident: once the PEA has a realized
    /// loss, the `order/prepare` response returns `accountFiscality.realGL` as a
    /// FLOAT (e.g. -20.84). The field was typed `i64`, so `order` crashed with
    /// "invalid type: floating point `-20.84`, expected i64" and the morning buy
    /// never went through. Every monetary field must deserialize as `f64`.
    ///
    /// The fixture below is a verbatim capture of a real PEA `order/prepare`
    /// response (logs/cron_pea.log, 2026-06-30), so it also guards the whole
    /// `OrderPrepareResponse` shape against future field-type drift.
    #[test]
    fn order_prepare_deserializes_with_float_realgl() {
        let json = r#"{"resourceId":"59a9a334b0394","isPcc":false,"pccRights":{"allocate":false,"force":false},"hasRightToAssign":false,"hasRightToForce":false,"position":{"cash":679.16,"srdCoverage":679.16,"quantity":0,"srdQuantity":0},"account":{"hasPfm":false,"rib":"40618 80610 00088465900 69","iban":"FR7640618806100008846590069","bic":"BOUSFRPPXXX","accountNumber":"00088465900","name":"PEA DESCAMPS","balance":679.16,"internal":true,"currency":"EUR","type":"PEA","professional":false,"subtype":"OMS_ACCOUNT_ISA","role":"titular","bankId":"1","bankName":"BoursoBank","cashOut":0,"cashIn":1,"accountKey":"faab190372918f26c5d2d518fd307d05","pfmAccountKey":null,"typeCategory":"TRADING","hasUnregularOperations":false,"shortName":"PEA DESCAMPS","minor":false,"contactIdOwner":null,"isKADOR":false,"profileType":null,"details":{"firstCashTransferDate":"2026-06-16","gainLossesPercent":0,"doneGainLossesPercent":0,"cash":679.16,"gainLosses":0,"doneGainLosses":0,"clearanceBalance":0,"stocks":0,"date":"2026-06-30","isDmc":false,"nextLiquidationDate":"2026-07-28"},"hasIncident":false},"accountFiscality":{"latGL":0,"realGL":-20.84},"accountFeesProfile":"DECOUVERTE","pendingExecutedOrders":{"pending":0,"executed":3},"acceptabilityMessages":[],"symbol":{"exchangeLabel":"Euronext Paris","symbol":"1rTPUST","nbDecimals":4,"currency":"EUR","label":"Amundi PEA Nasdaq-100 UCITS ETF Acc","isin":"FR0011871110","lastPrice":105.06,"fundMorningStarPdfUrl":"https:\/\/doc.morningstar.com\/LatestDoc.aspx?clientid=boursorama&key=507703e53b7dec23&language=454&investmentid=F00000TNQV&documenttype=299&market=1443&investmenttype=1&frame=0","directIssuerKidUrl":null,"priipsKidUrl":null,"allowTacticalOrders":true,"details":{"opcvm":false,"affiliated":false,"directIssuer":false,"tracker":true,"turbo":false,"warrant":false,"euronext":true},"extendedHours":{"associatedSymbol":"","loxSymbol":"","isEligible":false,"loxExchangeId":"","isOst":false,"isOpen":false}},"prepareOrderData":{"minExpireTm":"2026-06-30","maxExpireTm":"2027-06-29","invalidDatesList":["2026-12-25","2027-01-01"],"listOrdType":{"b":["ATP","LIM","STP","SLM","TSO"],"s":["ATP","LIM","STP","SLM","TSO"]},"listRiskMd":["CPT"],"sideList":["B","S"],"configOrdType":{"ATP":"Au marché (ex ATP)","LIM":"Ordre limité","STP":"Seuil de déclenchement","SLM":"Plage de déclenchement","TSO":"Ordre Suiveur","OCO":"Ordre Alternatif","TAL":"Trade At Last"}},"prefillOrderData":{"orderRiskMode":"CPT","orderAmount":105.06,"orderQuantity":null,"orderPriceLimit":null,"orderType":"LIM","orderValidity":"2026-06-30","alternativeOrder":{"orderType":"LIM"},"securedOrder":{"orderType":"LIM"}},"opcvmMessage":"","diciMessage":"\n     En confirmant le passage d'ordre, je reconnais avoir pris connaissance du <a href=\"https:\/\/doc.morningstar.com\/LatestDoc.aspx?clientid=boursorama&key=507703e53b7dec23&language=454&investmentid=F00000TNQV&documenttype=299&market=1443&investmenttype=1&frame=0\" target=\"_blank\" rel=\"noreferrer noopener\">DIC<\/a>.\n    ","performanceUrl":"https:\/\/www.boursobank.com\/static\/file\/default\/179864425\/i\/bourse\/performance.jpg","executionPolicyUrl":"https:\/\/bourse.boursobank.com\/bourse\/politique-execution\/"}"#;

        let resp: OrderPrepareResponse = serde_json::from_str(json)
            .expect("order/prepare must deserialize when realGL is a float");
        assert_eq!(resp.account_fiscality.real_gl, -20.84);
        assert_eq!(resp.account_fiscality.lat_gl, 0.0);
        assert_eq!(resp.position.cash, 679.16);
    }

    #[test]
    fn round_price_respects_decimals() {
        assert_eq!(round_price(105.06789, 4), 105.0679);
        assert_eq!(round_price(105.06789, 2), 105.07);
        assert_eq!(round_price(105.0, 0), 105.0);
    }

    #[test]
    fn limit_buy_tolerance_adds_buffer() {
        let opts = OrderOptions {
            price_tolerance: Some(0.02),
            ..Default::default()
        };
        let px = resolve_price_limit(OrderSide::Buy, OrderKind::Limit, 100.0, 4, None, &opts);
        assert_eq!(px, Some(102.0));
    }

    #[test]
    fn limit_sell_tolerance_subtracts_buffer() {
        let opts = OrderOptions {
            price_tolerance: Some(0.02),
            ..Default::default()
        };
        let px = resolve_price_limit(OrderSide::Sell, OrderKind::Limit, 100.0, 4, None, &opts);
        assert_eq!(px, Some(98.0));
    }

    #[test]
    fn explicit_limit_overrides_tolerance() {
        let opts = OrderOptions {
            price_limit: Some(99.5),
            price_tolerance: Some(0.02),
            ..Default::default()
        };
        let px = resolve_price_limit(OrderSide::Buy, OrderKind::Limit, 100.0, 4, None, &opts);
        assert_eq!(px, Some(99.5));
    }

    #[test]
    fn market_order_carries_no_price_limit() {
        let opts = OrderOptions {
            order_type: Some(OrderKind::Market),
            price_tolerance: Some(0.02),
            ..Default::default()
        };
        let px = resolve_price_limit(OrderSide::Buy, OrderKind::Market, 100.0, 4, None, &opts);
        assert_eq!(px, None);
    }

    #[test]
    fn limit_without_options_falls_back_to_amount_then_last() {
        let opts = OrderOptions::default();
        assert_eq!(
            resolve_price_limit(OrderSide::Buy, OrderKind::Limit, 100.0, 4, Some(105.06), &opts),
            Some(105.06)
        );
        assert_eq!(
            resolve_price_limit(OrderSide::Buy, OrderKind::Limit, 100.0, 4, None, &opts),
            Some(100.0)
        );
    }
}
