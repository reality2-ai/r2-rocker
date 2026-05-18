// r2-rocker webapp service worker — TEMPORARILY a kill-switch.
//
// A stuck v15/v16 SW state was intercepting `fetch()`s and returning
// "Failed to fetch" for every /api/* call, while leaving WebSocket
// connects working. The simplest unstick is to make this SW
// uninstall itself + drop every cache the moment it activates, then
// navigate every controlling client to reload from a clean slate.
//
// Once every browser has reloaded with a clean state, replace this
// file with the proper stale-while-revalidate SW (see git history
// for the v16 version).

self.addEventListener('install', (event) => {
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil((async () => {
    // Drop every named cache we ever created.
    const keys = await caches.keys();
    await Promise.all(keys.map((k) => caches.delete(k)));
    // Stop intercepting anything from now on.
    try { await self.registration.unregister(); } catch (_) {}
    // Force every controlled tab to reload without the SW so the
    // operator's existing window comes back to life.
    const clients = await self.clients.matchAll({ includeUncontrolled: true });
    for (const c of clients) {
      try { c.navigate(c.url); } catch (_) {}
    }
  })());
});

// Never intercept anything else — let the network do its job.
// (No `fetch` listener intentionally.)
