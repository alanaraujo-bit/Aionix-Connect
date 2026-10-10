// POST /api/ingest — called by aionix-telemetry on the server with
// "Authorization: Bearer $INGEST_TOKEN". Anything else gets 401.
import { timingSafeEqual } from 'node:crypto';
import { writeSnapshot } from '../lib/store.js';

const MAX_BYTES = 256 * 1024;

function authorized(request) {
  const expected = process.env.INGEST_TOKEN ?? '';
  const got = (request.headers.get('authorization') ?? '').replace(/^Bearer\s+/i, '');
  if (expected.length < 32 || got.length !== expected.length) return false;
  return timingSafeEqual(Buffer.from(got), Buffer.from(expected));
}

export async function POST(request) {
  if (!authorized(request)) return new Response('unauthorized', { status: 401 });
  const body = await request.text();
  if (body.length > MAX_BYTES) return new Response('too large', { status: 413 });
  let snap;
  try { snap = JSON.parse(body); } catch { return new Response('bad json', { status: 400 }); }
  if (typeof snap !== 'object' || snap === null || typeof snap.ts !== 'number') {
    return new Response('bad snapshot', { status: 400 });
  }
  // Server clocks drift; the dashboard measures freshness by when we received it.
  snap.receivedAt = Date.now();
  await writeSnapshot(snap);
  return new Response(null, { status: 204 });
}
