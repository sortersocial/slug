/**
 * Slug web UI: only plumbing — fetch/eval/SSE. No product UI logic here.
 */
(function () {
  var DRAFT_PREFIX = 'slug-draft:';
  var draftSaveTimers = new WeakMap();
  var draftBound = new WeakSet();

  function evalJs(js) {
    if (js && String(js).trim()) {
      eval(js);
    }
    initDrafts();
  }

  function draftStorageId(key) {
    return DRAFT_PREFIX + key;
  }

  function draftIsEmpty(data) {
    return Object.keys(data).every(function (k) {
      return !String(data[k] || '').trim();
    });
  }

  function collectDraft(container) {
    var data = {};
    if (container.tagName === 'FORM') {
      container.querySelectorAll('input[name], textarea[name], select[name]').forEach(function (el) {
        if (el.type === 'radio' && !el.checked) return;
        if (el.type === 'checkbox' && !el.checked) return;
        data[el.name] = el.value;
      });
      var slider = container.querySelector('.vote-preference-slider');
      if (slider) data.__slider = slider.value;
      return data;
    }
    if (container.tagName === 'TEXTAREA' || container.tagName === 'INPUT') {
      data.__self = container.value;
    }
    return data;
  }

  function clearDraftByKey(key) {
    if (!key) return;
    try {
      localStorage.removeItem(draftStorageId(key));
    } catch (e) {}
  }

  function saveDraft(container) {
    var key = container.getAttribute('data-draft-key');
    if (!key) return;
    var data = collectDraft(container);
    try {
      if (draftIsEmpty(data)) {
        clearDraftByKey(key);
      } else {
        localStorage.setItem(draftStorageId(key), JSON.stringify(data));
      }
    } catch (e) {}
  }

  function scheduleDraftSave(container) {
    var prev = draftSaveTimers.get(container);
    if (prev) clearTimeout(prev);
    var handle = setTimeout(function () {
      saveDraft(container);
    }, 500);
    draftSaveTimers.set(container, handle);
  }

  function syncVoteSliderFromDraft(form, data) {
    var slider = form.querySelector('.vote-preference-slider');
    if (!slider) return;
    if (data.__slider != null && String(data.__slider).trim() !== '') {
      slider.value = data.__slider;
      slider.dispatchEvent(new Event('input', { bubbles: true }));
      return;
    }
    var left = parseInt(data.ratio_left, 10);
    var right = parseInt(data.ratio_right, 10);
    if (!isNaN(left) && !isNaN(right) && left + right > 0) {
      slider.value = String(Math.round((right * 100) / (left + right)));
      slider.dispatchEvent(new Event('input', { bubbles: true }));
    }
  }

  function restoreDraft(container) {
    var key = container.getAttribute('data-draft-key');
    if (!key) return;
    var raw;
    try {
      raw = localStorage.getItem(draftStorageId(key));
    } catch (e) {
      return;
    }
    if (!raw) return;
    var data;
    try {
      data = JSON.parse(raw);
    } catch (e) {
      return;
    }
    if (!data || typeof data !== 'object') return;

    if (container.tagName === 'FORM') {
      container.querySelectorAll('input[name], textarea[name], select[name]').forEach(function (el) {
        if (!(el.name in data)) return;
        if (el.type === 'radio') {
          el.checked = el.value === data[el.name];
          return;
        }
        if (el.type === 'checkbox') {
          el.checked = !!data[el.name];
          return;
        }
        el.value = data[el.name];
      });
      syncVoteSliderFromDraft(container, data);
      return;
    }

    if (data.__self != null && (container.tagName === 'TEXTAREA' || container.tagName === 'INPUT')) {
      container.value = data.__self;
    }
  }

  function bindDraftAutosave(container) {
    if (!container || !container.getAttribute('data-draft-key') || draftBound.has(container)) {
      return;
    }
    draftBound.add(container);
    restoreDraft(container);

    if (container.tagName === 'FORM') {
      container.addEventListener('input', function (e) {
        var target = e.target;
        if (!target || !(target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT')) {
          return;
        }
        scheduleDraftSave(container);
      });
      container.addEventListener('change', function (e) {
        var target = e.target;
        if (!target || target.tagName !== 'SELECT') return;
        scheduleDraftSave(container);
      });
      return;
    }

    container.addEventListener('input', function () {
      scheduleDraftSave(container);
    });
  }

  function initDrafts() {
    document.querySelectorAll('form[data-draft-key], [data-draft-key]#editor-input').forEach(bindDraftAutosave);
  }

  var nativeFormReset = HTMLFormElement.prototype.reset;
  HTMLFormElement.prototype.reset = function () {
    nativeFormReset.call(this);
    clearDraftByKey(this.getAttribute('data-draft-key'));
  };

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

    function refreshPinHud() {
      var hud = document.getElementById('slug-pin-hud');
      if (!hud) return;
      var prefix = hud.getAttribute('data-garden-prefix') || '';
      var bodyRoom = document.body ? document.body.getAttribute('data-garden-room') : '';
      var pin = decodePinCookie();
      hud.innerHTML = '';
      if (!pin || !prefix || pin.room !== bodyRoom) return;
      var form = document.createElement('form');
      form.method = 'POST';
      form.action = '/ui';
      form.setAttribute('data-navigate', 'full');
      form.className = 'slug-pin-hud-form';
      var rpc = document.createElement('input');
      rpc.type = 'hidden';
      rpc.name = '__rpc__';
      rpc.value = JSON.stringify({
        action: 'set_garden_pin',
        clear: true,
        room_wire: '',
        next: window.location.pathname + window.location.search,
        form_action: '/ui',
      });
      form.appendChild(rpc);
      var btn = document.createElement('button');
      btn.type = 'submit';
      btn.className = 'slug-pin-hud-link slug-pin-hud-unpin-btn';
      btn.title = 'Unpin — removes this item from the corner HUD';
      btn.setAttribute('aria-label', 'Unpin pinned item');
      var span = document.createElement('span');
      span.className = 'slug-pin-hud-glyph';
      span.setAttribute('aria-hidden', 'true');
      span.textContent = '📌';
      btn.appendChild(span);
      var label = pin.item.replace(/^https:\/\/slug\.social\/~\/?/, '~/');
      if (label.length > 36) label = label.slice(0, 34) + '…';
      btn.appendChild(document.createTextNode(' ' + label));
      form.appendChild(btn);
      hud.appendChild(form);
    }
    refreshPinHud();

    // Vote compare: map slider 0–100 to reduced integer ratio weights.
    // Home index can render several forms; bind each slider inside its form.
    function gcd(a, b) {
      a = Math.abs(a | 0);
      b = Math.abs(b | 0);
      while (b) {
        var t = b;
        b = a % b;
        a = t;
      }
      return a || 1;
    }
    function reduceRatio(left, right) {
      var L = Math.max(1, left | 0);
      var R = Math.max(1, right | 0);
      var g = gcd(L, R);
      return [L / g, R / g];
    }
    function bindVoteSlider(voteSlider) {
      if (!voteSlider || voteSlider.getAttribute('data-slug-bound')) return;
      voteSlider.setAttribute('data-slug-bound', '1');
      var root = voteSlider.closest('form') || voteSlider.closest('.vote-compare-shell');
      var rl = root ? root.querySelector('input[name="ratio_left"]') : null;
      var rr = root ? root.querySelector('input[name="ratio_right"]') : null;
      var readout = root ? root.querySelector('.vote-ratio-readout') : null;
      function syncVoteRatio() {
        var p = parseInt(voteSlider.value, 10);
        if (isNaN(p)) p = 50;
        var rawL = 100 - p;
        var rawR = p;
        var reduced = reduceRatio(rawL, rawR);
        var L = reduced[0];
        var R = reduced[1];
        var label = L + ':' + R;
        if (rl) rl.value = String(L);
        if (rr) rr.value = String(R);
        if (readout) readout.textContent = label;
        voteSlider.setAttribute('aria-valuetext', label);
      }
      voteSlider.addEventListener('input', syncVoteRatio);
      syncVoteRatio();
    }
    document.querySelectorAll('input.vote-preference-slider').forEach(bindVoteSlider);

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
    initDrafts();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initSlugUi);
  } else {
    initSlugUi();
  }
})();
