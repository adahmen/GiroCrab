import SwiftUI
import WebKit
import GiroCrabCore

/// Union Investment customer portal (UnionDepot). Design page: `docs/unioninvestment-connector.md`.
///
/// The portal signs in through Keycloak (SMS code and captcha, done by the user in the visible
/// web view) and then calls a JSON API with a bearer token. The token stays in the page's
/// JavaScript closure, never in Swift, logs or the vault. Installed before the portal's first
/// authenticated request.
enum UnionInvestmentScripts {
    static let hook = #"""
    (() => {
      const pageOrigin = 'https://anlegen.meindepot.union-investment.de';
      if (location.origin !== pageOrigin || window.__gcUnion) return;
      const origin = 'https://service.meindepot.union-investment.de';
      const prefixes = ['/customer-functional-service/', '/anlageziel-functional-service/'];
      const nativeFetch = window.fetch;
      let authorization = null;
      const trusted = url => {
        try { const u = new URL(url, location.href);
          return u.origin === origin && prefixes.some(p => u.pathname.startsWith(p));
        } catch { return false; }
      };
      const remember = (url, headers) => {
        if (!trusted(url)) return;
        const value = new Headers(headers || {}).get('authorization');
        if (value && /^Bearer /i.test(value)) authorization = value;
      };
      window.fetch = function(input, init) {
        const url = input instanceof Request ? input.url : String(input);
        const headers = init?.headers || (input instanceof Request ? input.headers : {});
        const response = nativeFetch.apply(this, arguments);
        if (trusted(url)) response.then(r => { if (r.ok) remember(url, headers); }).catch(() => {});
        return response;
      };
      const open = XMLHttpRequest.prototype.open;
      const setHeader = XMLHttpRequest.prototype.setRequestHeader;
      const requests = new WeakMap();
      XMLHttpRequest.prototype.open = function(method, url) {
        const request = {url: String(url), headers: {}};
        requests.set(this, request);
        this.addEventListener('load', () => {
          if (this.status >= 200 && this.status < 300) remember(request.url, request.headers);
        }, {once:true});
        return open.apply(this, arguments);
      };
      XMLHttpRequest.prototype.setRequestHeader = function(name, value) {
        const request = requests.get(this);
        if (request && trusted(request.url)) request.headers[name] = value;
        return setHeader.apply(this, arguments);
      };
      // The goals endpoint is versioned through its media type; plain application/json answers 406.
      const GOALS = 'application/vv.anlageziel.v3+json';
      const json = async (path, accept = 'application/json, text/plain, */*') => {
        if (location.origin !== pageOrigin || !authorization)
          throw new Error('Union Investment: Bitte erneut anmelden.');
        if (!prefixes.some(p => path.startsWith(p)) || path.includes('://') || path.includes('..'))
          throw new Error('Union Investment: Ungültiger Abruf.');
        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), 30000);
        try {
          const r = await nativeFetch.call(window, origin + path, {
            method: 'GET', credentials: 'omit', redirect: 'error',
            headers: {'Authorization':authorization, 'Accept':accept}, signal:controller.signal
          });
          if (!r.ok) {
            if (r.status === 401 || r.status === 403) authorization = null;
            throw new Error('Union Investment: Abruf fehlgeschlagen (HTTP ' + r.status + '). Bitte erneut anmelden.');
          }
          return await r.json();
        } finally { clearTimeout(timeout); }
      };
      // Only the selected fields cross the bridge (no addresses, bank accounts, e-mail).
      window.__gcUnion = {
        ready: () => !!authorization,
        collect: async () => {
          const master = await json('/customer-functional-service/customer/depot/list/master-deposits');
          if (!Array.isArray(master.masterDeposits)) throw new Error('Union Investment: Keine Depots erkannt.');
          // The goals endpoint answers for the depot the portal has selected; with several
          // depots that selection is not verified yet, so refuse instead of mixing them up.
          if (master.masterDeposits.length !== 1)
            throw new Error('Union Investment: Mehrere Depots werden noch nicht unterstützt.');
          const d = master.masterDeposits[0];
          const goals = await json('/anlageziel-functional-service/depot/anlageziel', GOALS + ', application/json;q=0.5');
          if (!Array.isArray(goals)) throw new Error('Union Investment: Ungültige Depotübersicht.');
          const items = goals.map(g => {
            const f = g?.produkt?.einzelfonds;
            if (!f) throw new Error('Union Investment: Ein Produkt ohne Einzelfonds wird noch nicht unterstützt.');
            return {isin:f.isin, name:g.name?.vergeben || g.name?.gewaehlt, quantity:f.stueckanzahl,
              price:f.ruecknahmePreis?.value, priceDate:f.ruecknahmePreis?.date, currency:f.ruecknahmePreis?.currency,
              value:g.betrag?.aktuellerWert, unrealized:g.gewinnUndVerlust?.realisierbarerGewinnUndVerlust};
          });
          return JSON.stringify({version:1, asOf:new Date().toISOString().slice(0,10), depots:[{
            number:d.daskaStammdepotNr, owner:d.depotOwnerDisplayName, name:d.depotTypeDisplayName,
            value:d.depotValue?.value, items}]});
        }
      };
    })();
    """#

    static let probe = #"""
    location.origin === 'https://anlegen.meindepot.union-investment.de' && !!window.__gcUnion?.ready()
    """#
}

struct UnionInvestmentWebSheet: View {
    @Environment(\.reportRedaction) private var redaction
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let connection: BankConnection
    @State private var browser = PortalBrowser(url: URL(string: "https://anlegen.meindepot.union-investment.de/dashboard/super")!,
        domains: PortalDomains(kind: .unionInvestment), captureTraffic: false, userScript: UnionInvestmentScripts.hook)
    @State private var uiDelegate = AmazonUIDelegate()
    @State private var ready = false
    @State private var running = false
    @State private var notice: String?
    @State private var task: Task<Void, Never>?

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 3) {
                    Text("\(redaction.masking(connection.displayName)) – Union Investment").appFont(.headline)
                    if running { Text("Depot und Positionen abrufen …").appFont(.caption) }
                    else if let notice { Text(notice).appFont(.caption).foregroundStyle(Theme.warning) }
                    else { Text(ready ? String(localized: "Angemeldet – das Depot kann abgerufen werden.") : String(localized: "Melden Sie sich bei Union Investment an und bestätigen Sie den SMS-Code.")).appFont(.caption) }
                }
                Spacer()
                if running || browser.loading { ProgressView().controlSize(.small) }
            }.padding(14)
            Divider()
            PortalWebView(browser: browser).macFrame(minWidth: 760, minHeight: 620)
            Divider()
            HStack {
                Button("Schließen", role: .cancel) { task?.cancel(); close() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button("Depot abrufen") { start() }.buttonStyle(.borderedProminent).disabled(!ready || running)
            }.padding(12)
        }
        .webPageDialogs(uiDelegate)
        .task {
            browser.webView.uiDelegate = uiDelegate
            while !Task.isCancelled {
                let authenticated = (try? await browser.webView.evaluateJavaScript(UnionInvestmentScripts.probe)) as? Bool ?? false
                let ok = browser.onPortal && authenticated
                if ready != ok { ready = ok }
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
        .onDisappear { task?.cancel(); browser.teardown() }
        .interactiveDismissDisabled(running)
    }

    private func run(_ script: String) async throws -> String {
        try Task.checkCancellation()
        let guardScript = "if (location.origin !== 'https://anlegen.meindepot.union-investment.de' || !window.__gcUnion) throw new Error('Union Investment: Bitte erneut anmelden.');\n"
        let result: Any?
        do {
            result = try await browser.webView.callAsyncJavaScript(guardScript + script, arguments: [:], in: nil, contentWorld: .page)
        } catch {
            try Task.checkCancellation()
            // The page script's own message (e.g. "… HTTP 401 …"); WebKit's generic text
            // says nothing. Text that carries a URL is never shown (it can hold tokens).
            let message = ((error as NSError).userInfo["WKJavaScriptExceptionMessage"] as? String) ?? ""
            if !message.isEmpty, message.count <= 160, !message.contains("://") {
                throw UnionInvestmentError.portal(message)
            }
            throw UnionInvestmentError.unreadable
        }
        try Task.checkCancellation()
        guard let string = result as? String else { throw UnionInvestmentError.unreadable }
        return string
    }

    private func start() {
        running = true
        notice = nil
        task = Task {
            guard let pin = model.pinVault() else { running = false; return }
            let connectionId = connection.id
            do {
                let snapshot = try await run("return await window.__gcUnion.collect();")
                let summary = try await pin.run { try $0.importUnioninvestmentSnapshot(connectionId: connectionId, snapshotJson: snapshot) }
                model.toast = String(localized: "Union Investment: \(summary.accountsUpdated) Depots aktualisiert.")
                close()
            } catch is CancellationError {
            } catch { notice = describe(error) }
            await pin.checkpoint()
            running = false
            model.reloadAll()
            // accounts another login already delivers wait for a choice (#346)
            model.checkPendingAccountSelection()
        }
    }

    private func close() { model.webLoginConnection = nil; dismiss() }
}

private enum UnionInvestmentError: LocalizedError {
    case unreadable
    case portal(String)
    var errorDescription: String? {
        switch self {
        case .unreadable: return String(localized: "Union Investment: Die Portalantwort konnte nicht gelesen werden.")
        case .portal(let message): return message
        }
    }
}
