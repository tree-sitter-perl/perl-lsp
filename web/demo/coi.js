// GitHub Pages cannot send the two headers that make a page cross-origin
// isolated, and the server's threads need isolation to share memory. This
// service worker adds the headers to every response it relays; the page
// registers it and reloads once so the document itself comes through it.
if (typeof window === 'undefined') {
  self.addEventListener('install', () => self.skipWaiting());
  self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));
  self.addEventListener('fetch', event => {
    const request = event.request;
    if (request.cache === 'only-if-cached' && request.mode !== 'same-origin') return;
    event.respondWith(fetch(request).then(response => {
      if (response.status === 0) return response;
      const headers = new Headers(response.headers);
      headers.set('Cross-Origin-Opener-Policy', 'same-origin');
      headers.set('Cross-Origin-Embedder-Policy', 'require-corp');
      return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
    }));
  });
} else if (!crossOriginIsolated && 'serviceWorker' in navigator) {
  // The session flag stops a reload loop where isolation can never arrive
  // (a browser without it, or a frame that forbids it).
  navigator.serviceWorker.register(document.currentScript.src).then(async () => {
    await navigator.serviceWorker.ready;
    if (!sessionStorage.getItem('coi-reloaded')) {
      sessionStorage.setItem('coi-reloaded', '1');
      location.reload();
    }
  });
}
