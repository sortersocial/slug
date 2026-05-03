/**
 * Slug web UI: only plumbing — fetch/eval/SSE. No product UI logic here.
 */
(function () {
  function evalJs(js) {
    if (js && String(js).trim()) {
      eval(js);
    }
  }

  // Theme cookie sync (runs before paint; full reload if localStorage disagrees with cookie)
  try {
    var ls = localStorage.getItem('slug-theme');
    if (ls) {
      var m = document.cookie.match(/(?:^|;\s*)slug-theme=([^;]+)/);
      var c = m ? decodeURIComponent(m[1].replace(/\+/g, ' ')) : '';
      if (ls === c) {
        localStorage.removeItem('slug-theme');
      } else {
        document.cookie =
          'slug-theme=' + encodeURIComponent(ls) + '; Path=/; SameSite=Lax; Max-Age=31536000';
        location.reload();
      }
    }
  } catch (e) {}

  function initSlugUi() {
    // Spread control (client-only preference; not routed through /ui)
    var slider = document.getElementById('spread-slider');
    if (slider) {
      var storedSpread = localStorage.getItem('slug-spread');
      function setSpread(value) {
        document.documentElement.style.setProperty('--spread', value);
        localStorage.setItem('slug-spread', value);
      }
      if (storedSpread !== null) {
        slider.value = storedSpread;
        setSpread(parseFloat(storedSpread));
      }
      slider.addEventListener('input', function () {
        setSpread(parseFloat(this.value));
      });
    }

    // POST forms → eval response (except theme + full-navigation forms)
    document.addEventListener('submit', async function (e) {
      var f = e.target;
      if (!f || f.tagName !== 'FORM') return;
      if ((f.method || 'get').toLowerCase() !== 'post') return;
      if (f.id === 'slug-theme-form') return;
      if (f.id === 'vote-compare-form') return;
      if (f.getAttribute('data-navigate') === 'full') return;
      e.preventDefault();
      var resp = await fetch(f.action, {
        method: 'POST',
        body: new URLSearchParams(new FormData(f)),
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        credentials: 'same-origin',
      });
      evalJs(await resp.text());
    });

    // Debounced DSL check: POST /ui or data-check-action, eval body
    var checkTimers = new WeakMap();
    async function runFormCheck(form) {
      var action = form.getAttribute('data-check-action');
      if (!action) return;
      var fd = new URLSearchParams(new FormData(form));
      var checkRpc = form.getAttribute('data-check-rpc');
      if (checkRpc) fd.set('__rpc__', checkRpc);
      var resp = await fetch(action, {
        method: 'POST',
        body: fd,
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        credentials: 'same-origin',
      });
      evalJs(await resp.text());
    }
    document.addEventListener('input', function (e) {
      var target = e.target;
      if (!target || !target.closest) return;
      var form = target.closest('form[data-check-action]');
      if (!form) return;
      if (!(target.tagName === 'INPUT' || target.tagName === 'TEXTAREA')) return;
      var prev = checkTimers.get(form);
      if (prev) clearTimeout(prev);
      var handle = setTimeout(function () {
        runFormCheck(form).catch(function (err) {
          console.warn('slug form check failed', err);
        });
      }, 250);
      checkTimers.set(form, handle);
    });

    // Search: debounced GET fragment → Idiomorph (HTML only; not JS)
    var si = document.getElementById('search-input');
    if (si) {
      var st;
      si.addEventListener('input', function () {
        clearTimeout(st);
        st = setTimeout(async function () {
          var q = si.value.trim();
          var el = document.getElementById('search-results');
          if (!el) return;
          if (q.length < 2) {
            el.innerHTML = '';
            return;
          }
          var r = await fetch('/search/results?q=' + encodeURIComponent(q));
          Idiomorph.morph(el, await r.text());
        }, 150);
      });
      document.addEventListener('keydown', function (e) {
        if (e.key === '/' && document.activeElement !== si) {
          e.preventDefault();
          si.focus();
        }
      });
    }

    // Editor page: debounced POST /try/check → eval
    var ta = document.getElementById('editor-input');
    if (ta) {
      var status = document.getElementById('editor-status');
      var timer;
      ta.addEventListener('input', function () {
        clearTimeout(timer);
        if (status) status.textContent = 'checking…';
        timer = setTimeout(function () {
          var text = ta.value.trim();
          if (text.length < 3) {
            if (status) status.textContent = 'type to check…';
            var er = document.getElementById('editor-results');
            if (er) er.innerHTML = '';
            return;
          }
          fetch('/try/check', {
            method: 'POST',
            headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
            body: 'text=' + encodeURIComponent(text),
          })
            .then(function (r) {
              return r.text();
            })
            .then(evalJs)
            .catch(function (e) {
              if (status) status.textContent = 'error: ' + e;
            });
        }, 400);
      });
    }

    // Garden pin: POST /ui + __rpc__ `set_garden_pin` → 303 + Set-Cookie (same entrypoint as other UI actions)
    async function postUiRedirect(rpcObj) {
      var fd = new URLSearchParams();
      fd.set('__rpc__', JSON.stringify(rpcObj));
      var resp = await fetch('/ui', {
        method: 'POST',
        body: fd,
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        credentials: 'same-origin',
        redirect: 'manual',
      });
      if (resp.status === 303 || resp.status === 302 || resp.status === 301) {
        var loc = resp.headers.get('Location');
        if (loc) {
          window.location.assign(loc);
          return;
        }
      }
      console.warn('slug UI redirect: unexpected response', resp.status);
    }

    document.addEventListener('click', function (e) {
      var t = e.target;
      if (!t || !t.closest) return;
      var pinBtn = t.closest('button.ont-pin-btn');
      if (!pinBtn || pinBtn.getAttribute('data-unpin') === '1') return;
      var storage = pinBtn.getAttribute('data-item-storage');
      if (!storage) return;
      e.preventDefault();
      var shell = pinBtn.closest('.ont-item-pin-zone');
      var room = (shell && shell.getAttribute('data-garden-room')) || '';
      if (!room && document.body) {
        room = document.body.getAttribute('data-garden-room') || '';
      }
      if (!room) return;
      var next = window.location.pathname + window.location.search;
      postUiRedirect({
        action: 'set_garden_pin',
        clear: false,
        room_wire: room,
        item_storage: storage,
        next: next,
      }).catch(function (err) {
        console.warn('slug pin failed', err);
      });
    });

    document.addEventListener('click', function (e) {
      var t = e.target;
      if (!t || !t.closest) return;
      var unpin = t.closest('button.ont-pin-btn[data-unpin="1"]');
      if (!unpin) return;
      e.preventDefault();
      var next = window.location.pathname + window.location.search;
      postUiRedirect({
        action: 'set_garden_pin',
        clear: true,
        room_wire: '',
        next: next,
      }).catch(function (err) {
        console.warn('slug unpin failed', err);
      });
    });

    document.addEventListener('click', function (e) {
      var t = e.target;
      if (!t || !t.closest) return;
      var pinIco = t.closest('button.ont-garden-pin-ico');
      if (!pinIco) return;
      var storage = pinIco.getAttribute('data-item-storage');
      if (!storage) return;
      e.preventDefault();
      var zone = pinIco.closest('.ont-garden-child-actions');
      var room = (zone && zone.getAttribute('data-garden-room')) || '';
      if (!room && document.body) room = document.body.getAttribute('data-garden-room') || '';
      if (!room) return;
      var next = window.location.pathname + window.location.search;
      postUiRedirect({
        action: 'set_garden_pin',
        clear: false,
        room_wire: room,
        item_storage: storage,
        next: next,
      }).catch(function (err) {
        console.warn('slug pin failed', err);
      });
    });

    function decodePinCookie() {
      var m = document.cookie.match(/(?:^|;\s*)slug_garden_pin=([^;]+)/);
      if (!m) return null;
      var v = decodeURIComponent(m[1].replace(/\+/g, ' ')).trim();
      var raw;
      try {
        var b64 = v.replace(/-/g, '+').replace(/_/g, '/');
        while (b64.length % 4) b64 += '=';
        raw = atob(b64);
      } catch (err) {
        return null;
      }
      var sep = '\x1f';
      var i = raw.indexOf(sep);
      if (i < 0) {
        i = raw.indexOf('\t');
        if (i < 0) return null;
      }
      return { room: raw.slice(0, i), item: raw.slice(i + 1) };
    }

    function gardenItemHref(prefix, storageUrl) {
      var marker = 'https://slug.social/~/';
      if (storageUrl.indexOf(marker) === 0) {
        var tail = storageUrl.slice(marker.length);
        return prefix.replace(/\/$/, '') + (tail ? '/' + tail : '');
      }
      return storageUrl;
    }

    function refreshPinHud() {
      var hud = document.getElementById('slug-pin-hud');
      if (!hud) return;
      var prefix = hud.getAttribute('data-garden-prefix') || '';
      var bodyRoom = document.body ? document.body.getAttribute('data-garden-room') : '';
      var pin = decodePinCookie();
      hud.innerHTML = '';
      if (!pin || !prefix || pin.room !== bodyRoom) return;
      var a = document.createElement('a');
      a.className = 'slug-pin-hud-link';
      a.href = gardenItemHref(prefix, pin.item);
      a.title = 'Pinned item';
      var span = document.createElement('span');
      span.className = 'slug-pin-hud-glyph';
      span.setAttribute('aria-hidden', 'true');
      span.textContent = '📌';
      a.appendChild(span);
      var label = pin.item.replace(/^https:\/\/slug\.social\/~\/?/, '~/');
      if (label.length > 36) label = label.slice(0, 34) + '…';
      a.appendChild(document.createTextNode(' ' + label));
      hud.appendChild(a);
    }
    refreshPinHud();

    // Vote compare: map slider 0–100 to integer ratio weights
    var voteSlider = document.getElementById('vote-preference-slider');
    if (voteSlider) {
      var rl = document.getElementById('vote-ratio-left');
      var rr = document.getElementById('vote-ratio-right');
      function syncVoteRatio() {
        var p = parseInt(voteSlider.value, 10);
        if (isNaN(p)) p = 50;
        var L = 100 - p;
        var R = p;
        if (L === 0 && R === 0) {
          L = 1;
          R = 1;
        }
        if (rl) rl.value = String(L);
        if (rr) rr.value = String(R);
      }
      voteSlider.addEventListener('input', syncVoteRatio);
      syncVoteRatio();
    }

    // SSE: server-pushed JS
    function connectSSE() {
      var ssePath = window.location.pathname + window.location.search;
      var es = new EventSource('/sse?path=' + encodeURIComponent(ssePath));
      es.onmessage = function (e) {
        evalJs(e.data);
      };
      es.onerror = function () {
        es.close();
        setTimeout(connectSSE, 3000);
      };
    }
    connectSSE();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initSlugUi);
  } else {
    initSlugUi();
  }
})();
