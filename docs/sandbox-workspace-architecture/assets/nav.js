(function () {
  const path = window.location.pathname.split('/').pop() || 'index.html';
  document.querySelectorAll('[data-doc-section]').forEach((link) => {
    const href = link.getAttribute('href') || '';
    if (href === path) link.classList.add('active');
  });

  const input = document.querySelector('[data-doc-search]');
  const results = document.querySelector('[data-doc-search-results]');
  const index = window.SANDBOX_WORKSPACE_DOC_SEARCH || [];
  if (!input || !results || !index.length) return;

  function render(query) {
    const q = query.trim().toLowerCase();
    results.innerHTML = '';
    if (!q) return;
    const matches = index.filter((item) => {
      return (item.title + ' ' + item.text + ' ' + item.id).toLowerCase().includes(q);
    }).slice(0, 8);
    for (const item of matches) {
      const a = document.createElement('a');
      a.href = item.href;
      a.innerHTML = '<strong>' + item.title + '</strong><small>' + item.text + '</small>';
      results.appendChild(a);
    }
  }

  input.addEventListener('input', () => render(input.value));
})();
