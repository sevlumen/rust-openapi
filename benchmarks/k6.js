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

export const options = {
  summaryTrendStats: ['avg', 'min', 'med', 'max', 'p(90)', 'p(95)', 'p(99)'],
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
  const response = http.get(`${target}/plaintext`);
  measuredRequests.add(1);
  measuredDuration.add(response.timings.duration);
  if (!check(response, { 'status is 200': (value) => value.status === 200 })) {
    measuredErrors.add(1);
  }
}
