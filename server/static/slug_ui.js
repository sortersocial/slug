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

    // POST forms → eval response (except theme)
    document.addEventListener('submit', async function (e) {
      var f = e.target;
      if (!f || f.tagName !== 'FORM') return;
      if ((f.method || 'get').toLowerCase() !== 'post') return;
      if (f.id === 'slug-theme-form') return;
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
