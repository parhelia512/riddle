const source = document.getElementById('source');
const result = document.getElementById('result');
const statusEl = document.getElementById('status');
let timer = 0;
let requestId = 0;

function scheduleRender() {
  clearTimeout(timer);
  statusEl.textContent = 'editing';
  timer = setTimeout(render, 220);
}

async function render() {
  const id = ++requestId;
  statusEl.textContent = 'rendering';
  try {
    const response = await fetch('/graph', {
      method: 'POST',
      headers: { 'Content-Type': 'text/plain; charset=utf-8' },
      body: source.value
    });
    const html = await response.text();
    if (id !== requestId) return;
    result.innerHTML = html;
    statusEl.textContent = 'ready';
  } catch (error) {
    if (id !== requestId) return;
    result.innerHTML = '<div class="error-panel"><h2>Error</h2><p>' + escapeHtml(String(error)) + '</p></div>';
    statusEl.textContent = 'failed';
  }
}

function escapeHtml(text) {
  return text.replace(/[&<>"']/g, ch => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;'
  }[ch]));
}

source.addEventListener('input', scheduleRender);
source.addEventListener('keydown', event => {
  if (event.key === 'Tab') {
    event.preventDefault();
    const start = source.selectionStart;
    const end = source.selectionEnd;
    source.value = source.value.slice(0, start) + '    ' + source.value.slice(end);
    source.selectionStart = source.selectionEnd = start + 4;
    scheduleRender();
  }
});

render();
