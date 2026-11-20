# Union Investment browser connector

<!-- sync:connector-note:start -->
> **Hinweis / Note:** Dieser Connector ist ein unabhängiges Community-Projekt und steht in keiner Verbindung zur Union Investment. Schnittstelle: inoffiziell. Bei inoffiziellen Schnittstellen kann die Nutzung gegen die Nutzungsbedingungen der Bank verstoßen.
> This connector is an independent community project and is not affiliated with Union Investment. Interface: unofficial. Using unofficial interfaces may violate the bank's terms of service.
<!-- sync:connector-note:end -->

Sign in through **Add bank → Union Investment** (UnionDepot). Login, SMS code and
captcha happen in the embedded browser. After login, choose **Depot abrufen**.

This first version reads the **depot value and the fund positions** of a single
depot. Booked transactions and the postbox are planned as separate steps.

The connector uses the portal's internal API, not FinTS or Enable Banking.
It creates one securities account per depot. The portal lists one entry per
investment goal; goals holding the same fund are combined into one position
(quantity, value and gain are summed). A position shows quantity, redemption
price and value. The portal reports no purchase price: the connector derives it
as (value − unrealized gain) ÷ quantity and the unrealized gain from the
portal's own `realisierbarerGewinnUndVerlust` figure. Realized gains and tax
figures are not imported. Only EUR funds are accepted; anything else is
rejected rather than guessed.

## Browser flow and technical validation

The portal signs in with OpenID Connect (Keycloak, PKCE) and then calls a JSON
API with a bearer token. The token is captured from the portal's own successful
requests and stays in a JavaScript closure; Swift receives only the selected
depot and position fields. Passwords, tokens, addresses, bank accounts and the
e-mail address are not exported or logged. The generic portal traffic recorder
is disabled for this connector.

Base URL: `https://service.meindepot.union-investment.de`.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/customer-functional-service/customer/depot/list/master-deposits` | Depots, owner, depot value |
| GET | `/anlageziel-functional-service/depot/anlageziel` | Funds per goal: ISIN, quantity, redemption price, value, gain |

Consistency checks before anything is stored: the sum of the positions must
match the depot value (1 ct tolerance per position), ISINs must be well formed,
and every price date must be a valid date. A failed check imports nothing.

## Limits

- **One depot per login.** The goals endpoint answers for the depot selected in
  the portal; with several depots the connector stops with a message instead of
  mixing them up.
- Products without a single fund (portfolio products) are not supported yet.
- The embedded browser is needed on every fetch (SMS code); there is no
  background sync.
- The internal API is not documented and can change without notice.
