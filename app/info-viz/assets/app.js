const source = document.getElementById('source');
const result = document.getElementById('result');
const statusEl = document.getElementById('status');
const toggleCode = document.getElementById('toggleCode');
const toggleData = document.getElementById('toggleData');
let timer = 0;
let requestId = 0;
const viewState = {
  code: localStorage.getItem('riddleViz.showCode') !== 'false',
  data: localStorage.getItem('riddleViz.showData') !== 'false'
};
let sectionState = loadSectionState();

function applyViewState() {
  document.body.classList.toggle('hide-code', !viewState.code);
  document.body.classList.toggle('hide-data', !viewState.data);
  toggleCode.classList.toggle('active', viewState.code);
  toggleCode.setAttribute('aria-pressed', String(viewState.code));
  toggleData.classList.toggle('active', viewState.data);
  toggleData.setAttribute('aria-pressed', String(viewState.data));
}

function setViewState(key, value) {
  viewState[key] = value;
  localStorage.setItem(key === 'code' ? 'riddleViz.showCode' : 'riddleViz.showData', String(value));
  applyViewState();
}

function scheduleRender() {
  clearTimeout(timer);
  statusEl.textContent = 'editing';
  timer = setTimeout(render, 220);
}

async function render() {
  const id = ++requestId;
  statusEl.textContent = 'rendering';
  rememberSectionState();
  try {
    const response = await fetch('/graph', {
      method: 'POST',
      headers: { 'Content-Type': 'text/plain; charset=utf-8' },
      body: source.value
    });
    const html = await response.text();
    if (id !== requestId) return;
    result.innerHTML = html;
    applySectionState();
    statusEl.textContent = 'ready';
  } catch (error) {
    if (id !== requestId) return;
    result.innerHTML = '<div class="error-panel"><h2>Error</h2><p>' + escapeHtml(String(error)) + '</p></div>';
    statusEl.textContent = 'failed';
  }
}

function loadSectionState() {
  try {
    return JSON.parse(localStorage.getItem('riddleViz.sections') || '{}');
  } catch (_) {
    return {};
  }
}

function sectionTitle(details) {
  return details.querySelector('summary span')?.textContent || '';
}

function rememberSectionState() {
  result.querySelectorAll('.data-section').forEach(details => {
    const title = sectionTitle(details);
    if (title) sectionState[title] = details.open;
  });
  localStorage.setItem('riddleViz.sections', JSON.stringify(sectionState));
}

function applySectionState() {
  result.querySelectorAll('.data-section').forEach(details => {
    const title = sectionTitle(details);
    if (Object.prototype.hasOwnProperty.call(sectionState, title)) {
      details.open = sectionState[title];
    }
    details.addEventListener('toggle', () => {
      const nextTitle = sectionTitle(details);
      if (!nextTitle) return;
      sectionState[nextTitle] = details.open;
      localStorage.setItem('riddleViz.sections', JSON.stringify(sectionState));
    });
  });
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
toggleCode.addEventListener('click', () => setViewState('code', !viewState.code));
toggleData.addEventListener('click', () => setViewState('data', !viewState.data));

applyViewState();
render();
