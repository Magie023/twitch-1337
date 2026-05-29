const CSRF_COOKIE_RE = /(?:^|; )tw1337_csrf=([^;]+)/;

document.body.addEventListener('htmx:configRequest', (evt) => {
  if (evt.detail.verb !== 'get') {
    const m = document.cookie.match(CSRF_COOKIE_RE);
    if (m) evt.detail.headers['X-Csrf-Token'] = decodeURIComponent(m[1]);
  }
});

(function berlinTimer() {
  const root = document.getElementById('berlin-timer');
  if (!root) return;
  const labelEl = root.querySelector('.timer-label');
  const clockEl = root.querySelector('.timer-clock');
  if (!labelEl || !clockEl) return;

  const fmt = new Intl.DateTimeFormat('en-GB', {
    timeZone: 'Europe/Berlin',
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });

  function berlinHMS(d) {
    const parts = fmt.formatToParts(d);
    const get = (t) => parts.find((p) => p.type === t).value;
    return [+get('hour'), +get('minute'), +get('second')];
  }

  function pad(n) {
    return String(n).padStart(2, '0');
  }

  function tick() {
    const [h, m, s] = berlinHMS(new Date());
    const armed = h === 13 && m === 37;
    if (armed) {
      root.classList.add('armed');
      labelEl.textContent = '1337 ARMED';
      clockEl.textContent = `13:37:${pad(s)}`;
    } else {
      root.classList.remove('armed');
      const nowSec = h * 3600 + m * 60 + s;
      const targetSec = 13 * 3600 + 37 * 60;
      let delta = targetSec - nowSec;
      if (delta <= 0) delta += 86400;
      const dh = Math.floor(delta / 3600);
      const dm = Math.floor((delta % 3600) / 60);
      const ds = delta % 60;
      labelEl.textContent = 'T-MINUS';
      clockEl.textContent = `${pad(dh)}:${pad(dm)}:${pad(ds)}`;
    }
  }
  tick();
  setInterval(tick, 1000);
})();

document.addEventListener(
  'keydown',
  (evt) => {
    if (evt.key !== '/' || evt.metaKey || evt.ctrlKey || evt.altKey) return;
    const tag = (evt.target && evt.target.tagName) || '';
    if (tag === 'INPUT' || tag === 'TEXTAREA' || (evt.target && evt.target.isContentEditable)) {
      return;
    }
    const search = document.getElementById('page-search');
    if (search) {
      evt.preventDefault();
      search.focus();
      search.select();
    }
  },
  true,
);

(function liveFilter() {
  const search = document.getElementById('page-search');
  if (!search) return;
  const items = Array.from(document.querySelectorAll('[data-filter]')).map((el) => ({
    el,
    hay: (el.getAttribute('data-filter') || '').toLowerCase(),
  }));
  if (items.length === 0) return;
  search.addEventListener('input', () => {
    const q = search.value.trim().toLowerCase();
    for (const { el, hay } of items) {
      el.style.display = !q || hay.includes(q) ? '' : 'none';
    }
  });
})();

(function settingsPage() {
  const form = document.getElementById('settings-form');
  const saveBar = document.getElementById('settings-save-bar');
  if (!form || !saveBar) return;

  const countEl = saveBar.querySelector('[data-count]');
  const nounEl = saveBar.querySelector('[data-noun]');
  const previewEl = saveBar.querySelector('[data-preview]');
  const discardBtn = saveBar.querySelector('[data-discard]');

  const rows = Array.from(form.querySelectorAll('.settings-row'))
    .map((row) => {
      const inputs = Array.from(row.querySelectorAll('input[name]'));
      const isRadio = inputs.length > 1 && inputs.every((i) => i.type === 'radio');
      const primary = inputs[0];
      if (!primary) return null;
      const baseline = isRadio
        ? (inputs.find((i) => i.checked)?.value ?? '')
        : primary.type === 'checkbox' ? (primary.checked ? 'true' : 'false') : primary.value;
      return {
        el: row,
        input: primary,
        inputs,
        isRadio,
        reset: row.querySelector('.row-reset'),
        pretty: row.querySelector('.row-pretty'),
        section: row.dataset.section,
        baseline,
      };
    })
    .filter((r) => r);


  const BYTES_UNIT_FACTOR = { B: 1, KiB: 1024, MiB: 1024 * 1024 };

  function pickBestBytesUnit(n) {
    if (n >= BYTES_UNIT_FACTOR.MiB) return 'MiB';
    if (n >= BYTES_UNIT_FACTOR.KiB) return 'KiB';
    return 'B';
  }

  function formatBytesIec(bytes) {
    const n = Number(bytes);
    if (!Number.isFinite(n)) return String(bytes);
    const unit = pickBestBytesUnit(n);
    const f = BYTES_UNIT_FACTOR[unit];
    const v = n / f;
    return `${n % f === 0 ? v : v.toFixed(2)} ${unit}`;
  }

  function formatPretty(input, prettyEl) {
    const unit = prettyEl?.dataset.prettyUnit ?? '';
    if (unit === 'bytes' || unit === 'B') return formatBytesIec(input.value);
    if (unit === 'bool') return input.checked ? 'On' : 'Off';
    if (input.type === 'text' || input.type === 'time' || input.type === 'email' || input.type === 'url') {
      return input.value || (input.placeholder ? `(${input.placeholder})` : '');
    }
    const n = Number(input.value);
    if (!Number.isFinite(n)) return input.value;
    if (unit === 'pct') return `${Math.round(n * 100)}%`;
    if (unit === 's') return n === 1 ? '1 second' : `${n} seconds`;
    return unit ? `${n} ${unit}` : String(n);
  }
  const cards = Array.from(form.querySelectorAll('.settings-card'));
  const navItems = Array.from(document.querySelectorAll('.settings-nav-item'));
  const navGroups = Array.from(document.querySelectorAll('.settings-nav-group')).map((el) => ({
    el,
    items: Array.from(el.querySelectorAll('.settings-nav-item')),
    badge: el.querySelector('.gdirty'),
  }));

  const STORAGE_KEY = 'settings-nav-groups-collapsed';
  let collapsed = new Set();
  try { collapsed = new Set(JSON.parse(localStorage.getItem(STORAGE_KEY) || '[]')); } catch { /* ignore corrupt JSON, start fresh */ }
  for (const { el } of navGroups) {
    const open = !collapsed.has(el.dataset.group);
    el.dataset.open = open ? 'true' : 'false';
    el.querySelector('.settings-nav-group-head')?.setAttribute('aria-expanded', open ? 'true' : 'false');
  }
  for (const head of document.querySelectorAll('[data-group-toggle]')) {
    head.addEventListener('click', () => {
      const group = head.closest('.settings-nav-group');
      if (!group) return;
      const id = group.dataset.group;
      const open = group.dataset.open === 'false';
      group.dataset.open = open ? 'true' : 'false';
      head.setAttribute('aria-expanded', open ? 'true' : 'false');
      if (open) collapsed.delete(id); else collapsed.add(id);
      localStorage.setItem(STORAGE_KEY, JSON.stringify([...collapsed]));
    });
  }

  function currentValue(input) {
    return input.type === 'checkbox' ? (input.checked ? 'true' : 'false') : input.value;
  }

  function rowCurrent(r) {
    if (r.isRadio) return r.inputs.find((i) => i.checked)?.value ?? '';
    return currentValue(r.input);
  }

  function applyDefault(r) {
    const def = r.input.dataset.default ?? '';
    if (r.isRadio) {
      for (const i of r.inputs) i.checked = i.value === def;
      return;
    }
    if (r.input.type === 'checkbox') {
      r.input.checked = def === 'true' || def === '1';
    } else {
      r.input.value = def;
    }
  }

  function refreshAll() {
    const dirtyKeys = [];
    const perSection = new Map();
    for (const r of rows) {
      const cur = rowCurrent(r);
      const dirty = cur !== r.baseline;
      const offDefault = cur !== (r.input.dataset.default ?? '');
      r.el.classList.toggle('is-dirty', dirty);
      if (r.reset) r.reset.hidden = !offDefault;
      if (r.pretty) {
        const isText = ['text', 'time', 'email', 'url'].includes(r.input.type);
        // Server already renders nuanced markup (placeholder/empty muted span)
        // for text-type rows; only overwrite the pretty when the user has
        // edited the value, otherwise leave the SSR markup intact.
        if (!isText || dirty) r.pretty.textContent = formatPretty(r.input, r.pretty);
      }
      if (r.input.type === 'range') {
        const valEl = r.el.querySelector('[data-range-value]');
        if (valEl) {
          const n = Number(r.input.value);
          valEl.textContent = Number.isFinite(n) ? `${Math.round(n * 100)}%` : r.input.value;
        }
      }
      if (!dirty) continue;
      dirtyKeys.push(r.input.dataset.key ?? r.input.name);
      if (r.section) perSection.set(r.section, (perSection.get(r.section) ?? 0) + 1);
    }

    for (const card of cards) {
      const n = perSection.get(card.dataset.section) ?? 0;
      const badge = card.querySelector('.card-dirty');
      if (badge) {
        badge.hidden = n === 0;
        badge.textContent = `${n} modified`;
      }
    }
    for (const item of navItems) {
      const n = perSection.get(item.dataset.target) ?? 0;
      const badge = item.querySelector('.ndirty');
      if (badge) {
        badge.hidden = n === 0;
        badge.textContent = String(n);
      }
    }

    for (const g of navGroups) {
      let n = 0;
      for (const item of g.items) n += perSection.get(item.dataset.target) ?? 0;
      if (g.badge) {
        g.badge.hidden = n === 0;
        g.badge.textContent = String(n);
      }
    }

    const total = dirtyKeys.length;
    countEl.textContent = String(total);
    nounEl.textContent = total === 1 ? 'change' : 'changes';
    const preview = dirtyKeys.slice(0, 3).join(' · ');
    previewEl.textContent =
      total > 3 ? `${preview} +${total - 3}` : preview;
    saveBar.classList.toggle('visible', total > 0);
  }

  const bytesRows = Array.from(form.querySelectorAll('[data-bytes-row]'))
    .map((el) => ({
      el,
      hidden: el.querySelector('.bytes-canonical'),
      display: el.querySelector('.bytes-display'),
      buttons: Array.from(el.querySelectorAll('.bytes-unit')),
    }))
    .filter((b) => b.hidden && b.display);
  const bytesRowByHidden = new Map(bytesRows.map((b) => [b.hidden, b]));

  function displayInUnit(bytes, unit) {
    return parseFloat((bytes / BYTES_UNIT_FACTOR[unit]).toFixed(6)).toString();
  }

  function activeUnit(b) {
    return b.buttons.find((btn) => btn.classList.contains('is-active'))?.dataset.unit
      ?? pickBestBytesUnit(Number(b.hidden.value) || 0);
  }

  function syncBytesRow(b) {
    const bytes = Number(b.hidden.value) || 0;
    const unit = activeUnit(b);
    if (document.activeElement !== b.display) b.display.value = displayInUnit(bytes, unit);
    for (const btn of b.buttons) {
      const active = btn.dataset.unit === unit;
      btn.classList.toggle('is-active', active);
      btn.setAttribute('aria-pressed', active ? 'true' : 'false');
    }
  }

  function commitBytes(b, bytes) {
    const clamped = Math.max(0, Math.round(bytes));
    if (Number(b.hidden.value) === clamped) return;
    b.hidden.value = String(clamped);
    b.hidden.dispatchEvent(new Event('input', { bubbles: true }));
  }

  for (const b of bytesRows) {
    b.display.addEventListener('input', () => {
      const v = Number(b.display.value);
      if (!Number.isFinite(v) || v < 0) return;
      commitBytes(b, v * BYTES_UNIT_FACTOR[activeUnit(b)]);
    });
    b.display.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        syncBytesRow(b);
        b.display.blur();
      } else if (e.key === 'Enter') {
        e.preventDefault();
        b.display.blur();
      }
    });
    b.display.addEventListener('blur', () => syncBytesRow(b));

    for (const btn of b.buttons) {
      btn.addEventListener('click', () => {
        for (const other of b.buttons) other.classList.toggle('is-active', other === btn);
        syncBytesRow(b);
      });
    }
  }

  for (const b of bytesRows) {
    const def = b.el.querySelector('[data-bytes-default]');
    if (def) def.textContent = formatBytesIec(def.dataset.bytesDefault);
    syncBytesRow(b);
  }

  form.addEventListener('input', refreshAll);
  form.addEventListener('change', (e) => {
    const t = e.target;
    if (t instanceof HTMLInputElement && t.type === 'radio') {
      const group = form.querySelectorAll(`input[type=radio][name="${t.name}"]`);
      for (const i of group) {
        i.closest('.segment')?.classList.toggle('is-active', i.checked);
      }
    }
    refreshAll();
  });

  form.addEventListener('click', (e) => {
    const btn = e.target.closest('.row-reset');
    if (!btn) return;
    const r = rows.find((row) => row.reset === btn);
    if (!r) return;
    applyDefault(r);
    const b = bytesRowByHidden.get(r.input);
    if (b) syncBytesRow(b);
    refreshAll();
    r.input.focus();
  });

  discardBtn?.addEventListener('click', () => location.reload());

  if (cards.length && 'IntersectionObserver' in window) {
    const byId = new Map(navItems.map((el) => [el.dataset.target, el]));
    let active = null;
    const setActive = (section) => {
      if (section === active) return;
      if (active) byId.get(active)?.classList.remove('active');
      active = section;
      byId.get(section)?.classList.add('active');
      const activeItem = byId.get(section);
      const activeGroup = activeItem?.closest('.settings-nav-group');
      for (const g of navGroups) g.el.classList.toggle('has-active', g.el === activeGroup);
    };
    setActive(cards[0].dataset.section);
    const io = new IntersectionObserver(
      (entries) => {
        const vis = entries
          .filter((e) => e.isIntersecting)
          .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)[0];
        if (vis) setActive(vis.target.dataset.section);
      },
      { rootMargin: '-72px 0px -60% 0px' },
    );
    for (const card of cards) io.observe(card);
  }

  for (const item of navItems) {
    item.addEventListener('click', (e) => {
      const target = document.getElementById('sec-' + item.dataset.target);
      if (!target) return;
      e.preventDefault();
      target.scrollIntoView({ behavior: 'smooth', block: 'start' });
    });
  }

  refreshAll();
})();

// ─── Model autocomplete datalist (Task 20) ───────────────────────────────────
async function refreshModelDatalist(input) {
  const url = input.dataset.modelsUrl;
  const scope = input.dataset.scope;
  if (!url || !scope) return;
  let body;
  try {
    const res = await fetch(url, { credentials: 'same-origin' });
    if (!res.ok) return;
    body = await res.json();
  } catch {
    return;
  }
  const dl = document.getElementById(`ai-models-${scope}`);
  if (!dl) return;
  while (dl.firstChild) dl.removeChild(dl.firstChild);
  for (const m of body.models ?? []) {
    const opt = document.createElement('option');
    opt.value = m.id;
    opt.label = m.label;
    dl.appendChild(opt);
  }
  if (body.error) {
    input.setAttribute('aria-errormessage', body.error);
  } else {
    input.removeAttribute('aria-errormessage');
  }
}

document.body.addEventListener('focusin', (evt) => {
  const t = evt.target;
  if (t instanceof HTMLInputElement && t.classList.contains('model-input')) {
    if (!t.dataset.modelsLoaded) {
      t.dataset.modelsLoaded = '1';
      refreshModelDatalist(t);
    }
  }
});

// ─── Segmented selector active-state ─────────────────────────────────────────
document.body.addEventListener('change', (evt) => {
  const radio = evt.target;
  if (!(radio instanceof HTMLInputElement) || radio.type !== 'radio') return;
  const wrap = radio.closest('.segmented');
  if (!wrap) return;
  for (const seg of wrap.querySelectorAll('.segment')) {
    seg.classList.toggle('is-active', seg.querySelector('input').checked);
  }
});

// ─── Card toggle dimming ─────────────────────────────────────────────────────
function syncCardEnabled(card) {
  const toggle = card.querySelector('input[type=checkbox][data-card-toggle]');
  if (!toggle) return;
  const on = toggle.checked;
  card.classList.toggle('is-card-off', !on);
  for (const ctrl of card.querySelectorAll('[data-card-enabled-by]')) {
    if (
      ctrl instanceof HTMLInputElement ||
      ctrl instanceof HTMLButtonElement ||
      ctrl instanceof HTMLSelectElement ||
      ctrl instanceof HTMLTextAreaElement
    ) {
      ctrl.disabled = !on;
    } else {
      ctrl.classList.toggle('is-disabled', !on);
    }
  }
}
document.querySelectorAll('.settings-card[data-section]').forEach(syncCardEnabled);
document.body.addEventListener('change', (evt) => {
  if (evt.target.hasAttribute?.('data-card-toggle')) {
    syncCardEnabled(evt.target.closest('.settings-card'));
  }
});

// ─── Live character counters ─────────────────────────────────────────────────
// Any `<textarea data-char-count maxlength=N>` gets a live "n / N" readout in
// the sibling `[data-char-count-for]` within its `.row`; `.over` flags the cap.
for (const ta of document.querySelectorAll('textarea[data-char-count]')) {
  const counter = ta.closest('.row')?.querySelector('[data-char-count-for]');
  if (!counter) continue;
  const max = parseInt(ta.getAttribute('maxlength'), 10) || 500;
  const render = () => {
    counter.textContent = `${ta.value.length} / ${max}`;
    counter.classList.toggle('over', ta.value.length >= max);
  };
  ta.addEventListener('input', render);
  render();
}
