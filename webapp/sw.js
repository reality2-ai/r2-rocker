// r2-rocker webapp service worker.
//
// Stale-while-revalidate for static assets so the dashboard's
// `index.html` + WASM bundle don't have to round-trip through the
// network on every page load. WebSocket connections and `/api/`
// requests bypass the worker entirely.
//
// To force every connected browser to refresh the cache, bump CACHE.

const CACHE = 'r2-rocker-v4';
const PRECACHE = [
  '/',
  '/index.html',
  '/pkg/r2_wasm.js',
  '/pkg/r2_wasm_bg.wasm',
];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE).then((cache) => cache.addAll(PRECACHE))
  );
  // Activate the new worker the moment it finishes installing rather
  // than waiting for every existing tab to close.
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)))
    )
  );
  self.clients.claim();
});

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);

  // Dynamic endpoints — never cache; let the network handle them.
  if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/ws/')) {
    return;
  }

  // Only handle same-origin GETs. Cross-origin (e.g. chart.js CDN) and
  // POSTs go straight to network.
  if (event.request.method !== 'GET' || url.origin !== location.origin) {
    return;
  }

  event.respondWith((async () => {
    const cache = await caches.open(CACHE);
    const cached = await cache.match(event.request);
    const network = fetch(event.request)
      .then((resp) => {
        if (resp && resp.ok) cache.put(event.request, resp.clone());
        return resp;
      })
      .catch(() => null);
    return cached || (await network) || new Response('offline', { status: 503 });
  })());
});
