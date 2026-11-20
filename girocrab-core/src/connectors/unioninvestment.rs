//! Union Investment customer portal (UnionDepot). Requests and authorization stay
//! inside the embedded browser; only the selected depot data crosses the bridge.
//! The portal is the only interface (no FinTS/PSD2), see `docs/unioninvestment-connector.md`.
use std::{collections::HashSet, sync::Arc};

use serde_json::Value;

use super::{FetchResult, RemoteAccount, RemoteHolding, SyncSummary};
use crate::{
    error::{CoreError, CoreResult},
    i18n::tr,
    models::{AccountKind, ConnectorKind, Holding},
    vault::Vault,
};

fn invalid() -> CoreError {
    CoreError::InvalidInput {
        message: tr("Union Investment: Unvollständige oder ungültige Portalantwort. Es wurden keine Daten übernommen.").into(),
    }
}

fn text<'a>(v: &'a Value, key: &str) -> CoreResult<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(invalid)
}

fn date(v: &Value, key: &str) -> CoreResult<String> {
    let s = text(v, key)?.get(..10).ok_or_else(invalid)?;
    jiff::civil::Date::strptime("%Y-%m-%d", s).map_err(|_| invalid())?;
    Ok(s.into())
}

fn number(v: &Value, key: &str) -> CoreResult<f64> {
    v[key]
        .as_f64()
        .filter(|n| n.is_finite() && n.abs() < 1e12)
        .ok_or_else(invalid)
}

// The portal returns binary-float artefacts (e.g. 8398.279999999999).
// Round monetary values once at the boundary, never quantities.
fn cents_of(n: f64) -> i64 {
    (n * 100.0).round() as i64
}

fn cents(v: &Value, key: &str) -> CoreResult<i64> {
    Ok(cents_of(number(v, key)?))
}

fn rows<'a>(v: &'a Value, key: &str) -> CoreResult<&'a Vec<Value>> {
    v[key].as_array().ok_or_else(invalid)
}

/// One position per ISIN: the portal lists one entry per investment goal
/// (sub-depot), and several goals may hold the same fund. Quantities, values
/// and gains are summed; the price is the (identical) redemption price.
struct Position {
    isin: String,
    name: String,
    quantity: f64,
    price: f64,
    value: i64,
    unrealized: i64,
}

fn positions(depot: &Value) -> CoreResult<Vec<Position>> {
    let mut out: Vec<Position> = Vec::new();
    for item in rows(depot, "items")? {
        let isin = text(item, "isin")?;
        if isin.len() != 12 || !isin.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(invalid());
        }
        let quantity = number(item, "quantity")?;
        if quantity < 0.0 || text(item, "currency")? != "EUR" {
            return Err(invalid());
        }
        date(item, "priceDate")?;
        let value = cents(item, "value")?;
        let unrealized = cents(item, "unrealized")?;
        let price = number(item, "price")?;
        match out.iter_mut().find(|p| p.isin == isin) {
            Some(p) => {
                p.quantity += quantity;
                p.value += value;
                p.unrealized += unrealized;
            }
            None => out.push(Position {
                isin: isin.into(),
                name: text(item, "name")?.into(),
                quantity,
                price,
                value,
                unrealized,
            }),
        }
    }
    Ok(out)
}

fn holding(p: &Position) -> Holding {
    let cost = p.value - p.unrealized;
    // cost price per share; without shares or a cost basis there is nothing to show
    let purchase = if p.quantity > 0.0 && cost > 0 {
        (cost as f64 / p.quantity).round() as i64
    } else {
        0
    };
    Holding {
        isin: p.isin.clone(),
        name: p.name.clone(),
        quantity: p.quantity,
        current_price_cents: cents_of(p.price),
        purchase_price_cents: purchase,
        value_cents: p.value,
        profit_loss_purchase_cents: (purchase > 0).then_some(p.unrealized),
        profit_loss_purchase_rel: (cost > 0).then(|| p.unrealized as f64 / cost as f64 * 100.0),
        ..Default::default()
    }
}

fn snapshot(json: &str) -> CoreResult<FetchResult> {
    let root: Value = serde_json::from_str(json).map_err(|_| invalid())?;
    if root["version"].as_u64() != Some(1) {
        return Err(invalid());
    }
    let as_of = date(&root, "asOf")?;
    let mut result = FetchResult {
        bank_name: Some("Union Investment".into()),
        ..Default::default()
    };
    let mut seen = HashSet::new();
    for depot in rows(&root, "depots")? {
        let number_ = text(depot, "number")?;
        if !number_.bytes().all(|b| b.is_ascii_digit()) || !seen.insert(number_.to_string()) {
            return Err(invalid());
        }
        let account_ref = format!("depot:{number_}");
        let positions = positions(depot)?;
        let depot_value = cents(depot, "value")?;
        // the portal's own depot value must match the listed funds (rounding: 1 ct per row)
        let total: i64 = positions.iter().map(|p| p.value).sum();
        if (total - depot_value).abs() > positions.len() as i64 {
            return Err(invalid());
        }
        result.accounts.push(RemoteAccount {
            external_ref: account_ref.clone(),
            name: text(depot, "name")?.into(),
            owner: text(depot, "owner")?.into(),
            iban: String::new(),
            account_number: number_.into(),
            currency: "EUR".into(),
            kind: AccountKind::Brokerage,
            balance_cents: depot_value,
            balance_date: as_of.clone(),
        });
        for p in &positions {
            result.holdings.push(RemoteHolding {
                classification: Default::default(),
                direct_crypto: false,
                account_ref: account_ref.clone(),
                holding: holding(p),
            });
        }
        // an empty depot clears positions that were sold since the last fetch
        result.holdings_fetched.push(account_ref);
    }
    if result.accounts.is_empty() {
        return Err(invalid());
    }
    Ok(result)
}

#[uniffi::export]
impl Vault {
    pub fn import_unioninvestment_snapshot(
        self: Arc<Self>,
        connection_id: i64,
        snapshot_json: String,
    ) -> CoreResult<SyncSummary> {
        if self.portal_connection(connection_id)?.0 != ConnectorKind::UnionInvestment {
            return Err(invalid());
        }
        crate::banking_gate::check_connector("unioninvestment")?;
        let summary = super::store_fetch(
            &self,
            ConnectorKind::UnionInvestment,
            connection_id,
            snapshot(&snapshot_json)?,
        )?;
        let _ = self.checkpoint();
        Ok(summary)
    }
}

pub struct WebOnlyConnector;

#[async_trait::async_trait]
impl super::BankConnector for WebOnlyConnector {
    async fn begin(&mut self, _ctx: &super::ConnectContext) -> CoreResult<super::Phase> {
        Err(CoreError::BrowserLoginRequired {
            message: tr(
                "Bitte bei Union Investment im Browserfenster anmelden und die Daten abrufen.",
            )
            .into(),
        })
    }
    async fn respond(
        &mut self,
        ctx: &super::ConnectContext,
        _response: super::ChallengeResponse,
    ) -> CoreResult<super::Phase> {
        self.begin(ctx).await
    }
    async fn fetch(&mut self, _ctx: &super::ConnectContext) -> CoreResult<FetchResult> {
        Err(CoreError::BrowserLoginRequired {
            message: tr("Bitte bei Union Investment im Browserfenster abrufen.").into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(isin: &str, quantity: f64, price: f64, value: f64, unrealized: f64) -> Value {
        json!({"isin":isin,"name":"Test Fonds","quantity":quantity,"price":price,
            "priceDate":"2026-11-19","currency":"EUR","value":value,"unrealized":unrealized})
    }

    fn fixture() -> Value {
        json!({"version":1,"asOf":"2026-11-20","depots":[{
        "number":"123456789","owner":"Test Owner","name":"UnionDepot","value":330.0,
        "items":[
            item("DE0000000001", 2.5, 40.0, 100.0, 10.0),
            // same fund in a second goal: summed into one position
            item("DE0000000001", 2.5, 40.0, 100.0, -5.0),
            item("DE0000000002", 10.0, 13.0, 130.0, 0.0)
        ]}]})
    }

    #[test]
    fn maps_depot_and_sums_goals_per_isin() {
        let r = snapshot(&fixture().to_string()).unwrap();
        assert_eq!(r.accounts.len(), 1);
        assert_eq!(r.accounts[0].kind, AccountKind::Brokerage);
        assert_eq!(r.accounts[0].balance_cents, 33000);
        assert_eq!(r.accounts[0].balance_date, "2026-11-20");
        assert_eq!(r.holdings.len(), 2);
        let h = &r.holdings[0].holding;
        assert_eq!(h.quantity, 5.0);
        assert_eq!(h.value_cents, 20000);
        // cost basis = value - unrealized gain: (20000 - 500) / 5 shares
        assert_eq!(h.purchase_price_cents, 3900);
        assert_eq!(h.profit_loss_purchase_cents, Some(500));
        assert_eq!(r.holdings_fetched, vec!["depot:123456789".to_string()]);
    }

    #[test]
    fn float_artifacts_are_rounded_once() {
        let mut f = fixture();
        f["depots"][0]["items"] =
            json!([item("DE0000000001", 1.0001, 83.98, 83.98279999999999, 0.0)]);
        f["depots"][0]["value"] = json!(83.98);
        let r = snapshot(&f.to_string()).unwrap();
        assert_eq!(r.holdings[0].holding.value_cents, 8398);
        assert_eq!(r.holdings[0].holding.quantity, 1.0001);
    }

    #[test]
    fn empty_depot_is_valid_and_clears_positions() {
        let mut f = fixture();
        f["depots"][0]["items"] = json!([]);
        f["depots"][0]["value"] = json!(0.0);
        let r = snapshot(&f.to_string()).unwrap();
        assert!(r.holdings.is_empty());
        assert_eq!(r.holdings_fetched.len(), 1);
    }

    #[test]
    fn rejects_incomplete_or_inconsistent_snapshots() {
        let bad = |f: Value| assert!(snapshot(&f.to_string()).is_err(), "{f}");
        let mut f = fixture();
        f["version"] = json!(2);
        bad(f);
        let mut f = fixture();
        f["depots"][0]["value"] = json!(999.0); // does not match the funds
        bad(f);
        let mut f = fixture();
        f["depots"][0]["items"][0]["isin"] = json!("XX");
        bad(f);
        let mut f = fixture();
        f["depots"][0]["items"][0]["currency"] = json!("USD");
        bad(f);
        let mut f = fixture();
        f["depots"][0]["number"] = json!("12 34");
        bad(f);
        let mut f = fixture();
        f["depots"] = json!([]);
        bad(f);
        let mut f = fixture();
        f["depots"] = json!([f["depots"][0].clone(), f["depots"][0].clone()]);
        bad(f);
        assert!(snapshot("not json").is_err());
    }

    #[test]
    fn rejects_other_connector_kinds() {
        let dir = crate::test_support::TempDir::new("unioninvestment-other");
        let vault =
            Vault::open(dir.join("vault.db").to_string_lossy().into(), vec![7; 32]).unwrap();
        let id = vault
            .add_bank_connection(ConnectorKind::Quirion, "Andere".into(), "x".into())
            .unwrap();
        assert!(vault
            .clone()
            .import_unioninvestment_snapshot(id, fixture().to_string())
            .is_err());
    }
}
