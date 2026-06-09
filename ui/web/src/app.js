const status = document.getElementById('status');
const version = document.getElementById('version');
const models = document.getElementById('models');
const advisories = document.getElementById('advisories');

function renderList(target, values, emptyLabel) {
  target.innerHTML = '';
  if (!values.length) {
    const li = document.createElement('li');
    li.textContent = emptyLabel;
    target.appendChild(li);
    return;
  }
  for (const value of values) {
    const li = document.createElement('li');
    li.textContent = value;
    target.appendChild(li);
  }
}

async function boot() {
  const response = await fetch('/api/status');
  const payload = await response.json();
  status.textContent = payload.message;
  version.textContent = payload.current?.active ?? 'uninitialized';
  renderList(models, payload.models.map((model) => `${model.display_name} (${model.profile})`), 'No models discovered yet.');
  renderList(advisories, payload.advisories.map((item) => `${item.id}: ${item.summary}`), 'No advisories apply to this install.');
}

boot().catch((error) => {
  status.textContent = `Runtime API unavailable: ${error.message}`;
});
