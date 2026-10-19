const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

async function tick() {
  const s = await invoke('snap');
  $('title').textContent = s.title;
  $('percent').textContent = s.percent;
  if (s.copying) {
    $('fill').style.width = (s.frac * 100).toFixed(1) + '%';
    $('name').textContent = s.name;
    $('eta').textContent = s.eta;
    $('detail').textContent = s.detail;
  }
  document.body.classList.toggle('paused', s.paused);
}

$('pause').onclick = async () => {
  const paused = await invoke('toggle_pause');
  $('pause').textContent = paused ? '继续' : '暂停';
};
$('cancel').onclick = () => invoke('cancel');
document.addEventListener('contextmenu', (e) => e.preventDefault());

tick().then(() => invoke('page_ready'));
setInterval(tick, 100);
