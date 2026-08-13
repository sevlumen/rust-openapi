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

const cases = {
  plaintext: { method: 'get', path: '/plaintext', expected: 200, body: (body) => body === 'OK' },
  'static-json': { method: 'get', path: '/json-static', expected: 200, body: (body) => body === '{"id":1,"name":"Alice"}' },
  'static-route': { method: 'get', path: '/fixed/path', expected: 200, body: (body) => body === 'OK' },
  'path-integer': { method: 'get', path: '/users/123456', expected: 200, body: (body) => body === '123456' },
  'path-uuid': { method: 'get', path: '/uuid/550e8400-e29b-41d4-a716-446655440000', expected: 200, body: (body) => body === '550e8400-e29b-41d4-a716-446655440000' },
  'validation-success': { method: 'get', path: '/validation-success/42', expected: 200, body: (body) => body === '{"name":"valid-42"}' },
  problem: { method: 'get', path: '/problem', expected: 400, body: (body) => body.includes('"detail":"invalid request"') },
  'raw-handler': { method: 'get', path: '/raw-handler', expected: 200, body: (body) => body === 'OK' },
  security: { method: 'get', path: '/secure', headers: { 'X-API-Key': 'abc-secret' }, expected: 200, body: (body) => body === 'authorized' },
  query: { method: 'get', path: '/search?page=42&active=true', expected: 200, body: (body) => body === '42:true' },
  header: { method: 'get', path: '/trace', headers: { 'X-Trace-ID': 'abc123' }, expected: 200, body: (body) => body === 'abc123' },
  'json-small': { method: 'get', path: '/json-small', expected: 200, body: (body) => body === '{"id":1,"name":"Alice"}' },
  'json-100-users': { method: 'get', path: '/users', expected: 200, body: (body) => body.includes('"User 100"') },
  postgres: { method: 'get', path: '/users-db', expected: 200, body: (body) => body.includes('user100@example.test') },
  '404': { method: 'get', path: '/missing', expected: 404, body: (body) => body === 'Not Found' },
  '405': { method: 'post', path: '/plaintext', expected: 405, body: (body) => body === '' },
};

function send(selected) {
  return selected.method === 'post'
    ? http.post(`${target}${selected.path}`, null, { headers: selected.headers || {} })
    : http.get(`${target}${selected.path}`, { headers: selected.headers || {} });
}

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
  send(cases[benchmarkCase] || cases.plaintext);
}

export function measured() {
  const selected = cases[benchmarkCase] || cases.plaintext;
  const response = send(selected);
  measuredRequests.add(1);
  measuredDuration.add(response.timings.duration);
  const checks = {
    [`${benchmarkCase} status is expected`]: (value) => value.status === selected.expected,
    [`${benchmarkCase} body is equivalent`]: (value) => selected.body(value.body),
  };
  if (!check(response, checks)) {
    measuredErrors.add(1);
  }
}
