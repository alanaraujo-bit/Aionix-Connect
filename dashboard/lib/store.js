// Latest snapshot + short history, kept in Vercel's Runtime Cache (regional, ephemeral:
// the server re-sends every few seconds, so losing it only blanks the page briefly).
// Outside Vercel (vercel dev without the cache) it falls back to process memory.
import { getCache } from '@vercel/functions';

const KEY_LATEST = 'latest';
const KEY_HISTORY = 'history';
export const HISTORY_POINTS = 360; // 1 h at 10 s

const mem = new Map();
let cache;
function store() {
  if (cache === undefined) {
    try { cache = getCache({ namespace: 'aionix-status' }); } catch { cache = null; }
  }
  return cache;
}

async function get(key) {
  const c = store();
  if (c) { try { return await c.get(key); } catch { /* fall through */ } }
  return mem.get(key);
}

async function set(key, value) {
  mem.set(key, value);
  const c = store();
  if (c) await c.set(key, value, { ttl: 7 * 24 * 3600, tags: ['aionix-status'] });
}

export async function readState() {
  const [latest, history] = await Promise.all([get(KEY_LATEST), get(KEY_HISTORY)]);
  return { latest: latest ?? null, history: history ?? [] };
}

export async function writeSnapshot(snap) {
  const history = (await get(KEY_HISTORY)) ?? [];
  const n = snap.net ?? {};
  history.push({
    t: snap.ts,
    cpu: snap.cpu,
    mem: snap.mem?.total ? Math.round((1000 * snap.mem.used) / snap.mem.total) / 10 : null,
    rx: Math.round(n.rx ?? 0),
    tx: Math.round(n.tx ?? 0),
    load: snap.load?.[0] ?? null,
    temp: snap.temp ?? null,
  });
  const trimmed = history.slice(-HISTORY_POINTS);
  await Promise.all([set(KEY_LATEST, snap), set(KEY_HISTORY, trimmed)]);
}
