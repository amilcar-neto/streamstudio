import {
  Room, RoomEvent, Track, ConnectionQuality, VideoQuality, DisconnectReason,
} from './livekit-client.js?v=2.22.3';

/* ============================================================
   Configuração
   ============================================================ */
// filme e jogo são movimento: se faltar banda ou CPU, cai a resolução antes da fluidez
const DICA_CONTEUDO = 'motion';
const PREFERENCIA = 'maintain-framerate';
const LARGURA = { 720: 1280, 1080: 1920, 1440: 2560, 2160: 3840 };
// o site mora em um subcaminho (ex.: /stream/), dado pelo <base> do index.html
const BASE = new URL(document.baseURI).pathname.replace(/\/?$/, '/');
const URL_LIVEKIT = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}${BASE.replace(/\/$/, '')}`;

/* ============================================================
   Utilidades
   ============================================================ */
const $ = (id) => document.getElementById(id);

function h(tag, attrs = {}, ...filhos) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === 'class') e.className = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v === true ? '' : v);
  }
  for (const f of filhos.flat()) if (f != null && f !== false) e.append(f);
  return e;
}
const icone = (nome) => h('i', { class: `ph ph-${nome}`, 'aria-hidden': 'true' });

const guardar = {
  ler(k, padrao) { try { const v = localStorage.getItem(k); return v === null ? padrao : JSON.parse(v); } catch { return padrao; } },
  salvar(k, v) { try { localStorage.setItem(k, JSON.stringify(v)); } catch {} },
};
// mapas id da sala -> valor (chave de acesso, segredo de dono)
const mapa = (nome) => ({
  pegar: (id) => guardar.ler(nome, {})[id],
  salvar(id, v) { const m = guardar.ler(nome, {}); m[id] = v; guardar.salvar(nome, m); },
  apagar(id) { const m = guardar.ler(nome, {}); delete m[id]; guardar.salvar(nome, m); },
});
const chaves = mapa('chaves');
const donos = mapa('donos');
// o servidor só guarda o hash da senha: quem criou guarda a senha aqui pra ver depois no Convidar
const senhas = mapa('senhas');

const plural = (n, um, varios) => `${n} ${n === 1 ? um : varios}`;
const nomeDe = (p) => p?.name || p?.identity?.split('#')[0] || '?';
function corDe(nome) {
  let x = 0;
  for (const c of nome) x = (x * 31 + c.charCodeAt(0)) % 360;
  return x;
}
function iniciais(nome) {
  const partes = nome.trim().split(/\s+/);
  return ((partes[0]?.[0] || '') + (partes[1]?.[0] || '')).toUpperCase() || '?';
}
const doisDigitos = (n) => String(n).padStart(2, '0');
function relogio(segundos) {
  const s = Math.max(0, Math.floor(segundos));
  return `${doisDigitos(Math.floor(s / 3600))}:${doisDigitos(Math.floor(s / 60) % 60)}:${doisDigitos(s % 60)}`;
}
function minSeg(segundos) {
  const s = Math.max(0, Math.round(segundos));
  return `${Math.floor(s / 60)}:${doisDigitos(s % 60)}`;
}
const formatarCodigo = (id) => `${id.slice(0, 4)}-${id.slice(4)}`;

function toast(msg, { erro = false, acao, tempo = 4000 } = {}) {
  const t = h('div', { class: 'toast' + (erro ? ' erro-t' : '') }, h('span', {}, msg));
  if (acao) t.append(h('button', { type: 'button', onclick: () => { acao.fn(); t.remove(); } }, acao.rotulo));
  $('toasts').append(t);
  setTimeout(() => t.remove(), tempo);
}

async function copiar(texto) {
  try { await navigator.clipboard.writeText(texto); } catch {
    const tmp = h('textarea', {}, texto);
    document.body.append(tmp);
    tmp.select();
    document.execCommand('copy');
    tmp.remove();
  }
  toast('Copiado.', { tempo: 2000 });
}

async function api(caminho, corpo) {
  let r;
  try {
    r = await fetch(`api/${caminho}`, {
      method: corpo ? 'POST' : 'GET',
      headers: corpo ? { 'Content-Type': 'application/json' } : {},
      body: corpo ? JSON.stringify(corpo) : undefined,
    });
  } catch {
    throw Object.assign(new Error('Sem conexão com o servidor. Confira sua internet.'), { status: 0 });
  }
  const dados = await r.json().catch(() => ({}));
  if (!r.ok) throw Object.assign(new Error(dados.erro || 'O servidor não respondeu. Tente de novo em instantes.'), { status: r.status });
  return dados;
}

/* ============================================================
   Estado
   ============================================================ */
// o Firefox codifica H.264 só no processador (e devagar); lá o VP8 rende muito mais
const FIREFOX = /firefox/i.test(navigator.userAgent);
// iPhone e iPad ignoram o volume do <audio>: lá o som passa pelo Web Audio, que deixa controlar
const IOS = /iPhone|iPad|iPod/.test(navigator.userAgent) || (/Macintosh/.test(navigator.userAgent) && navigator.maxTouchPoints > 1);
// sem isso, o som do Web Audio some com o iPhone no silencioso (iOS 17+)
if (IOS && navigator.audioSession) try { navigator.audioSession.type = 'playback'; } catch {}
const TX_PADRAO = { res: 1080, fps: 60, mbps: 12, codec: FIREFOX ? 'vp8' : 'vp9', audio: true };
const estado = {
  nome: guardar.ler('nome', ''),
  nomeConfirmado: false,      // o apelido é confirmado uma vez cada vez que o site abre
  entrada: {},                // { sala, nomeSala } quando veio por um endereço de sala
  salaId: null,
  sala: null,                 // { id, codigo, nome, convite, ocioso }
  meta: null,                 // metadados do LiveKit: { ocioso, semStreamDesde }
  convidarAoEntrar: false,
  layout: guardar.ler('layout', 'foco'),
  foco: null,                 // identity da transmissão em destaque
  volume: guardar.ler('volume', 80),
  mudo: guardar.ler('mudo', false),
  tx: { ...TX_PADRAO, ...guardar.ler('tx', {}) },
  noArDesde: 0,
};
// quem já usava o site no Firefox com H.264 salvo passa pro VP8 uma vez
if (FIREFOX && !guardar.ler('tx', {}).migrado) { estado.tx.codec = 'vp8'; estado.tx.migrado = true; guardar.salvar('tx', estado.tx); }
// o seletor Filme/Jogo/Texto saiu; "filme" travava em 30 fps sem a pessoa perceber
const veioDoModoAntigo = 'tipo' in estado.tx;
if (veioDoModoAntigo) { delete estado.tx.tipo; estado.tx.fps = 60; guardar.salvar('tx', estado.tx); }

/* sem placa de vídeo, o H.264 do navegador é o codificador mais pesado: em teste segurou só
   720p60 por CPU, enquanto o VP9 manteve 1080p60 folgado. H.264 só quando for por hardware. */
async function escolherCodecPadrao() {
  if (FIREFOX || (guardar.ler('tx', null) && !veioDoModoAntigo)) return;
  let hw = false;
  try {
    const r = await navigator.mediaCapabilities.encodingInfo({
      type: 'webrtc',
      video: { contentType: 'video/H264;profile-level-id=42e01f', width: 1920, height: 1080, bitrate: 12e6, framerate: 60 },
    });
    hw = r.supported && r.powerEfficient;
  } catch {}
  estado.tx.codec = hw ? 'h264' : 'vp9';
  salvarTx();
  pintarTx();
}

let room = null;
/** identity -> { p, pubV, pubA, som, local } */
const streams = new Map();
let saindoDeProposito = false;

const telas = { abrir: $('tela-abrir'), entrada: $('tela-entrada'), inicio: $('tela-inicio'), sala: $('tela-sala') };
/* Ícone da aba: bolinha vermelha apagada; acende quando alguém da sala está transmitindo. */
const FAVICON = (aceso) => 'data:image/svg+xml,' + encodeURIComponent(
  `<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'><circle cx='16' cy='16' r='6' fill='#ff3b30'${aceso ? '' : " fill-opacity='.35'"}/></svg>`);
let faviconAceso = false;
function pintarFavicon(aceso) {
  if (aceso === faviconAceso) return;
  faviconAceso = aceso;
  $('favicon').href = FAVICON(aceso);
}
function mostrar(qual) {
  if (qual !== 'sala') pintarFavicon(false);
  for (const [nome, tela] of Object.entries(telas)) tela.hidden = nome !== qual;
  document.body.classList.toggle('na-sala', qual === 'sala');
  document.title = qual === 'sala' && estado.sala ? `${estado.sala.nome} · StreamStudio` : 'StreamStudio';
}

const minhaPub = (fonte) => room?.localParticipant.getTrackPublication(fonte);
const euAoVivo = () => !!minhaPub(Track.Source.ScreenShare);

/* ============================================================
   Rotas: /  ·  /s/<código>  ·  /c/<convite>
   ============================================================ */
const caminho = () => (location.pathname.startsWith(BASE) ? location.pathname.slice(BASE.length) : '');
function ir(rel, { substituir = false } = {}) {
  history[substituir ? 'replaceState' : 'pushState'](null, '', BASE + rel);
  rota();
}
document.addEventListener('click', (e) => {
  const a = e.target.closest('a[data-link]');
  if (!a || e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return;
  e.preventDefault();
  ir(new URL(a.href).pathname.slice(BASE.length));
});
window.addEventListener('popstate', rota);

/* O site não consegue perguntar ao Windows se o app está instalado. Fica um sinal guardado
   quando há sinal de app: a pessoa veio do app (troca F4.6) ou um "Abrir no app" fez a página
   perder o foco (o app abriu ou o navegador perguntou se pode abrir). Só no Windows. */
const WINDOWS = /Windows/.test(navigator.userAgent);
const temApp = () => WINDOWS && guardar.ler('temApp', false) === true;
function notarAberturaDoApp(abriu) {
  const marcar = () => { guardar.salvar('temApp', true); parar(); abriu?.(); };
  const sumiu = () => { if (document.hidden) marcar(); };
  const parar = () => { window.removeEventListener('blur', marcar); document.removeEventListener('visibilitychange', sumiu); };
  window.addEventListener('blur', marcar);
  document.addEventListener('visibilitychange', sumiu);
  setTimeout(parar, 10000);
}

/* Quem tem o app e abre um link de sala no navegador: o site já tenta abrir o app (como o
   Zoom faz). Se o app não abrir, a tela fica com "Continuar no navegador". Só na primeira
   rota da página; links colados dentro do site seguem direto. */
let perguntarApp = temApp();
function mostrarAbrir(tipo, id) {
  // F4.6: leva o apelido junto, se já tiver um; o fragmento # não vai ao servidor
  $('btn-abrir-app').href = `streamstudio://${tipo}/${id}` + (estado.nome ? `#n=${encodeURIComponent(estado.nome)}` : '');
  $('abrir-titulo').textContent = tipo === 'c' ? 'Te chamaram pra uma sala.' : 'Entrar na sala.';
  mostrar('abrir');
  $('btn-abrir-app').focus();
  abrirApp();
}
function abrirApp() {
  $('abrir-sub').textContent = 'Abrindo no app do StreamStudio… Se ele não abrir, continue aqui no navegador.';
  notarAberturaDoApp(() => {
    $('abrir-sub').textContent = 'Abriu no app. Pode fechar esta aba, ou continuar aqui no navegador.';
  });
  // espera a página terminar de carregar: abrir o app no meio interrompe o carregamento
  const ir = () => { location.href = $('btn-abrir-app').href; };
  if (document.readyState === 'complete') ir(); else window.addEventListener('load', ir, { once: true });
}
$('btn-abrir-app').addEventListener('click', (e) => { e.preventDefault(); abrirApp(); });
$('btn-continuar-navegador').addEventListener('click', () => rota());

/* F4.6: alternar entre site e app sem conta. O botão leva nome, chave da sala e, para quem
   é dono, o segredo de dono no fragmento # (nunca vai ao servidor). O link de convite não
   leva nada disso. Quem recebe guarda como se tivesse entrado aqui e apaga o fragmento. */
const segredoValido = (v, max) => typeof v === 'string' && v.length >= 16 && v.length <= max && /^[A-Za-z0-9._-]+$/.test(v);
function linkTroca(id) {
  const f = new URLSearchParams({ n: estado.nome, k: chaves.pegar(id) || '' });
  const dono = donos.pegar(id);
  if (dono) f.set('d', dono);
  return `streamstudio://s/${id}#${f}`;
}
function receberTroca() {
  if (location.hash.length < 2) return;
  const f = new URLSearchParams(location.hash.slice(1));
  history.replaceState(history.state, '', location.pathname + location.search);
  const sala = caminho().match(/^s\/([a-z0-9]{8})$/)?.[1];
  const nome = (f.get('n') || '').trim();
  if (nome && nome.length <= 24 && !/[\u0000-\u001f]/.test(nome)) { definirNome(nome); estado.nomeConfirmado = true; }
  if (!sala || !segredoValido(f.get('k'), 512)) return;
  chaves.salvar(sala, f.get('k'));
  if (segredoValido(f.get('d'), 128)) donos.salvar(sala, f.get('d'));
  perguntarApp = false;  // veio do app: não pergunta "abrir no app?" de novo
  if (WINDOWS) guardar.salvar('temApp', true);
}
$('btn-sala-app').addEventListener('click', (e) => {
  if (!estado.sala) return e.preventDefault();
  e.currentTarget.href = linkTroca(estado.sala.id);
  notarAberturaDoApp();
  toast('Abrindo no app… Quando ele entrar, pode sair daqui.', { acao: { rotulo: 'Sair daqui', fn: () => ir('') }, tempo: 15000 });
});

async function rota() {
  const p = caminho();
  if (perguntarApp) {
    perguntarApp = false;
    const link = p.match(/^(c)\/([a-z0-9]{6,16})$/) || p.match(/^(s)\/([a-z0-9]{8})$/);
    if (link) return mostrarAbrir(link[1], link[2]);
  }
  const sala = p.match(/^s\/([a-z0-9]{8})$/)?.[1] || null;
  if (estado.salaId && sala !== estado.salaId) {
    if (euAoVivo() && !confirm('Você está transmitindo. Sair da sala encerra a transmissão. Sair mesmo assim?')) {
      history.pushState(null, '', `${BASE}s/${estado.salaId}`);
      return;
    }
    await sairDaSala();
  }
  const convite = p.match(/^c\/([a-z0-9]{6,16})$/)?.[1];
  if (convite) return usarConvite(convite);
  if (sala) {
    if (estado.salaId === sala) return mostrar('sala');
    if (!estado.nomeConfirmado || !chaves.pegar(sala)) return mostrarEntrada({ sala });
    return entrarNaSala(sala);
  }
  if (p) history.replaceState(null, '', BASE);
  return estado.nomeConfirmado ? mostrarInicio() : mostrarEntrada({});
}

async function usarConvite(codigo) {
  try {
    const d = await api(`convite/${codigo}`, {});
    chaves.salvar(d.id, d.chave);
    history.replaceState(null, '', `${BASE}s/${d.id}`);
    if (estado.nomeConfirmado) entrarNaSala(d.id);
    else mostrarEntrada({ sala: d.id, nomeSala: d.nome });
  } catch (err) {
    toast(err.message, { erro: true, tempo: 8000 });
    ir('', { substituir: true });
  }
}

/* ============================================================
   Entrada: apelido (e senha, se abriu o endereço de uma sala sem convite)
   ============================================================ */
function mostrarEntrada({ sala = null, nomeSala = null, erro = '' }) {
  estado.entrada = { sala, nomeSala };
  const precisaSenha = !!sala && !chaves.pegar(sala);
  $('entrada-titulo').textContent = !sala ? 'Transmita junto com seus amigos.'
    : nomeSala ? `Te chamaram pra ${nomeSala}.` : 'Entrar na sala.';
  $('entrada-sub').textContent = !sala ? 'Tela, janela ou jogo, com som e em alta qualidade. Sem cadastro.'
    : precisaSenha ? `Sala ${formatarCodigo(sala)}. Confirme seu apelido e digite a senha.` : 'Só confirmar seu apelido.';
  $('campo-senha-sala').hidden = !precisaSenha;
  $('link-inicio').hidden = !sala;
  $('btn-entrar').textContent = sala ? 'Entrar na sala' : 'Continuar';
  $('in-nome').value = estado.nome;
  $('in-senha-sala').value = '';
  $('erro-entrada').textContent = erro;
  if (erro) $('in-senha-sala').setAttribute('aria-invalid', 'true');
  else $('in-senha-sala').removeAttribute('aria-invalid');
  mostrar('entrada');
  if (!estado.nome) $('in-nome').focus();
  else if (precisaSenha) $('in-senha-sala').focus();
  else { $('in-nome').focus(); $('in-nome').select(); }
}

$('form-entrada').addEventListener('submit', async (e) => {
  e.preventDefault();
  const nome = $('in-nome').value.trim().replace(/\s+/g, ' ');
  const { sala } = estado.entrada;
  const precisaSenha = !$('campo-senha-sala').hidden;
  const senha = $('in-senha-sala').value;
  const erro = $('erro-entrada');
  erro.textContent = '';
  $('in-nome').removeAttribute('aria-invalid');
  $('in-senha-sala').removeAttribute('aria-invalid');
  if (!nome) { $('in-nome').setAttribute('aria-invalid', 'true'); erro.textContent = 'Escolha um apelido.'; return $('in-nome').focus(); }
  if (precisaSenha && !senha) { $('in-senha-sala').setAttribute('aria-invalid', 'true'); erro.textContent = 'Digite a senha da sala.'; return $('in-senha-sala').focus(); }

  definirNome(nome);
  estado.nomeConfirmado = true;
  if (!sala) return ir('', { substituir: true });
  if (!precisaSenha) return entrarNaSala(sala);

  const btn = $('btn-entrar');
  btn.disabled = true;
  btn.textContent = 'Entrando…';
  try {
    const d = await api('entrar', { codigo: sala, senha });
    chaves.salvar(d.id, d.chave);
    entrarNaSala(d.id);
  } catch (err) {
    if (err.status === 404) { toast(err.message, { erro: true, tempo: 8000 }); return ir('', { substituir: true }); }
    mostrarEntrada({ sala, erro: err.message });
  } finally {
    btn.disabled = false;
  }
});

function definirNome(nome) {
  estado.nome = nome.slice(0, 24);
  guardar.salvar('nome', estado.nome);
  $('nome-atual').textContent = estado.nome;
  $('avatar-eu').textContent = iniciais(estado.nome);
  $('avatar-eu').style.setProperty('--h', corDe(estado.nome));
}

const dlgNome = $('dlg-nome');
$('btn-nome').addEventListener('click', () => {
  $('in-novo-nome').value = estado.nome;
  $('erro-nome').textContent = '';
  dlgNome.returnValue = '';
  dlgNome.showModal();
});
$('form-nome').addEventListener('submit', (e) => {
  if (e.submitter?.value !== 'ok') return;
  const nome = $('in-novo-nome').value.trim().replace(/\s+/g, ' ');
  if (!nome) { e.preventDefault(); $('erro-nome').textContent = 'Escolha um apelido.'; return; }
  definirNome(nome);
});

/* ============================================================
   Início: entrar ou criar
   ============================================================ */
function gerarSenha() {
  const letras = 'abcdefghjkmnpqrstuvwxyz23456789';
  return [...crypto.getRandomValues(new Uint32Array(8))].map((x) => letras[x % letras.length]).join('');
}
const inSalaSenha = $('in-sala-senha');
inSalaSenha.value = gerarSenha();

$('btn-ver-senha').addEventListener('click', (e) => {
  const ver = inSalaSenha.type === 'password';
  inSalaSenha.type = ver ? 'text' : 'password';
  e.currentTarget.setAttribute('aria-pressed', String(ver));
  e.currentTarget.setAttribute('aria-label', ver ? 'Esconder senha' : 'Mostrar senha');
  e.currentTarget.replaceChildren(icone(ver ? 'eye-slash' : 'eye'));
});
$('btn-gerar-senha').addEventListener('click', () => {
  inSalaSenha.value = gerarSenha();
  if (inSalaSenha.type === 'password') $('btn-ver-senha').click();
});

function mostrarInicio() {
  definirNome(estado.nome);
  mostrar('inicio');
  api('info').then((d) => {
    $('capacidade').textContent = d.abertas >= d.max
      ? `O servidor está cheio: ${d.abertas} de ${d.max} salas abertas. Espere uma fechar.`
      : `${d.abertas} de ${d.max} salas abertas agora.`;
  }).catch(() => { $('capacidade').textContent = ''; });
  oferecerApp();
}

/* "Baixar o app": só no Windows e só se o instalador estiver mesmo no servidor
   (sem ele, o servidor devolve o index.html no lugar do .exe) */
let appNoServidor = null;
function oferecerApp() {
  if (!/Windows/.test(navigator.userAgent)) return;
  appNoServidor ??= fetch($('link-baixar-app').href, { method: 'HEAD', cache: 'no-store' })
    .then((r) => r.ok && !(r.headers.get('content-type') || '').includes('text/html'))
    .catch(() => false);
  appNoServidor.then((tem) => { $('baixar-app').hidden = !tem; });
}

/** entende link de convite, endereço de sala, código ou nome */
function lerAlvo(texto) {
  const t = texto.trim();
  const convite = t.match(/\/c\/([a-z0-9]{6,16})/i);
  if (convite) return { convite: convite[1].toLowerCase() };
  const sala = t.match(/\/s\/([a-z0-9]{8})/i);
  if (sala) return { codigo: sala[1].toLowerCase() };
  if (/^[a-z0-9]{4}[\s-]?[a-z0-9]{4}$/i.test(t)) return { codigo: t.replace(/[\s-]/g, '').toLowerCase() };
  return t ? { nome: t } : null;
}

$('form-entrar-sala').addEventListener('submit', async (e) => {
  e.preventDefault();
  const erro = $('erro-entrar-sala');
  const inAlvo = $('in-codigo');
  const inSenha = $('in-codigo-senha');
  erro.textContent = '';
  inAlvo.removeAttribute('aria-invalid');
  inSenha.removeAttribute('aria-invalid');
  const alvo = lerAlvo(inAlvo.value);
  if (!alvo) { inAlvo.setAttribute('aria-invalid', 'true'); erro.textContent = 'Cole o link ou digite o código ou o nome da sala.'; return inAlvo.focus(); }
  if (alvo.convite) { inAlvo.value = ''; return ir(`c/${alvo.convite}`); }
  const senha = inSenha.value;
  if (alvo.codigo && !senha && chaves.pegar(alvo.codigo)) { inAlvo.value = ''; return ir(`s/${alvo.codigo}`); }
  if (!senha) { inSenha.setAttribute('aria-invalid', 'true'); erro.textContent = 'Digite a senha da sala.'; return inSenha.focus(); }

  const btn = $('btn-entrar-sala');
  btn.disabled = true;
  btn.textContent = 'Entrando…';
  try {
    const d = await api('entrar', { ...(alvo.codigo ? { codigo: alvo.codigo } : { nome: alvo.nome }), senha });
    chaves.salvar(d.id, d.chave);
    inAlvo.value = '';
    inSenha.value = '';
    ir(`s/${d.id}`);
  } catch (err) {
    erro.textContent = err.message;
    (err.status === 401 ? inSenha : inAlvo).setAttribute('aria-invalid', 'true');
  } finally {
    btn.disabled = false;
    btn.textContent = 'Entrar';
  }
});

$('form-criar').addEventListener('submit', async (e) => {
  e.preventDefault();
  const nome = $('in-sala-nome').value.trim();
  const senha = inSalaSenha.value;
  const erro = $('erro-criar');
  erro.textContent = '';
  $('in-sala-nome').removeAttribute('aria-invalid');
  inSalaSenha.removeAttribute('aria-invalid');
  if (senha.length < 4) { inSalaSenha.setAttribute('aria-invalid', 'true'); erro.textContent = 'A senha precisa ter pelo menos 4 caracteres.'; return inSalaSenha.focus(); }

  const btn = $('btn-criar');
  btn.disabled = true;
  btn.textContent = 'Criando…';
  try {
    const d = await api('salas', { nome, senha, ocioso: +$('in-ocioso').value });
    chaves.salvar(d.id, d.chave);
    donos.salvar(d.id, d.dono);
    senhas.salvar(d.id, senha);
    $('in-sala-nome').value = '';
    inSalaSenha.value = gerarSenha();
    estado.convidarAoEntrar = true;
    ir(`s/${d.id}`);
  } catch (err) {
    erro.textContent = err.message;
  } finally {
    btn.disabled = false;
    btn.textContent = 'Criar sala';
  }
});

/* ============================================================
   Entrar / sair da sala
   ============================================================ */
const el = {
  status: $('status-conexao'), salaNome: $('sala-nome'), btnCodigo: $('btn-codigo'), qtd: $('qtd-sala'),
  btnTransmitir: $('btn-transmitir'), aoVivo: $('ao-vivo'), tempoNoAr: $('tempo-no-ar'),
  btnParar: $('btn-parar'), btnPessoas: $('btn-pessoas'), btnAjustesSala: $('btn-ajustes-sala'),
  palcoArea: $('palco-area'), vazio: $('vazio'), palco: $('palco'), palcoVideo: $('palco-video'),
  faixa: $('faixa'), mosaico: $('mosaico'), avisoOcioso: $('aviso-ocioso'),
  carregando: $('carregando'), stats: $('stats'), quem: $('quem-assiste'), semSom: $('sem-som'),
  btnMudo: $('btn-mudo'), volume: $('volume'),
  btnStats: $('btn-stats'), btnPip: $('btn-pip'), btnTeatro: $('btn-teatro'), btnCheia: $('btn-cheia'),
  pessoas: $('lista-pessoas'),
  doca: $('doca'), popTx: $('pop-tx'), txInfo: $('tx-info'), txAviso: $('tx-aviso'), btnTrocar: $('btn-trocar'),
  opRes: $('op-res'), opFps: $('op-fps'), opBitrate: $('op-bitrate'), opCodec: $('op-codec'), opAudio: $('op-audio'),
  luzes: [$('luz-e'), $('luz-c'), $('luz-d')],
};
// o vídeo do palco é um só; troca a transmissão ligada nele
const videoPalco = h('video', { playsinline: true, muted: true, 'aria-label': 'Transmissão em destaque' });
el.palcoVideo.append(videoPalco);

async function entrarNaSala(id) {
  const chave = chaves.pegar(id);
  if (!chave) return mostrarEntrada({ sala: id });
  estado.salaId = id;
  estado.sala = null;
  estado.meta = null;
  el.salaNome.textContent = '';
  el.btnCodigo.textContent = formatarCodigo(id);
  mostrar('sala');
  setStatus('Conectando…');
  render();
  try {
    const d = await api('token', { chave, nome: estado.nome });
    if (estado.salaId !== id) return;
    chaves.salvar(id, d.chave);
    estado.sala = d.sala;
    pintarCabecalho();
    mostrar('sala');
    await conectar(d.token);
    if (estado.convidarAoEntrar && estado.salaId === id) {
      estado.convidarAoEntrar = false;
      abrirConvite();
    }
  } catch (err) {
    if (estado.salaId !== id) return;
    estado.salaId = null;
    estado.convidarAoEntrar = false;
    if (err.status === 404) { chaves.apagar(id); donos.apagar(id); }
    toast(err.message, { erro: true, tempo: 8000 });
    ir('', { substituir: true });
  }
}

function pintarCabecalho() {
  if (!estado.sala) return;
  el.salaNome.textContent = estado.sala.nome;
  el.btnCodigo.textContent = estado.sala.codigo;
  // sala sem nome se chama pelo código: não repete o mesmo texto duas vezes
  el.salaNome.classList.toggle('sr', estado.sala.nome === estado.sala.codigo);
  el.btnAjustesSala.hidden = !donos.pegar(estado.sala.id);
  document.title = `${estado.sala.nome} · StreamStudio`;
}
el.btnCodigo.addEventListener('click', () => copiar(el.btnCodigo.textContent));

async function sairDaSala() {
  if (!room && !estado.salaId) return;
  await pararTransmissao(true);
  saindoDeProposito = true;
  const r = room;
  room = null;
  estado.salaId = null;
  estado.sala = null;
  estado.meta = null;
  estado.foco = null;
  for (const s of streams.values()) desligarStream(s);
  streams.clear();
  limparTiles();
  pintarLuz(null);
  if (document.pictureInPictureElement) document.exitPictureInPicture().catch(() => {});
  alternarTeatro(false);
  if (cheiaFalsa()) alternarCheiaFalsa(false);
  await r?.disconnect();
  saindoDeProposito = false;
  pintarTx();
}

async function conectar(token) {
  const r = new Room({
    adaptiveStream: false,   // a qualidade é escolhida à mão: destaque sempre no máximo
    dynacast: true,          // quem transmite para de mandar camadas que ninguém está vendo
    disconnectOnPageLeave: true,
    webAudioMix: IOS,
  });
  room = r;

  r.on(RoomEvent.ParticipantConnected, (p) => { toast(`${nomeDe(p)} entrou`); agendar(); })
    .on(RoomEvent.ParticipantDisconnected, (p) => { toast(`${nomeDe(p)} saiu`); agendar(); })
    .on(RoomEvent.TrackPublished, (pub, p) => {
      if (pub.source === Track.Source.ScreenShare) {
        if (estado.foco && estado.foco !== p.identity) {
          toast(`${nomeDe(p)} começou a transmitir`, { acao: { rotulo: 'Assistir', fn: () => focar(p.identity) }, tempo: 8000 });
        } else {
          toast(`${nomeDe(p)} começou a transmitir`);
        }
      }
      agendar();
    })
    .on(RoomEvent.TrackUnpublished, (pub, p) => {
      if (pub.source === Track.Source.ScreenShare) toast(`${nomeDe(p)} parou de transmitir`);
      agendar();
    })
    .on(RoomEvent.LocalTrackPublished, agendar)
    .on(RoomEvent.LocalTrackUnpublished, agendar)
    .on(RoomEvent.TrackSubscribed, (track) => {
      if (track.kind === Track.Kind.Audio) {
        const a = track.attach();
        a.hidden = true;
        document.body.append(a);
        aplicarVolume();
      }
      agendar();
    })
    .on(RoomEvent.TrackUnsubscribed, (track) => {
      for (const e of track.detach()) if (e.tagName === 'AUDIO') e.remove();
      agendar();
    })
    .on(RoomEvent.TrackSubscriptionFailed, (_sid, p) => {
      toast(`Não deu pra receber a transmissão de ${nomeDe(p)}. Peça pra trocar o codec pra H.264.`, { erro: true, tempo: 8000 });
    })
    .on(RoomEvent.ParticipantAttributesChanged, agendar)
    .on(RoomEvent.ActiveSpeakersChanged, desenharNiveis)
    .on(RoomEvent.RoomMetadataChanged, lerMetadados)
    .on(RoomEvent.ConnectionQualityChanged, (q, p) => { if (p.isLocal) atualizarQualidade(q); })
    .on(RoomEvent.Reconnecting, () => setStatus('Reconectando…', 'ruim'))
    .on(RoomEvent.Reconnected, () => setStatus('Conectado', 'ok'))
    .on(RoomEvent.Disconnected, async (motivo) => {
      if (saindoDeProposito || room !== r) return;
      const id = estado.salaId;
      if (motivo === DisconnectReason.ROOM_DELETED) {
        chaves.apagar(id);
        donos.apagar(id);
        await sairDaSala();
        toast('A sala foi fechada.', { tempo: 8000 });
        ir('', { substituir: true });
        return;
      }
      setStatus('Desconectado', 'ruim');
      toast('A conexão caiu.', {
        erro: true, tempo: 15000,
        acao: { rotulo: 'Reconectar', fn: async () => { await sairDaSala(); entrarNaSala(id); } },
      });
    })
    .on(RoomEvent.AudioPlaybackStatusChanged, () => {
      if (!r.canPlaybackAudio) toast('O navegador bloqueou o som.', { acao: { rotulo: 'Ativar som', fn: () => r.startAudio() }, tempo: 15000 });
    });

  await r.connect(URL_LIVEKIT, token, { autoSubscribe: false });
  if (room !== r) return r.disconnect();
  setStatus('Conectado', 'ok');
  lerMetadados();
  aplicarVolume();
  render();
}

function lerMetadados() {
  try { estado.meta = JSON.parse(room?.metadata || 'null'); } catch { estado.meta = null; }
  if (estado.sala && estado.meta?.ocioso) estado.sala.ocioso = estado.meta.ocioso;
  atualizarAvisoOcioso();
}

/* aviso de que a sala vai fechar por ninguém estar transmitindo */
function atualizarAvisoOcioso() {
  const m = estado.meta;
  if (!room || streams.size || !m?.semStreamDesde) { el.avisoOcioso.hidden = true; return; }
  const resta = m.semStreamDesde + m.ocioso * 60 - Date.now() / 1000;
  el.avisoOcioso.hidden = false;
  el.avisoOcioso.classList.toggle('urgente', resta <= 60);
  el.avisoOcioso.replaceChildren('Ninguém transmitindo. A sala fecha em ', h('b', {}, minSeg(resta)), '.');
}
setInterval(atualizarAvisoOcioso, 1000);

/* ============================================================
   Status
   ============================================================ */
function setStatus(txt, tipo = '') {
  el.status.textContent = txt;
  el.status.className = 'status ' + tipo;
}
function atualizarQualidade(q) {
  const nomes = {
    [ConnectionQuality.Excellent]: ['Conexão excelente', 'ok'],
    [ConnectionQuality.Good]: ['Conexão boa', 'ok'],
    [ConnectionQuality.Poor]: ['Conexão fraca', 'ruim'],
    [ConnectionQuality.Lost]: ['Conexão perdida', 'ruim'],
  };
  const [t, c] = nomes[q] || ['Conectado', 'ok'];
  setStatus(t, c);
}

/* ============================================================
   Transmissões da sala (a sua também aparece)
   ============================================================ */
function coletar() {
  const vistos = new Set();
  const por = (id, dados) => {
    vistos.add(id);
    const s = streams.get(id) || { som: false };
    Object.assign(s, dados);
    streams.set(id, s);
  };
  if (room) {
    for (const p of room.remoteParticipants.values()) {
      const pubV = p.getTrackPublication(Track.Source.ScreenShare);
      if (pubV) por(p.identity, { p, pubV, pubA: p.getTrackPublication(Track.Source.ScreenShareAudio), local: false });
    }
    const minha = minhaPub(Track.Source.ScreenShare);
    if (minha?.track) por(room.localParticipant.identity, { p: room.localParticipant, pubV: minha, pubA: null, local: true });
  }
  for (const [id, s] of streams) if (!vistos.has(id)) { desligarStream(s); streams.delete(id); }
  if (estado.foco && !streams.has(estado.foco)) estado.foco = null;
  if (!estado.foco && streams.size) focar(streams.keys().next().value, { silencioso: true });
}

function desligarStream(s) {
  if (s.local) return;
  try { s.pubV?.setSubscribed(false); s.pubA?.setSubscribed(false); } catch {}
}

function focar(identity, { silencioso = false } = {}) {
  if (!streams.has(identity)) return;
  const mudou = estado.foco !== identity;
  estado.foco = identity;
  if (estado.layout === 'mosaico' && !silencioso) definirLayout('foco');
  if (mudou) {
    room?.localParticipant.setAttributes({ assistindo: identity }).catch(() => {});
    if (!streams.get(identity).local) el.carregando.hidden = false;
  }
  agendar();
}

let pendente = false;
function agendar() {
  if (pendente) return;
  pendente = true;
  requestAnimationFrame(() => { pendente = false; render(); });
}

/* liga uma track num <video>, sem religar se já estiver ligada */
function ligarVideo(video, track) {
  if (video._track === track) return;
  video._track?.detach(video);
  video._track = track || null;
  if (track) track.attach(video);
}

const tiles = { faixa: new Map(), mosaico: new Map() };
const rotuloDe = (s) => (s.local ? 'Você' : nomeDe(s.p));

function criarTile(identity) {
  const video = h('video', { playsinline: true, muted: true, 'aria-hidden': 'true' });
  const nome = h('span', { class: 'truncar' });
  const som = h('button', { class: 'tile-som', type: 'button' });
  som.addEventListener('click', (e) => {
    e.stopPropagation();
    const s = streams.get(identity);
    if (!s) return;
    s.som = !s.som;
    agendar();
  });
  const abrir = h('button', { class: 'tile-abrir', type: 'button', onclick: () => focar(identity) });
  abrir.addEventListener('dblclick', () => { focar(identity); setTimeout(alternarTelaCheia, 50); });
  const tile = h('div', { class: 'tile' }, video, abrir, h('div', { class: 'tile-rodape' }, h('span', { class: 'ponto-rec', 'aria-hidden': 'true' }), nome, som));
  Object.assign(tile, { _video: video, _nome: nome, _som: som, _abrir: abrir });
  return tile;
}

function sincronizarTiles(container, mapaTiles, ids, comSom) {
  for (const [id, tile] of mapaTiles) {
    if (!ids.includes(id)) { ligarVideo(tile._video, null); tile.remove(); mapaTiles.delete(id); }
  }
  ids.forEach((id, i) => {
    const s = streams.get(id);
    let tile = mapaTiles.get(id);
    if (!tile) { tile = criarTile(id); mapaTiles.set(id, tile); }
    if (container.children[i] !== tile) container.insertBefore(tile, container.children[i] || null);
    const nome = rotuloDe(s);
    tile._nome.textContent = nome;
    tile._abrir.setAttribute('aria-label', s.local ? 'Ver sua transmissão' : `Assistir ${nome}`);
    tile.classList.toggle('eu', s.local);
    tile._som.hidden = !comSom || !s.pubA;
    const ligado = somEfetivo(id);
    tile._som.setAttribute('aria-pressed', String(ligado));
    tile._som.setAttribute('aria-label', `${ligado ? 'Desligar' : 'Ligar'} o som de ${nome}`);
    tile._som.replaceChildren(icone(ligado ? 'speaker-high' : 'speaker-slash'));
    ligarVideo(tile._video, s.pubV.track);
  });
}

function limparTiles() {
  for (const m of Object.values(tiles)) {
    for (const tile of m.values()) { ligarVideo(tile._video, null); tile.remove(); }
    m.clear();
  }
  ligarVideo(videoPalco, null);
}

/** no foco só toca o som de quem está em destaque; no mosaico cada um escolhe. O seu nunca toca. */
function somEfetivo(id) {
  const s = streams.get(id);
  if (!s || s.local) return false;
  return estado.layout === 'foco' ? id === estado.foco : !!s.som;
}

function render() {
  if (telas.sala.hidden) return;
  coletar();
  const ids = [...streams.keys()];
  const temAlgo = ids.length > 0;
  pintarFavicon(temAlgo);
  const foco = estado.layout === 'foco';

  el.vazio.hidden = temAlgo;
  if (!temAlgo) el.vazio.querySelector('h2').textContent = room ? 'Ninguém transmitindo ainda' : 'Entrando na sala…';

  el.palco.hidden = !(foco && temAlgo);
  const outros = ids.filter((id) => id !== estado.foco);
  el.faixa.hidden = !(foco && outros.length);
  if (foco && temAlgo) {
    const s = streams.get(estado.foco);
    ligarVideo(videoPalco, s.pubV.track);
    el.quem.textContent = s.local ? 'Você' : nomeDe(s.p);
    // a sua prévia nunca toca som (daria eco), então volume não faz sentido nela
    el.btnMudo.hidden = s.local;
    el.volume.hidden = s.local;
    el.semSom.hidden = !s.local && !!s.pubA;
    el.semSom.lastChild.textContent = s.local ? 'Sua prévia, sem som pra você' : 'Sem som';
    if (s.local || (s.pubV.track && !videoPalco.paused && videoPalco.readyState >= 2)) el.carregando.hidden = true;
    sincronizarTiles(el.faixa, tiles.faixa, outros, false);
  } else {
    ligarVideo(videoPalco, null);
    sincronizarTiles(el.faixa, tiles.faixa, [], false);
  }

  el.mosaico.hidden = !(!foco && temAlgo);
  if (!foco && temAlgo) {
    const n = ids.length;
    el.mosaico.style.setProperty('--cols', n <= 1 ? 1 : n <= 4 ? 2 : n <= 9 ? 3 : 4);
    sincronizarTiles(el.mosaico, tiles.mosaico, ids, true);
  } else {
    sincronizarTiles(el.mosaico, tiles.mosaico, [], true);
  }

  aplicarAssinaturas();
  desenharPessoas();
  pintarTx();
  atualizarAvisoOcioso();
  if (!el.stats.hidden && !streams.size) alternarStats(false);
}

/* decide o que baixar e em que qualidade */
function aplicarAssinaturas() {
  const teatro = document.body.classList.contains('teatro') || !!emTelaCheia();
  const dpr = window.devicePixelRatio || 1;
  for (const [id, s] of streams) {
    if (s.local) continue;
    let visivel, qualidade;
    if (estado.layout === 'foco') {
      visivel = id === estado.foco || !teatro;
      qualidade = id === estado.foco ? VideoQuality.HIGH : VideoQuality.LOW;
    } else {
      visivel = true;
      const altura = (tiles.mosaico.get(id)?.clientHeight || 0) * dpr;
      qualidade = altura >= 480 ? VideoQuality.HIGH : VideoQuality.LOW;
    }
    if (!s.pubV.isSubscribed) s.pubV.setSubscribed(true);
    if (s._visivel !== visivel) { s.pubV.setEnabled(visivel); s._visivel = visivel; }
    if (s._qualidade !== qualidade) { s.pubV.setVideoQuality(qualidade); s._qualidade = qualidade; }
    const som = somEfetivo(id);
    if (s.pubA && s.pubA.isSubscribed !== som) s.pubA.setSubscribed(som);
  }
}
new ResizeObserver(() => { if (estado.layout === 'mosaico') agendar(); }).observe(el.palcoArea);

videoPalco.addEventListener('playing', () => { el.carregando.hidden = true; });
videoPalco.addEventListener('emptied', () => { if (estado.foco && !streams.get(estado.foco)?.local) el.carregando.hidden = false; });

/* ============================================================
   Luz ambiente: a sala pega a cor do vídeo em destaque
   ============================================================ */
const amostra = h('canvas', { width: 24, height: 8 });
const amostraCtx = amostra.getContext('2d', { willReadFrequently: true });

function pintarLuz(cores) {
  el.luzes.forEach((luz, i) => {
    luz.style.backgroundColor = cores?.[i] ? `rgb(${cores[i].join(' ')})` : 'rgb(255 166 77 / .14)';
  });
}

setInterval(() => {
  if (telas.sala.hidden || document.hidden) return;
  const video = estado.layout === 'foco' ? videoPalco : tiles.mosaico.get(estado.foco)?._video;
  if (!video || video.readyState < 2 || !video.videoWidth) return pintarLuz(null);
  try {
    amostraCtx.drawImage(video, 0, 0, 24, 8);
    const px = amostraCtx.getImageData(0, 0, 24, 8).data;
    const cores = [0, 1, 2].map((zona) => {
      let r = 0, g = 0, b = 0, n = 0;
      for (let y = 0; y < 8; y++) {
        for (let x = zona * 8; x < zona * 8 + 8; x++) {
          const i = (y * 24 + x) * 4;
          r += px[i]; g += px[i + 1]; b += px[i + 2]; n++;
        }
      }
      // realça cenas escuras pra luz não sumir
      const m = Math.max(r, g, b, 1) / n;
      const k = Math.min(2.4, 210 / m);
      return [r, g, b].map((c) => Math.min(255, Math.round((c / n) * k)));
    });
    pintarLuz(cores);
  } catch {
    pintarLuz(null);
  }
}, 600);

/* ============================================================
   Pessoas
   ============================================================ */
function desenharPessoas() {
  if (!room) { el.pessoas.replaceChildren(); el.qtd.textContent = ''; return; }
  const todos = [room.localParticipant, ...room.remoteParticipants.values()];
  // O app do StreamStudio assiste com uma segunda conexão "<nome> (player)": é a mesma
  // pessoa, então não entra na lista nem na contagem.
  const lista = todos.filter((p) => !/ \(player\)$/.test(nomeDe(p)));
  el.qtd.textContent = `· ${lista.length}`;

  const vendo = new Map();
  for (const p of todos) {
    const alvo = p.isLocal ? estado.foco : p.attributes?.assistindo;
    if (alvo && alvo !== p.identity) vendo.set(alvo, (vendo.get(alvo) || 0) + 1);
  }
  const transmitindo = (p) => (p.isLocal ? euAoVivo() : streams.has(p.identity));
  lista.sort((a, b) => (transmitindo(b) - transmitindo(a)) || (b.isLocal - a.isLocal));

  el.pessoas.replaceChildren(...lista.map((p) => {
    const nome = nomeDe(p) + (p.isLocal ? ' (você)' : '');
    const vivo = transmitindo(p);
    let sub;
    if (vivo) {
      const n = vendo.get(p.identity) || 0;
      sub = n ? `Transmitindo para ${n}` : 'Transmitindo';
    } else {
      const alvo = p.isLocal ? estado.foco : p.attributes?.assistindo;
      const s = alvo && streams.get(alvo);
      sub = s ? `Assistindo ${s.local ? 'você' : nomeDe(s.p)}` : 'Na sala';
    }
    const clicavel = vivo && p.identity !== estado.foco;
    const li = h('li', { class: 'pessoa' + (vivo ? ' vivo' : '') + (clicavel ? ' clicavel' : ''), 'data-id': p.identity },
      h('span', { class: 'avatar', style: `--h:${corDe(nomeDe(p))}`, 'aria-hidden': 'true' }, iniciais(nomeDe(p))),
      h('span', { class: 'pessoa-nome truncar' }, nome),
      h('span', { class: 'pessoa-sub truncar' }, sub),
      vivo && !p.isLocal ? h('span', { class: 'nivel', 'aria-hidden': 'true' }, h('span')) : null);
    if (clicavel) {
      li.tabIndex = 0;
      li.setAttribute('role', 'button');
      li.setAttribute('aria-label', p.isLocal ? 'Ver sua transmissão' : `Assistir ${nomeDe(p)}`);
      li.addEventListener('click', () => focar(p.identity));
      li.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); focar(p.identity); } });
    }
    return li;
  }));
  desenharNiveis();
}

function desenharNiveis() {
  if (!room) return;
  for (const p of room.remoteParticipants.values()) {
    const barra = el.pessoas.querySelector(`[data-id="${CSS.escape(p.identity)}"] .nivel span`);
    if (barra) barra.style.width = `${Math.min(100, Math.round((p.audioLevel || 0) * 140))}%`;
  }
}

el.btnPessoas.addEventListener('click', () => {
  const esconder = !document.body.classList.contains('sem-lateral');
  document.body.classList.toggle('sem-lateral', esconder);
  el.btnPessoas.setAttribute('aria-expanded', String(!esconder));
  agendar();
});

/* ============================================================
   Layout (foco / mosaico)
   ============================================================ */
function definirLayout(layout) {
  estado.layout = layout;
  guardar.salvar('layout', layout);
  for (const b of document.querySelectorAll('[data-layout]')) b.setAttribute('aria-checked', String(b.dataset.layout === layout));
  if (layout === 'mosaico') {
    for (const [id, s] of streams) s.som = id === estado.foco;
    alternarTeatro(false);
  }
  agendar();
}
for (const b of document.querySelectorAll('[data-layout]')) b.addEventListener('click', () => definirLayout(b.dataset.layout));
definirLayout(estado.layout);

/* ============================================================
   Convite e ajustes da sala
   ============================================================ */
const dlgConvite = $('dlg-convite');
async function abrirConvite() {
  if (!estado.sala) return;
  try { estado.sala = await api('sala', { chave: chaves.pegar(estado.sala.id) }); } catch {}
  $('convite-sala').textContent = estado.sala.nome;
  $('convite-link').value = `${location.origin}${BASE}c/${estado.sala.convite}`;
  $('convite-codigo').value = estado.sala.codigo;
  // só quem criou a sala vê a senha
  const dono = !!donos.pegar(estado.sala.id);
  const senha = dono ? senhas.pegar(estado.sala.id) : null;
  $('campo-convite-senha').hidden = !senha;
  $('convite-sem-senha').hidden = !dono || !!senha;
  $('convite-senha').value = senha || '';
  mostrarSenhaConvite(false);
  dlgConvite.showModal();
}
function mostrarSenhaConvite(ver) {
  const b = $('btn-ver-convite-senha');
  $('convite-senha').type = ver ? 'text' : 'password';
  b.setAttribute('aria-pressed', String(ver));
  b.setAttribute('aria-label', ver ? 'Esconder senha' : 'Mostrar senha');
  b.replaceChildren(icone(ver ? 'eye-slash' : 'eye'));
}
$('btn-ver-convite-senha').addEventListener('click', () => mostrarSenhaConvite($('convite-senha').type === 'password'));
$('btn-convidar').addEventListener('click', abrirConvite);
for (const b of document.querySelectorAll('[data-copiar]')) {
  b.addEventListener('click', () => copiar($(b.dataset.copiar).value));
}

const dlgAjustes = $('dlg-ajustes');
el.btnAjustesSala.addEventListener('click', () => {
  if (!estado.sala) return;
  $('aj-ocioso').value = String(estado.sala.ocioso);
  $('erro-ajustes').textContent = '';
  dlgAjustes.showModal();
});
async function salvarAjustes(extra) {
  const id = estado.sala?.id;
  try {
    estado.sala = await api(`salas/${id}/ajustes`, { dono: donos.pegar(id), ...extra });
    $('erro-ajustes').textContent = '';
    return true;
  } catch (err) {
    $('erro-ajustes').textContent = err.message;
    return false;
  }
}
$('aj-ocioso').addEventListener('change', async () => {
  if (await salvarAjustes({ ocioso: +$('aj-ocioso').value })) toast('Tempo atualizado.', { tempo: 2000 });
});
$('btn-novo-convite').addEventListener('click', async () => {
  if (await salvarAjustes({ novoConvite: true })) toast('Link trocado. O antigo não funciona mais.');
});
$('btn-encerrar').addEventListener('click', async () => {
  const id = estado.sala?.id;
  if (!id || !confirm(`Encerrar ${estado.sala.nome}? Todo mundo sai agora e a sala é apagada.`)) return;
  try {
    await api(`salas/${id}/encerrar`, { dono: donos.pegar(id) });
    dlgAjustes.close();
    chaves.apagar(id);
    donos.apagar(id);
    senhas.apagar(id);
    await sairDaSala();
    toast('Sala encerrada.');
    ir('', { substituir: true });
  } catch (err) {
    $('erro-ajustes').textContent = err.message;
  }
});

/* ============================================================
   Controles do player
   ============================================================ */
function aplicarVolume() {
  const v = estado.mudo ? 0 : estado.volume / 100;
  if (room) {
    for (const p of room.remoteParticipants.values()) p.getTrackPublication(Track.Source.ScreenShareAudio)?.track?.setVolume(v);
  }
  el.volume.value = estado.volume;
  el.btnMudo.replaceChildren(icone(estado.mudo || estado.volume === 0 ? 'speaker-slash' : estado.volume < 50 ? 'speaker-low' : 'speaker-high'));
  el.btnMudo.setAttribute('aria-label', estado.mudo ? 'Ligar o som (M)' : 'Mudo (M)');
  guardar.salvar('volume', estado.volume);
  guardar.salvar('mudo', estado.mudo);
}
el.volume.addEventListener('input', () => { estado.volume = +el.volume.value; estado.mudo = false; aplicarVolume(); });
el.btnMudo.addEventListener('click', () => { estado.mudo = !estado.mudo; aplicarVolume(); });
function mudarVolume(delta) { estado.volume = Math.max(0, Math.min(100, estado.volume + delta)); estado.mudo = false; aplicarVolume(); mostrarControles(); }

const TOQUE = matchMedia('(pointer: coarse)').matches;
/* O iPhone não deixa a página pôr o palco em tela cheia: só o próprio <video>, no player do
   sistema, que mostra barra de tempo e não tem volume. Lá a tela cheia é o palco cobrindo a
   página, com os controles do site. */
const cheiaFalsa = () => document.body.classList.contains('cheia-falsa');
const emTelaCheia = () => document.fullscreenElement || document.webkitFullscreenElement || cheiaFalsa();
function alternarCheiaFalsa(ligar) {
  document.body.classList.toggle('cheia-falsa', ligar);
  document.dispatchEvent(new Event('fullscreenchange'));
}

async function alternarTelaCheia() {
  if (cheiaFalsa()) return alternarCheiaFalsa(false);
  if (emTelaCheia()) {
    screen.orientation?.unlock?.();
    return (document.exitFullscreen || document.webkitExitFullscreen).call(document);
  }
  if (el.palco.hidden) return;
  const pedir = el.palco.requestFullscreen || el.palco.webkitRequestFullscreen;
  if (!pedir) return alternarCheiaFalsa(true);
  try {
    await pedir.call(el.palco);
    // no celular, vídeo deitado gira a tela sozinho, como num player de vídeo
    if (TOQUE && videoPalco.videoWidth > videoPalco.videoHeight) await screen.orientation?.lock?.('landscape').catch(() => {});
  } catch {
    alternarCheiaFalsa(true);
  }
}
el.btnCheia.addEventListener('click', alternarTelaCheia);
videoPalco.addEventListener('dblclick', alternarTelaCheia);
// se o iPhone abrir o player do sistema mesmo assim, o vídeo pausa ao sair; numa live isso só atrapalha
videoPalco.addEventListener('webkitendfullscreen', () => videoPalco.play().catch(() => {}));
document.addEventListener('webkitfullscreenchange', () => document.dispatchEvent(new Event('fullscreenchange')));
document.addEventListener('fullscreenchange', () => {
  const cheia = !!emTelaCheia();
  if (!cheia) screen.orientation?.unlock?.();
  el.btnCheia.replaceChildren(icone(cheia ? 'corners-in' : 'corners-out'));
  el.btnCheia.setAttribute('aria-label', cheia ? 'Sair da tela cheia (F)' : 'Tela cheia (F)');
  agendar();
});

function alternarTeatro(forcar) {
  const ligado = document.body.classList.toggle('teatro', forcar);
  el.btnTeatro.setAttribute('aria-pressed', String(ligado));
  agendar();
}
el.btnTeatro.addEventListener('click', () => alternarTeatro());

if (!document.pictureInPictureEnabled) el.btnPip.hidden = true;
async function alternarPip() {
  try {
    if (document.pictureInPictureElement) await document.exitPictureInPicture();
    else if (videoPalco._track) await videoPalco.requestPictureInPicture();
  } catch {
    toast('Não deu pra abrir a janela flutuante agora.', { erro: true });
  }
}
el.btnPip.addEventListener('click', alternarPip);

let timerOcioso;
function mostrarControles() {
  el.palco.classList.remove('ocioso');
  el.palco.classList.add('mostrar');
  clearTimeout(timerOcioso);
  timerOcioso = setTimeout(() => {
    el.palco.classList.remove('mostrar');
    if (!el.palco.querySelector('.controles:hover, .controles:focus-within')) el.palco.classList.add('ocioso');
  }, 2500);
}
el.palco.addEventListener('pointermove', mostrarControles);
el.palco.addEventListener('touchstart', mostrarControles, { passive: true });

document.addEventListener('keydown', (e) => {
  if (telas.sala.hidden || e.target.closest('input, select, textarea, dialog') || e.ctrlKey || e.metaKey || e.altKey) return;
  const k = e.key.toLowerCase();
  const ids = [...streams.keys()];
  if (k === 'f') alternarTelaCheia();
  else if (k === 't') alternarTeatro();
  else if (k === 'p') alternarPip();
  else if (k === 'g') definirLayout(estado.layout === 'foco' ? 'mosaico' : 'foco');
  else if (k === 'm') { estado.mudo = !estado.mudo; aplicarVolume(); mostrarControles(); }
  else if (k === 'i') alternarStats();
  else if (k === 'arrowup') mudarVolume(5);
  else if (k === 'arrowdown') mudarVolume(-5);
  else if (/^[1-9]$/.test(k) && ids[+k - 1]) focar(ids[+k - 1]);
  else if (k === 'escape' && cheiaFalsa()) alternarCheiaFalsa(false);
  else if (k === 'escape' && document.body.classList.contains('teatro')) alternarTeatro(false);
  else return;
  e.preventDefault();
});

/* ============================================================
   Estatísticas
   ============================================================ */
let timerStats;
function alternarStats(forcar) {
  const ligar = forcar ?? el.stats.hidden;
  if (ligar && !videoPalco._track) return;
  el.stats.hidden = !ligar;
  el.btnStats.setAttribute('aria-pressed', String(ligar));
  clearInterval(timerStats);
  if (ligar) { lerStats(); timerStats = setInterval(lerStats, 1000); }
}
el.btnStats.addEventListener('click', () => alternarStats());

/** a maior saída de vídeo (com simulcast há mais de uma) */
function maiorSaida(rep) {
  let s;
  rep?.forEach((x) => { if (x.type === 'outbound-rtp' && x.kind === 'video' && (!s || (x.frameWidth || 0) > (s.frameWidth || 0))) s = x; });
  return s;
}

/** quantos quadros por segundo a tela entrega pro navegador, antes de codificar */
function fpsCaptura(rep) {
  let fps;
  rep?.forEach((x) => { if (x.type === 'media-source' && x.kind === 'video' && x.framesPerSecond != null) fps = x.framesPerSecond; });
  return fps;
}

async function lerStats() {
  const vt = videoPalco._track;
  if (!vt || el.stats.hidden) return;
  const rep = await vt.getRTCStatsReport();
  const local = streams.get(estado.foco)?.local;
  let s, codec = '?', rtt;
  if (local) s = maiorSaida(rep);
  rep?.forEach((x) => {
    if (!local && x.type === 'inbound-rtp' && x.kind === 'video') s = x;
    if (x.type === 'candidate-pair' && x.nominated && x.currentRoundTripTime != null) rtt = x.currentRoundTripTime;
  });
  if (s?.codecId) codec = rep.get(s.codecId)?.mimeType?.split('/')[1] || '?';
  const perda = !local && s?.packetsReceived ? ((s.packetsLost / (s.packetsReceived + s.packetsLost)) * 100).toFixed(2) : null;
  const captura = local ? fpsCaptura(rep) : null;
  el.stats.textContent = [
    local ? 'Saindo de você' : 'Chegando',
    `Resolução  ${s?.frameWidth || videoPalco.videoWidth}×${s?.frameHeight || videoPalco.videoHeight}`,
    captura != null ? `Captura    ${Math.round(captura)} fps` : null,
    `FPS        ${Math.round(s?.framesPerSecond || 0)}`,
    `Bitrate    ${(vt.currentBitrate / 1e6).toFixed(1)} Mbps`,
    `Codec      ${codec}`,
    perda != null ? `Perda      ${perda}%` : null,
    rtt != null ? `Ping       ${Math.round(rtt * 1000)} ms` : null,
  ].filter(Boolean).join('\n');
}

/* ============================================================
   Sua transmissão: ajustes valem antes e durante a live
   ============================================================ */
function salvarTx() { guardar.salvar('tx', estado.tx); }

function pintarTx() {
  const tx = estado.tx;
  const vivo = euAoVivo();
  el.opRes.value = String(tx.res);
  el.opFps.value = String(tx.fps);
  el.opBitrate.value = String(tx.mbps);
  el.opCodec.value = tx.codec;
  el.opAudio.checked = tx.audio;
  el.txInfo.hidden = !vivo;
  el.btnTrocar.hidden = !vivo;
  el.btnTransmitir.hidden = vivo;
  el.aoVivo.hidden = !vivo;
  el.btnTransmitir.disabled = !room;
  $('aviso-navegador').hidden = !FIREFOX;
}

for (const [campo, sel] of [['res', el.opRes], ['fps', el.opFps], ['mbps', el.opBitrate]]) {
  sel.addEventListener('change', () => { estado.tx[campo] = +sel.value; salvarTx(); aplicarAoVivo([campo]); });
}
el.opCodec.addEventListener('change', () => { estado.tx.codec = el.opCodec.value; salvarTx(); aplicarAoVivo(['codec']); });
el.opAudio.addEventListener('change', async () => {
  estado.tx.audio = el.opAudio.checked;
  salvarTx();
  if (!euAoVivo()) return;
  const pa = minhaPub(Track.Source.ScreenShareAudio);
  if (!estado.tx.audio && pa) { await room.localParticipant.unpublishTrack(pa.track, true); toast('Som desligado.'); }
  if (estado.tx.audio && !pa) toast('Pra levar o som, clique em Trocar tela ou janela e marque Compartilhar áudio.', { tempo: 7000 });
  agendar();
});

function opcoesVideo() {
  const tx = estado.tx;
  const svc = tx.codec === 'vp9' || tx.codec === 'av1';
  const encoding = { maxBitrate: tx.mbps * 1_000_000, maxFramerate: tx.fps, priority: 'high' };
  return {
    source: Track.Source.ScreenShare,
    name: 'tela',
    videoCodec: tx.codec,
    screenShareEncoding: encoding,
    videoEncoding: encoding,
    // sem simulcast: com camadas extras o Chrome larga o H.264 da placa de vídeo e codifica
    // no processador, e o fps despenca. Miniaturas recebem a original mesmo.
    simulcast: false,
    scalabilityMode: svc ? 'L3T3_KEY' : undefined,
    // quem não decodifica VP9/AV1 recebe VP8 (só é gerado se alguém precisar)
    backupCodec: svc ? { codec: 'vp8' } : false,
    degradationPreference: PREFERENCIA,
  };
}
const OPCOES_AUDIO = {
  source: Track.Source.ScreenShareAudio,
  name: 'som',
  audioPreset: { maxBitrate: 192_000, priority: 'high' },
  forceStereo: true,
  dtx: false,
  red: true,
};

async function capturar() {
  const tx = estado.tx;
  const midia = await navigator.mediaDevices.getDisplayMedia({
    video: { width: { ideal: LARGURA[tx.res] }, height: { ideal: tx.res }, frameRate: { ideal: tx.fps, max: tx.fps } },
    audio: tx.audio ? { echoCancellation: false, noiseSuppression: false, autoGainControl: false, channelCount: 2, sampleRate: 48000 } : false,
    systemAudio: 'include',
    // compartilhando uma janela (ex.: o jogo), pega só o som dela, sem o Discord (Chrome/Edge 141+)
    windowAudio: 'window',
    selfBrowserSurface: 'exclude',
    surfaceSwitching: 'include',
    suppressLocalAudioPlayback: false,
  });
  const video = midia.getVideoTracks()[0];
  video.contentHint = DICA_CONTEUDO;
  return { video, audio: midia.getAudioTracks()[0] || null };
}

// botão "Parar compartilhamento" do próprio navegador
function aoAcabarCaptura() { pararTransmissao(); }

let ocupado = false;
async function transmitir() {
  if (!navigator.mediaDevices?.getDisplayMedia) {
    return toast('Este navegador não consegue transmitir a tela. Use Chrome, Edge ou Firefox no computador.', { erro: true, tempo: 8000 });
  }
  if (!room || euAoVivo() || ocupado) return;
  ocupado = true;
  el.btnTransmitir.disabled = true;
  let cap;
  try {
    cap = await capturar();
    const lp = room.localParticipant;
    await lp.publishTrack(cap.video, opcoesVideo());
    cap.video.addEventListener('ended', aoAcabarCaptura);
    if (cap.audio) await lp.publishTrack(cap.audio, OPCOES_AUDIO);
    estado.noArDesde = Date.now();
    if (!estado.foco) estado.foco = lp.identity;
    if (!cap.audio && estado.tx.audio) toast('Você está ao vivo, mas sem som. Pra ter som, troque a fonte e marque Compartilhar áudio.', { tempo: 8000 });
    monitorarTransmissao();
  } catch (err) {
    if (err?.name !== 'NotAllowedError') toast(`Não deu pra transmitir: ${err.message || err}`, { erro: true, tempo: 8000 });
    cap?.video?.stop();
    cap?.audio?.stop();
    await pararTransmissao(true);
  } finally {
    ocupado = false;
    agendar();
  }
}
el.btnTransmitir.addEventListener('click', transmitir);

async function trocarFonte() {
  if (!euAoVivo() || ocupado) return;
  ocupado = true;
  el.btnTrocar.disabled = true;
  try {
    const cap = await capturar();
    const lp = room.localParticipant;
    const vt = minhaPub(Track.Source.ScreenShare).track;
    const antigo = vt.mediaStreamTrack;
    antigo.removeEventListener('ended', aoAcabarCaptura);
    await vt.replaceTrack(cap.video, true);
    antigo.stop();
    cap.video.addEventListener('ended', aoAcabarCaptura);
    await ajustarEncoding(vt);

    const pa = minhaPub(Track.Source.ScreenShareAudio);
    if (cap.audio && pa) {
      const somAntigo = pa.track.mediaStreamTrack;
      await pa.track.replaceTrack(cap.audio, true);
      somAntigo.stop();
    } else if (cap.audio) {
      await lp.publishTrack(cap.audio, OPCOES_AUDIO);
    } else if (pa) {
      await lp.unpublishTrack(pa.track, true);
    }
    toast(cap.audio ? 'Fonte trocada.' : 'Fonte trocada, sem som.');
  } catch (err) {
    if (err?.name !== 'NotAllowedError') toast(`Não deu pra trocar: ${err.message || err}`, { erro: true, tempo: 7000 });
  } finally {
    ocupado = false;
    el.btnTrocar.disabled = false;
    agendar();
  }
}
el.btnTrocar.addEventListener('click', trocarFonte);

/** bitrate e fps da camada principal, sem parar a transmissão */
async function ajustarEncoding(vt) {
  const tx = estado.tx;
  if (vt.encodings?.length) Object.assign(vt.encodings[vt.encodings.length - 1], { maxBitrate: tx.mbps * 1_000_000, maxFramerate: tx.fps });
  const sender = vt.sender;
  if (!sender) return;
  const params = sender.getParameters();
  if (!params.encodings?.length) return;
  Object.assign(params.encodings[params.encodings.length - 1], { maxBitrate: tx.mbps * 1_000_000, maxFramerate: tx.fps });
  try { await sender.setParameters(params); } catch {}
}

async function aplicarAoVivo(campos) {
  const vt = minhaPub(Track.Source.ScreenShare)?.track;
  if (!vt) return;
  const tx = estado.tx;
  const mudou = new Set(campos);
  try {
    if (mudou.has('res') || mudou.has('fps')) {
      await vt.mediaStreamTrack.applyConstraints({
        width: { max: LARGURA[tx.res] }, height: { max: tx.res }, frameRate: { max: tx.fps },
      }).catch(() => {});
    }
    if (mudou.has('mbps') || mudou.has('fps')) await ajustarEncoding(vt);
    if (mudou.has('codec')) {
      // trocar codec exige publicar de novo: a imagem some por um instante
      const lp = room.localParticipant;
      const mst = vt.mediaStreamTrack;
      mst.removeEventListener('ended', aoAcabarCaptura);
      await lp.unpublishTrack(vt, false);
      await lp.publishTrack(mst, opcoesVideo());
      mst.addEventListener('ended', aoAcabarCaptura);
      toast(`Agora em ${tx.codec.toUpperCase()}.`, { tempo: 2500 });
    }
  } catch (err) {
    toast(`Não deu pra aplicar: ${err.message || err}`, { erro: true });
  }
  agendar();
}

let parando = false;
async function pararTransmissao(silencioso = false) {
  if (parando || !room) return;
  const pubs = [minhaPub(Track.Source.ScreenShare), minhaPub(Track.Source.ScreenShareAudio)].filter(Boolean);
  if (!pubs.length) return;
  parando = true;
  clearInterval(timerMonitor);
  for (const pub of pubs) {
    pub.track?.mediaStreamTrack?.removeEventListener('ended', aoAcabarCaptura);
    await room.localParticipant.unpublishTrack(pub.track, true).catch(() => {});
  }
  estado.noArDesde = 0;
  el.txAviso.hidden = true;
  parando = false;
  if (!silencioso) toast('Transmissão encerrada.');
  agendar();
}
el.btnParar.addEventListener('click', () => pararTransmissao());

// o que está saindo de verdade, e se o PC ou a internet estão limitando
let timerMonitor;
function monitorarTransmissao() {
  clearInterval(timerMonitor);
  let ultimosQuadros = -1;
  let segundosParado = 0;
  timerMonitor = setInterval(async () => {
    if (!euAoVivo()) return clearInterval(timerMonitor);
    el.tempoNoAr.textContent = relogio((Date.now() - estado.noArDesde) / 1000);
    const vt = minhaPub(Track.Source.ScreenShare).track;
    const rep = await vt.getRTCStatsReport().catch(() => null);
    const s = maiorSaida(rep);
    if (!s) return;
    // captura x envio: mostra se o fps cai antes (Windows/jogo) ou depois (codificação/internet)
    const captura = fpsCaptura(rep);
    el.txInfo.textContent =
      `${s.frameWidth || '?'}×${s.frameHeight || '?'} · ${(vt.currentBitrate / 1e6).toFixed(1)} Mbps · ${estado.tx.codec.toUpperCase()}\n` +
      (captura != null ? `Captura ${Math.round(captura)} fps → envio ${Math.round(s.framesPerSecond || 0)} fps` : `Envio ${Math.round(s.framesPerSecond || 0)} fps`);
    // nenhum quadro novo por uns segundos: quase sempre é janela minimizada
    const quadros = s.framesSent ?? s.framesEncoded ?? 0;
    segundosParado = quadros === ultimosQuadros ? segundosParado + 1 : 0;
    ultimosQuadros = quadros;
    const aviso = segundosParado >= 6
      ? 'Nenhuma imagem nova há alguns segundos. Se a janela transmitida está minimizada, restaure ela: o Windows não desenha janela minimizada. Transmitir a tela inteira evita isso.'
      : {
        cpu: FIREFOX ? 'O PC está no limite. No Chrome ou no Edge fica bem mais leve.' : 'O PC está no limite. Se o codec estiver em H.264, troque pra VP9; se já estiver, baixe a resolução.',
        bandwidth: 'Sua internet está no limite. Baixe o bitrate.',
      }[s.qualityLimitationReason];
    el.txAviso.hidden = !aviso;
    if (aviso) el.txAviso.textContent = aviso;
  }, 1000);
}

window.addEventListener('beforeunload', (e) => { if (euAoVivo()) e.preventDefault(); });
window.addEventListener('pagehide', () => room?.disconnect());

/* ============================================================
   Início
   ============================================================ */
// o menu da engrenagem abre centrado em cima da doca, sem sair da tela
el.popTx.addEventListener('beforetoggle', (e) => {
  if (e.newState !== 'open') return;
  const r = el.doca.getBoundingClientRect();
  const metade = Math.min(420, innerWidth - 32) / 2;
  el.popTx.style.left = `${Math.max(16 + metade, Math.min(innerWidth - 16 - metade, r.left + r.width / 2))}px`;
  el.popTx.style.bottom = `${innerHeight - r.top + 12}px`;
});

// celular não transmite tela: lá a doca só ocuparia espaço
if (!navigator.mediaDevices?.getDisplayMedia) {
  el.doca.hidden = true;
  $('btn-sala-app').hidden = true;  // o app é só para Windows
  el.vazio.querySelector('p').textContent = 'Quando alguém da sala começar a transmitir, aparece aqui na hora. Pelo celular dá pra assistir; transmitir a tela só pelo computador.';
}
aplicarVolume();
pintarTx();
escolherCodecPadrao();
receberTroca();
rota();
