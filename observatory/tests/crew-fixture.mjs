// A stand-in kernel for the Crew zone browser tests: GET /api/snapshot from memory, so the
// preview example (started with its address as the upstream) renders a snapshot the test edits.
import { createServer } from 'node:http';

const seat = (pid, project) => ({
  rank: pid === 1 ? 1 : 2, pid, project, state: 'live',
  occupied_by: { app: 'claude-desktop', harness: 'claude' },
  model: 'claude-opus-5-5', effort: 'high', source: 'transcript',
});

export const worker = (pid, parent, project, now, extra = {}) => ({
  pid, parent, project, intent: `Intent of ${pid}`, harness: 'claude', model: 'claude-sonnet-5-5',
  go_quote: 'Ok, go', done_when: 'Report cites sources',
  go_source: { kind: 'captain_prompt', thread: 'claude:fixture', prompt_id: 1, at: 1790900000000 },
  effort: 'high', elapsed_s: 90, state: 'working', ...(now ? { now } : {}), ...extra,
});

/** The snapshot: L1, "L2 brand" (PID 2) and "L2 travel" (PID 3) live, with these workers. */
export const snapshot = (workers) => ({
  at: Date.now(),
  kernel: { version: 't', uptime_s: 60, os_pid: 1, home: '/nowhere' },
  calls: [], quota: [],
  seats: [seat(1, null), seat(2, 'brand'), seat(3, 'travel')],
  workers,
});

/** Serves `state.workers` as the snapshot; returns { port, state, close }. */
export const startKernel = async (workers) => {
  const state = { workers };
  const server = createServer((req, res) => {
    if (req.url === '/api/snapshot') {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify(snapshot(state.workers)));
    } else {
      res.writeHead(404).end();
    }
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  return { port: server.address().port, state, close: () => new Promise((r) => server.close(r)) };
};
