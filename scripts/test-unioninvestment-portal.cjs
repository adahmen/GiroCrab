// Run the actual scripts embedded in UnionInvestmentUI.swift (synthetic data only), as for quirion.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const source = readFileSync(new URL('../GiroCrabApp/Sources/GiroCrabUI/UnionInvestmentUI.swift', `file://${__filename}`), 'utf8');
const hook = source.match(/static let hook = #"""\n([\s\S]*?)\n    """#/)[1];
const api = 'https://service.meindepot.union-investment.de';
const pageOrigin = 'https://anlegen.meindepot.union-investment.de';
const response = (data, status=200) => ({ok: status >= 200 && status < 300, status, json: async () => data});
function page(fetch, origin=pageOrigin) {
  class XHR { open() {} setRequestHeader() {} addEventListener() {} }
  const window = {fetch};
  const context = vm.createContext({window, location:{origin, href:origin+'/'}, Request, Headers, URL,
    XMLHttpRequest:XHR, AbortController, setTimeout, clearTimeout, Date});
  vm.runInContext(hook, context);
  return window;
}
async function authenticate(window) {
  await window.fetch(api+'/customer-functional-service/customer', {headers:{Authorization:'Bearer synthetic-test-token'}});
  await Promise.resolve();
}
const goal = (isin, name, shares, price, value, gain) => ({
  name:{vergeben:name, gewaehlt:'DO NOT EXPORT'}, betrag:{aktuellerWert:value},
  gewinnUndVerlust:{realisierbarerGewinnUndVerlust:gain}, bild:{dateiname:'DO NOT EXPORT'},
  produkt:{einzelfonds:{isin, stueckanzahl:shares, ruecknahmePreis:{value:price, date:'2026-11-19', currency:'EUR'}}}});
function server(calls, masters=1) {
  return async (url, options={}) => {
    calls.push({url, options});
    const path = new URL(url).pathname;
    if (path.endsWith('/depot/list/master-deposits')) return response({masterDeposits:Array.from({length:masters}, () => ({
      daskaStammdepotNr:'123456789', depotOwnerDisplayName:'Test Owner', depotTypeDisplayName:'UnionDepot',
      depotValue:{currency:'EUR', value:250}, links:{baseUrl:'DO NOT EXPORT'}}))});
    if (path.endsWith('/depot/anlageziel')) return response([
      goal('DE0000000001', 'Test Fonds A', 2.5, 40, 100, 10), goal('DE0000000002', 'Test Fonds B', 10, 15, 150, -3)]);
    if (path.endsWith('/customer')) return response({email:'DO NOT EXPORT'});
    throw new Error('Unexpected request ' + url);
  };
}

test('collects depot and funds; tokens and unrelated fields stay inside the page', async () => {
  const calls = [], w = page(server(calls));
  assert.equal(w.__gcUnion.ready(), false);
  await authenticate(w);
  assert.equal(w.__gcUnion.ready(), true);
  calls.length = 0;
  const snapshot = await w.__gcUnion.collect(), data = JSON.parse(snapshot);
  assert.equal(data.version, 1);
  assert.match(data.asOf, /^\d{4}-\d{2}-\d{2}$/);
  assert.equal(data.depots.length, 1);
  assert.equal(data.depots[0].number, '123456789');
  assert.equal(data.depots[0].value, 250);
  assert.deepEqual(data.depots[0].items.map(i => [i.isin, i.name, i.quantity, i.price, i.value, i.unrealized, i.currency]),
    [['DE0000000001', 'Test Fonds A', 2.5, 40, 100, 10, 'EUR'], ['DE0000000002', 'Test Fonds B', 10, 15, 150, -3, 'EUR']]);
  assert(!snapshot.includes('DO NOT EXPORT'));
  assert(!snapshot.includes('synthetic-test-token'));
  for (const c of calls) {
    assert.equal(c.options.method, 'GET');
    assert.equal(c.options.credentials, 'omit');
    assert.equal(c.options.redirect, 'error');
    assert.equal(c.options.headers.Authorization, 'Bearer synthetic-test-token');
  }
  // the goals endpoint is versioned through its media type (plain application/json answers 406)
  const goals = calls.find(c => c.url.endsWith('/depot/anlageziel'));
  assert.match(goals.options.headers.Accept, /^application\/vv\.anlageziel\.v3\+json/);
});

test('only successful bearer requests to the real API establish authorization', async () => {
  const w = page(async () => response({}, 200));
  await w.fetch('https://service.meindepot.union-investment.de.evil.example/customer-functional-service/customer', {headers:{authorization:'Bearer bad'}});
  assert.equal(w.__gcUnion.ready(), false);
  // the Keycloak endpoints on the same host never count
  await w.fetch(api + '/auth/realms/union-investment/protocol/openid-connect/token', {headers:{authorization:'Bearer bad'}});
  assert.equal(w.__gcUnion.ready(), false);
  // a non-bearer scheme is ignored
  await w.fetch(api + '/customer-functional-service/customer', {headers:{authorization:'Basic abc'}});
  assert.equal(w.__gcUnion.ready(), false);
  const failed = page(async () => response({}, 401));
  await authenticate(failed);
  assert.equal(failed.__gcUnion.ready(), false);
  assert.equal(page(server([]), 'https://evil.example').__gcUnion, undefined);
});

test('expired sessions, several depots and unsupported products cannot produce a snapshot', async () => {
  let fail = false;
  const base = server([]);
  const w = page(async (url, options) => fail ? response({}, 401) : base(url, options));
  await authenticate(w); fail = true;
  await assert.rejects(w.__gcUnion.collect(), /HTTP 401/);
  assert.equal(w.__gcUnion.ready(), false);
  const several = page(server([], 2));
  await authenticate(several);
  await assert.rejects(several.__gcUnion.collect(), /Mehrere Depots/);
  const portfolio = page(async (url, options) => url.endsWith('/depot/anlageziel') ? response([{produkt:{}}]) : base(url, options));
  await authenticate(portfolio);
  await assert.rejects(portfolio.__gcUnion.collect(), /ohne Einzelfonds/);
});
