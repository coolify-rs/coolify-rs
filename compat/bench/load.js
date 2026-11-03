// k6 load for one endpoint: VUS virtual users, each with its own keep-alive connection, for DURATION.
// Each user keeps one of TOKENS (comma-separated), so the API is used by several clients at once.
import http from 'k6/http';
import { check } from 'k6';

const tokens = __ENV.TOKENS.split(',');
const url = __ENV.BASE + __ENV.ENDPOINT;

export const options = {
  vus: Number(__ENV.VUS),
  duration: __ENV.DURATION,
  discardResponseBodies: true,
  summaryTrendStats: ['avg', 'med', 'p(95)', 'p(99)', 'max'],
};

export default function () {
  const token = tokens[__VU % tokens.length];
  const r = http.get(url, { headers: { Authorization: `Bearer ${token}`, Accept: 'application/json' } });
  check(r, { 'status is 200': (res) => res.status === 200 });
}
