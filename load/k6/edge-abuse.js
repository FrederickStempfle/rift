import http from 'k6/http';
import { check, sleep } from 'k6';
import { Counter, Rate } from 'k6/metrics';

const TARGET_URL = __ENV.TARGET_URL || 'https://127.0.0.1';
const TARGET_HOST = __ENV.TARGET_HOST || 'rift.atrainbots.com';

const rateLimited = new Rate('rate_limited_ratio');
const hardBlocked = new Counter('hard_blocked_total');

export const options = {
  insecureSkipTLSVerify: true,
  scenarios: {
    steady: {
      executor: 'constant-vus',
      vus: Number(__ENV.STEADY_VUS || 80),
      duration: __ENV.STEADY_DURATION || '2m',
      exec: 'steadyTraffic',
    },
    burst: {
      executor: 'ramping-arrival-rate',
      startRate: Number(__ENV.BURST_START_RPS || 300),
      timeUnit: '1s',
      preAllocatedVUs: Number(__ENV.BURST_PREALLOCATED_VUS || 200),
      maxVUs: Number(__ENV.BURST_MAX_VUS || 800),
      stages: [
        { target: Number(__ENV.BURST_PEAK_RPS || 2000), duration: __ENV.BURST_RAMP_UP || '30s' },
        { target: Number(__ENV.BURST_PEAK_RPS || 2000), duration: __ENV.BURST_HOLD || '45s' },
        { target: Number(__ENV.BURST_FLOOR_RPS || 400), duration: __ENV.BURST_RAMP_DOWN || '20s' },
      ],
      exec: 'burstTraffic',
    },
  },
  thresholds: {
    http_req_failed: ['rate<0.03'],
    'http_req_duration{scenario:steady}': ['p(95)<800'],
    'http_req_duration{scenario:burst}': ['p(95)<1200'],
    rate_limited_ratio: ['rate<0.75'],
  },
};

function makeRequest(path) {
  const res = http.get(`${TARGET_URL}${path}`, {
    headers: {
      Host: TARGET_HOST,
      'User-Agent': 'rift-load-gate/1.0',
      Accept: 'text/html,application/json',
    },
    redirects: 0,
    timeout: __ENV.REQUEST_TIMEOUT || '8s',
  });

  const ok = check(res, {
    'response status is expected': (r) => [200, 301, 302, 404, 429, 503].includes(r.status),
  });

  const limited = res.status === 429;
  rateLimited.add(limited);
  if (limited) {
    hardBlocked.add(1);
  }

  if (!ok) {
    console.error(`unexpected status=${res.status} body=${String(res.body).slice(0, 120)}`);
  }

  return res;
}

export function steadyTraffic() {
  makeRequest('/');
  sleep(0.05);
}

export function burstTraffic() {
  makeRequest('/');
}
