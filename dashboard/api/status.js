// GET /api/status — public, read-only. The CDN absorbs viewer polling (one function
// call per region every few seconds at most, however many people are watching).
import { readState } from '../lib/store.js';

export async function GET() {
  const { latest, history } = await readState();
  return Response.json(
    { now: Date.now(), latest, history },
    {
      headers: {
        'Cache-Control': 'public, max-age=0, must-revalidate',
        'CDN-Cache-Control': 'public, max-age=4, stale-while-revalidate=10',
        'Access-Control-Allow-Origin': '*',
      },
    },
  );
}
