import http from 'k6/http';
import { Counter, Trend } from 'k6/metrics';
import { check } from 'k6';

const target = __ENV.TARGET_URL || 'http://oas:8080';
const iterations = Number(__ENV.ITERATIONS || 1000000);
const vus = Number(__ENV.VUS || 32);
const warmupSeconds = Number(__ENV.WARMUP_SECONDS || 30);
const measuredRequests = new Counter('measured_requests');
const measuredErrors = new Counter('measured_errors');
const measuredDuration = new Trend('measured_http_req_duration', true);
const mode = __ENV.MODE || 'measured';
const benchmarkCase = __ENV.CASE || 'plaintext';

export const options = {
  summaryTrendStats: ['avg', 'min', 'med', 'max', 'p(90)', 'p(95)', 'p(99)', 'p(99.9)'],
  scenarios: mode === 'warmup'
    ? { warmup: { executor: 'constant-vus', vus, duration: `${warmupSeconds}s`, exec: 'warmup' } }
    : { requests: { executor: 'shared-iterations', vus, iterations, maxDuration: '30m', exec: 'measured' } },
  thresholds: {
    ...(mode === 'measured' ? { measured_errors: ['count==0'] } : {}),
  },
};

export function warmup() {
  http.get(`${target}/plaintext`);
}

export function measured() {
  const cases = {
    plaintext: { method: 'get', path: '/plaintext', expected: 200 },
    'static-json': { method: 'get', path: '/json-static', expected: 200 },
    'static-route': { method: 'get', path: '/fixed/path', expected: 200 },
    'path-integer': { method: 'get', path: '/users/123456', expected: 200 },
    'path-uuid': { method: 'get', path: '/uuid/550e8400-e29b-41d4-a716-446655440000', expected: 200 },
    query: { method: 'get', path: '/search?page=42&active=true', expected: 200 },
    header: { method: 'get', path: '/trace', headers: { 'X-Trace-ID': 'abc123' }, expected: 200 },
    'json-small': { method: 'get', path: '/json-small', expected: 200 },
    'json-100-users': { method: 'get', path: '/users', expected: 200 },
    postgres: { method: 'get', path: '/users-db', expected: 200 },
    '404': { method: 'get', path: '/missing', expected: 404 },
    '405': { method: 'post', path: '/plaintext', expected: 405 },
  };
  const selected = cases[benchmarkCase] || cases.plaintext;
  const response = selected.method === 'post'
    ? http.post(`${target}${selected.path}`, null, { headers: selected.headers || {} })
    : http.get(`${target}${selected.path}`, { headers: selected.headers || {} });
  measuredRequests.add(1);
  measuredDuration.add(response.timings.duration);
  if (!check(response, { [`${benchmarkCase} status is expected`]: (value) => value.status === selected.expected })) {
    measuredErrors.add(1);
  }
}
