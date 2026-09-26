(() => {
  const input = document.getElementById('search-input');
  const panel = document.getElementById('search-panel');
  const list = document.getElementById('search-results');
  const status = document.getElementById('search-status');
  const entries = window.HYDRA_SEARCH || [];
  const normalized = text => text.toLowerCase().replace(/[^a-z0-9_]+/g, ' ').trim();
  const hide = () => { panel.hidden = true; input.setAttribute('aria-expanded', 'false'); };

  function show() {
    const query = normalized(input.value);
    if (!query) { hide(); return; }
    const words = query.split(/\s+/);
    const matches = entries.map(entry => {
      const label = normalized(entry.label);
      const full = normalized(`${entry.label} ${entry.kind} ${entry.description}`);
      const score = label === query ? 100 : label.startsWith(query) ? 60 : words.every(w => label.includes(w)) ? 40 : 10;
      return { entry, score, matches: words.every(w => full.includes(w)) };
    }).filter(item => item.matches).sort((a, b) => b.score - a.score || a.entry.label.localeCompare(b.entry.label));
    list.replaceChildren();
    for (const { entry } of matches.slice(0, 12)) {
      const li = document.createElement('li');
      const link = document.createElement('a');
      link.href = entry.url;
      const kind = document.createElement('span');
      kind.className = 'search-kind';
      kind.textContent = entry.kind;
      const label = document.createElement('code');
      label.textContent = entry.label;
      const description = document.createElement('span');
      description.className = 'search-description';
      description.textContent = entry.description;
      link.append(kind, label, description);
      li.append(link);
      list.append(li);
    }
    status.textContent = matches.length ? `${matches.length} result${matches.length === 1 ? '' : 's'}${matches.length > 12 ? ' · showing 12' : ''}` : `No results for “${input.value}”`;
    panel.hidden = false;
    input.setAttribute('aria-expanded', 'true');
  }

  input.addEventListener('input', show);
  input.addEventListener('focus', () => { if (input.value) show(); });
  input.addEventListener('keydown', event => {
    if (event.key === 'ArrowDown') { event.preventDefault(); list.querySelector('a')?.focus(); }
    if (event.key === 'Enter') { const first = list.querySelector('a'); if (!panel.hidden && first) first.click(); }
  });
  list.addEventListener('keydown', event => {
    if (!['ArrowDown', 'ArrowUp'].includes(event.key)) return;
    event.preventDefault();
    const links = [...list.querySelectorAll('a')];
    const index = links.indexOf(document.activeElement);
    const next = index + (event.key === 'ArrowDown' ? 1 : -1);
    if (next < 0) input.focus(); else links[Math.min(next, links.length - 1)]?.focus();
  });
  document.addEventListener('keydown', event => {
    const editing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName) || document.activeElement.isContentEditable;
    if (event.key === '/' && !editing && !event.metaKey && !event.ctrlKey && !event.altKey) { event.preventDefault(); input.focus(); }
    if (event.key === 'Escape' && !panel.hidden) { input.focus(); hide(); }
  });
  document.addEventListener('click', event => { if (!event.target.closest('.search')) hide(); });
  document.addEventListener('focusin', event => { if (!event.target.closest('.search')) hide(); });
  list.addEventListener('click', event => { if (event.target.closest('a')) hide(); });
})();
