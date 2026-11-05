import {criarPrevia, abrirPlanos, lerMemoria} from './previa.js';
// Envio nativo no Rust; o player local recebe seu próprio token de recepção.
const $ = (s, raiz = document) => raiz.querySelector(s);
const $$ = (s, raiz = document) => [...raiz.querySelectorAll(s)];
const chamar = (comando, args = {}) => window.__TAURI_INTERNALS__.invoke(comando, args);
const PADRAO = 'https://stream.exemplo.com/stream';
const ler = (k, padrao) => { try { return JSON.parse(localStorage.getItem(k)) ?? padrao; } catch { return padrao; } };
const guardar = (k, v) => localStorage.setItem(k, JSON.stringify(v));
let apelido = ler('apelido', ''), servidor = ler('servidor', PADRAO);
localStorage.removeItem('entrada-pendente');  // chave antiga do botão de retomar
let config, codecAutomatico, sala, fonte, audio = {modo: 'tudo_menos_discord'}, info, aoVivo = false;
let fontes = [], tipo = 'janela', escolhida, ocupado = false, atualizando = false;
// P6: I420 binário; conversão de cor na GPU, no ritmo da tela e sem fila de quadros.
let previaGeracao = 0, previaFila = Promise.resolve();
async function memoriaPrevia(largura, altura) {
  const webview = window.chrome?.webview;
  if (!webview) return null;
  let receber, timer;
  const evento = new Promise((resolve,reject) => {
    receber = e => { if (e.additionalData?.tipo === 'previa-envio') { const buffer=e.getBuffer(), passo=(buffer.byteLength-16)/3; resolve({buffer,passo,capacidade:passo-32}); } };
    webview.addEventListener('sharedbufferreceived',receber);
    timer = setTimeout(() => reject(Error('A prévia não recebeu a memória compartilhada.')),3000);
  });
  // Registra a rejeição desde já, mesmo se o comando nativo falhar antes do evento.
  evento.catch(() => {});
  try { await chamar('preparar_previa',{largura,altura}); return await evento; }
  catch {
    await chamar('preparar_previa',{largura:0,altura:0}).catch(() => {});
    return null; // WebView2 antigo: continua com o transporte binário sem perda.
  } finally { clearTimeout(timer); webview.removeEventListener('sharedbufferreceived',receber); }
}
function lacoPrevia() {
  const geracao=++previaGeracao;
  previaFila=previaFila.catch(() => {}).then(() => rodarPrevia(geracao));
}
async function rodarPrevia(geracao) {
  if(geracao!==previaGeracao || !aoVivo) return;
  let canvas, desenhar, memoria, tentou=false, ultimo=0, ultimaVida=0, mantendo;
  const proximaTela=() => new Promise(requestAnimationFrame);
  const visivel=el => el?.isConnected && !document.hidden && el.getBoundingClientRect().width>0;
  // P7: com o player na tela, a sua transmissão é um quadro dele ("Você"), desenhado aqui.
  const alvo=() => { const player=$('#player-app'); return player ? player.contentWindow?.canvasPreviaNativa?.() : $('.fonte-no-ar canvas'); };
  const liberar=() => { if(memoria) { window.chrome.webview.releaseBuffer(memoria.buffer); memoria=null; } };
  try {
    while(aoVivo && geracao===previaGeracao) {
      const atual=alvo();
      if(!visivel(atual)) { await proximaTela(); continue; }
      if(canvas!==atual) {
        desenhar?.fechar(); canvas=atual; ultimo=0;
        try { desenhar=criarPrevia(canvas); }
        catch(e) { aviso('Não foi possível exibir a prévia: '+e.message); break; }
      }
      const r=canvas.getBoundingClientRect();
      const largura=Math.min(4096,Math.ceil(r.width*devicePixelRatio)), altura=Math.min(4096,Math.ceil(r.height*devicePixelRatio));
      if(!tentou || (memoria && largura*altura*3/2>memoria.capacidade)) {
        tentou=true; liberar(); memoria=await memoriaPrevia(largura,altura); ultimo=0; ultimaVida=0;
        canvas.dataset.transporte=memoria ? 'memoria' : 'binario';
      }
      try {
        if(memoria) {
          // Só sinaliza tamanho/visibilidade quatro vezes por segundo; os pixels
          // chegam pela memória compartilhada, sem uma chamada IPC por quadro.
          if(!mantendo && performance.now()-ultimaVida>=250) {
            ultimaVida=performance.now();
            mantendo=chamar('manter_previa',{largura,altura}).catch(e => { if(canvas.isConnected) canvas.dataset.erro=String(e); }).finally(() => { mantendo=null; });
          }
          const quadro=lerMemoria(memoria,ultimo);
          if(quadro) { const id=desenhar(quadro.bytes,quadro.planos,quadro.valido); if(id) ultimo=id; }
          await proximaTela();
        } else {
          const bytes=await chamar('previa_envio',{largura,altura,ultimo});
          if(!aoVivo || !visivel(canvas) || !bytes.byteLength) { await proximaTela(); continue; }
          const planos=await abrirPlanos(bytes);
          await proximaTela();
          if(aoVivo && visivel(canvas)) ultimo=desenhar(bytes,planos);
        }
      } catch(e) {
        if(aoVivo && canvas.isConnected) canvas.dataset.erro=String(e);
        await proximaTela();
      }
    }
  } finally {
    await mantendo; liberar(); desenhar?.fechar();
    await chamar('preparar_previa',{largura:0,altura:0}).catch(() => {});
  }
}
let assistindo = false;
window.identidadeEnvioNativo = null;
window.acessoPlayer = () => chamar('acesso_player');
let linkPendente = null; // link streamstudio:// esperando a tela de início (F4.1)
const numero = (v, casas = 1) => Number.isFinite(v) ? v.toLocaleString('pt-BR', {maximumFractionDigits: casas, minimumFractionDigits: casas}) : '–';
function aviso(texto) {
  const el = document.createElement('p'); el.className = 'toast'; el.textContent = String(texto);
  $('#toasts').append(el); setTimeout(() => el.remove(), 6000);
}
async function executar(botao, erro, acao) {
  if (ocupado) return;
  ocupado = true; if (botao) botao.disabled = true;
  if (erro) erro.textContent = '';
  try { await acao(); } catch (e) { if (erro) erro.textContent = String(e); else aviso(e); }
  finally {
    ocupado = false; if (botao) botao.disabled = false;
    if (linkPendente && !sala && $('#in-codigo')) queueMicrotask(usarLink);
  }
}
function tela(nome) {
  $('#app').replaceChildren($('#t-' + nome).content.cloneNode(true));
  document.body.classList.toggle('na-sala', ['sala', 'ao-vivo'].includes(nome));
}
function entrada() {
  tela('entrada'); $('#in-nome').value = apelido; $('#in-nome').focus();
  $('.form-entrada').onsubmit = e => {
    e.preventDefault(); const nome = $('#in-nome').value.trim();
    if (!nome) { $('.erro').textContent = 'Digite seu apelido pra continuar.'; return; }
    apelido = nome; guardar('apelido', nome); inicio();
  };
}
function novaSenha() {
  const bytes = crypto.getRandomValues(new Uint8Array(12));
  $('#in-sala-senha').value = [...bytes].map(x => (x % 36).toString(36)).join('');
}
function inicio() {
  tela('inicio');
  $('.chip-eu .truncar').textContent = apelido;
  $('.chip-eu .avatar').textContent = apelido.slice(0,2).toUpperCase();
  $('.chip-eu').onclick = entrada;
  const forms = $$('.form-painel');
  forms.forEach((form, i) => {
    $('[data-submit]', form).onclick = () => form.requestSubmit();
    form.onsubmit = e => {
      e.preventDefault();
      executarEntrada(form, i === 1);
    };
  });
  function executarEntrada(form, criar) {
    const tentativa = executar($('[data-submit]', form), $('.erro', form), async () => {
      let preparada;
      if (criar) {
        preparada = await chamar('preparar_sala', {servidor, nome: $('#in-sala-nome').value,
          senha: $('#in-sala-senha').value, ocioso: Number($('#in-ocioso').value)});
        const donos = ler('salas-criadas', {});
        donos[`${servidor}/${preparada.id}`] = {dono: preparada.dono, senha: $('#in-sala-senha').value};
        guardar('salas-criadas', donos);
        // Se a entrada falhar, a sala já existe: o botão "Voltar para a sala" leva até ela.
        guardar('sala-para-voltar', {servidor, id: preparada.id, codigo: preparada.codigo, nome: preparada.nome, chave: preparada.chave});
      }
      const resposta = await chamar('entrar_sala', {servidor, apelido, criar: false, chave: preparada?.chave,
        alvo: criar ? $('#in-sala-nome').value : $('#in-codigo').value,
        senha: criar ? $('#in-sala-senha').value : $('#in-codigo-senha').value,
        ocioso: Number($('#in-ocioso').value)});
      sala = resposta.sala; mostrarSala(); preencherSala(resposta);
    });
    if (criar) tentativa.then(() => { if (!sala && $('#in-codigo')) mostrarRetomada(); });
  }
  /* Quem sai de uma sala que continua aberta ganha um botão para voltar. Só aparece depois de
     conferir no servidor que a sala ainda existe, para não piscar um botão que não leva a nada. */
  let conferencia = 0;
  async function mostrarRetomada() {
    $('#btn-retomar')?.remove();
    const vez = ++conferencia;
    const alvo = ler('sala-para-voltar', null);
    if (!alvo?.chave || alvo.servidor !== servidor) return;
    let aberta;
    try { aberta = await chamar('conferir_sala', {servidor, chave: alvo.chave}); }
    catch { return; }
    if (vez !== conferencia || sala || !forms[1].isConnected) return;
    if (!aberta) { localStorage.removeItem('sala-para-voltar'); return; }
    const botao = document.createElement('button');
    botao.id = 'btn-retomar'; botao.type = 'button'; botao.className = 'btn btn-contorno';
    botao.textContent = `Voltar para a sala ${aberta.nome || aberta.codigo}`;
    forms[1].append(botao);
    botao.onclick = () => executar(botao, $('.erro', forms[1]), async () => {
      try {
        const resposta = await chamar('entrar_sala', {servidor, apelido, criar: false, alvo: alvo.id || aberta.id,
          senha: '', ocioso: 5, chave: alvo.chave});
        sala = resposta.sala; mostrarSala(); preencherSala(resposta);
      } catch (e) {
        if (/não existe mais|fechou/i.test(String(e))) { localStorage.removeItem('sala-para-voltar'); botao.remove(); }
        throw e;
      }
    });
  }
  mostrarRetomada();
  const codigo = $('#in-codigo');
  codigo.oninput = () => {
    const convite = /(?:\/c\/|streamstudio:\/\/c\/)[a-z0-9]{6,16}/i.test(codigo.value);
    $('#in-codigo-senha').closest('.campo').hidden = convite;
  }; codigo.oninput();
  novaSenha();
  $('[aria-label="Gerar outra senha"]').onclick = novaSenha;
  $('[aria-label="Mostrar senha"]').onclick = e => {
    const b = e.currentTarget, input = $('#in-sala-senha');
    input.type = input.type === 'password' ? 'text' : 'password';
    b.setAttribute('aria-pressed', String(input.type === 'text'));
    b.setAttribute('aria-label', input.type === 'text' ? 'Esconder senha' : 'Mostrar senha');
  };
  const dlg = $('#dlg-servidor'); $('#in-servidor').value = servidor;
  $('.status', dlg).textContent = 'Confira o endereço antes de salvar.';
  // Servidor é coisa de desenvolvimento: sem botão, só pelo atalho Ctrl+Shift+S.
  document.onkeydown = e => { if (e.ctrlKey && e.shiftKey && e.code === 'KeyS' && $('#dlg-servidor')) dlg.showModal(); };
  $('.btn-fantasma', dlg).onclick = () => { $('#in-servidor').value = PADRAO; };
  $('form', dlg).onsubmit = e => {
    e.preventDefault(); executar($('[type=submit]', dlg), $('.status', dlg), async () => {
      servidor = await chamar('conferir_servidor', {servidor: $('#in-servidor').value.trim()});
      guardar('servidor', servidor); dlg.close(); aviso('Servidor salvo.');
    });
  };
  if (linkPendente) usarLink();
}
// Links streamstudio://c/<convite> e streamstudio://s/<código> chegam pelo Rust (link.rs).
async function verLink() {
  let l; try { l = await chamar('pegar_link'); } catch { return; }
  if (!l) return;
  if (sala) {
    const mesma = l.tipo === 'convite' ? sala.convite === l.id : sala.codigo.replace(/-/g, '') === l.id;
    if (mesma) { aviso('Você já está nessa sala.'); return; }
    linkPendente = l; aviso('Você já está numa sala. Saia dela para abrir o link.');
    return;
  }
  linkPendente = l;
  // F4.6: veio do site com o nome; não precisa perguntar de novo.
  if (l.nome && l.nome !== apelido) { apelido = l.nome; guardar('apelido', apelido); }
  if ($('#in-codigo')) usarLink();
  else if ($('#in-nome') && apelido) inicio();
  else if ($('#in-nome')) aviso('Digite seu apelido para entrar na sala do link.');
}
function usarLink() {
  if (ocupado || !linkPendente || !$('#in-codigo')) return;
  const l = linkPendente; linkPendente = null;
  const campo = $('#in-codigo'), form = campo.closest('form');
  if (l.chave) {
    // F4.6: troca do site para o app. A chave do site deixa entrar sem senha nem convite.
    const id = l.tipo === 'codigo' ? l.id : null;
    executar($('[data-submit]', form), $('.erro', form), async () => {
      const resposta = await chamar('entrar_sala', {servidor, apelido, criar: false, alvo: l.id,
        senha: '', ocioso: 5, chave: l.chave});
      sala = resposta.sala;
      if (l.dono && (!id || id === sala.id)) {
        const donos = ler('salas-criadas', {}), atual = donos[`${servidor}/${sala.id}`];
        donos[`${servidor}/${sala.id}`] = {dono: l.dono, senha: atual?.dono === l.dono ? atual.senha || '' : ''};
        guardar('salas-criadas', donos);
      }
      mostrarSala(); preencherSala(resposta);
    });
    return;
  }
  campo.value = `streamstudio://${l.tipo === 'convite' ? 'c' : 's'}/${l.id}`; campo.oninput();
  if (l.tipo === 'convite') { form.requestSubmit(); return; }
  $('#in-codigo-senha').value = ''; $('#in-codigo-senha').focus();
  $('.erro', form).textContent = 'Digite a senha da sala para entrar.';
}
function preencherSala(p) {
  window.marcaEnvioNativo = p.marca_envio;
  window.identidadeEnvioNativo = p.identidade_envio;
  $('#player-app')?.contentWindow.sincronizarEnvioNativo?.();
  sala = p.sala;
  $('.sala-nome').textContent = sala.nome || sala.codigo;
  $('.chip-codigo').textContent = sala.codigo;
  const status = $('#status-conexao');
  status.textContent = p.conexao === 'conectado' ? 'Conectado' : p.conexao === 'encerrada' ? 'Sala desconectada' : 'Reconectando…';
  status.className = 'status ' + (p.conexao === 'conectado' ? 'ok' : 'ruim');
  $('#pessoas-tit .num').textContent = p.pessoas.length;
  const lista = $('.lista-pessoas'); lista.replaceChildren();
  for (const pessoa of p.pessoas) {
    const li = document.createElement('li'); li.className = 'pessoa';
    const avatar = document.createElement('span'); avatar.className = 'avatar'; avatar.setAttribute('aria-hidden','true');
    avatar.textContent = pessoa.nome.slice(0,2).toUpperCase();
    const nome = document.createElement('span'); nome.className = 'pessoa-nome truncar';
    nome.textContent = pessoa.nome + (pessoa.voce ? ' (você)' : '');
    const sub = document.createElement('span'); sub.className = 'pessoa-sub truncar';
    sub.textContent = pessoa.voce && aoVivo ? 'Transmitindo' : 'Na sala';
    li.append(avatar, nome, sub); lista.append(li);
  }
}
async function copiar(texto) {
  try { await navigator.clipboard.writeText(texto); aviso('Copiado.'); }
  catch { aviso('Não foi possível copiar.'); }
}
function mostrarSala() {
  localStorage.removeItem('sala-para-voltar');
  tela(aoVivo ? 'ao-vivo' : 'sala');
  $('.sala-nome').textContent = sala.nome || sala.codigo;
  $('.chip-codigo').textContent = sala.codigo;
  $('[data-sair]').onclick = e => {
    e.preventDefault(); executar(e.currentTarget, null, async () => {
      await $('#player-app')?.contentWindow.fecharPlayer?.(); $('#player-app')?.remove(); assistindo = false;
      const saida = await chamar('sair_sala');
      if (saida?.chave) guardar('sala-para-voltar', {servidor, id: saida.sala.id, codigo: saida.sala.codigo, nome: saida.sala.nome, chave: saida.chave});
      aoVivo = false; sala = null; info = null; fonte = null; inicio();
    });
  };
  $('.chip-codigo').onclick = () => copiar(sala.codigo);
  $('.topo-dir .btn-contorno').onclick = e => executar(e.currentTarget, null, convidar);
  $('[aria-label="Abrir no navegador"]').onclick = e => executar(e.currentTarget, null,
    async () => {
      // F4.6: o site abre a mesma sala com o mesmo nome; quem é dono continua dono lá.
      const p = await chamar('estado_sala', {atualizar: true}); sala = p.sala;
      const dono = ler('salas-criadas', {})[`${servidor}/${sala.id}`]?.dono || null;
      await chamar('abrir_navegador', {dono});
      aviso('Abrindo a sala no navegador…');
    });
  $('[data-ir="fonte.html"]').onclick = abrirFontes;
  if (aoVivo) {
    $('[data-ir="sala.html"]').onclick = e => executar(e.currentTarget, null, async () => {
      await chamar('parar_envio'); aoVivo = false; info = null; mostrarSala();
    });
    $('.fonte-no-ar .quem').textContent = fonte.nome;
    $('.fonte-no-ar .sem-som').textContent = nomeAudio();
    $$('.medidor-valor').forEach(el => { el.textContent = '–'; });
    $$('.medidor-sub').forEach(el => { el.textContent = 'Aguardando medição'; });
    $('.ao-vivo .num').textContent = '00:00:00';
    lacoPrevia();
  }
  // P7: a sala mostra as transmissões de todo mundo, como no site, também durante o envio.
  // A sua aparece como "Você", com a prévia local. Foco/Mosaico iguais aos do site.
  assistindo = true;
  for (const b of $$('[data-layout]')) b.onclick = () => $('#player-app')?.contentWindow.definirLayout?.(b.dataset.layout);
  marcarLayout(ler('layout', 'foco'));
  mostrarPlayer();
  ajustes();
  atualizar();
}
async function convidar() {
  const p = await chamar('estado_sala', {atualizar: true});
  if (p.conexao === 'encerrada') throw 'Essa sala foi encerrada.';
  sala = p.sala;
  $('#app').append($('#t-convite').content.cloneNode(true));
  const dlg = $('#dlg-convite');
  $('#convite-sala').textContent = sala.nome || sala.codigo;
  $('#convite-link').value = `${servidor}/c/${sala.convite}`;
  $('#convite-codigo').value = sala.codigo;
  const dono = ler('salas-criadas', {})[`${servidor}/${sala.id}`];
  $('#campo-convite-senha').hidden = !dono?.dono || !dono?.senha;
  $('#convite-senha').value = dono?.dono ? dono.senha || '' : '';
  $('#convite-sem-senha').hidden = true;
  $('#btn-ver-convite-senha').onclick = e => {
    const campo = $('#convite-senha'), botao = e.currentTarget;
    const mostrar = campo.type === 'password'; campo.type = mostrar ? 'text' : 'password';
    botao.setAttribute('aria-pressed', String(mostrar));
    botao.setAttribute('aria-label', mostrar ? 'Esconder senha' : 'Mostrar senha');
    $('i', botao).className = mostrar ? 'ph ph-eye-slash' : 'ph ph-eye';
  };
  $$('[data-copiar]',dlg).forEach(b => { b.onclick = () => copiar($('#' + b.dataset.copiar).value); });
  dlg.addEventListener('close', () => dlg.remove(), {once:true});
  dlg.showModal();
}
function nomeAudio() {
  return audio.modo === 'sem_som' ? 'Sem som' : audio.modo === 'app' ? 'Só o app escolhido' : 'Tudo menos o Discord';
}
function ajustes() {
  const pop = $('#pop-tx');
  $('#op-codec option[value=auto]').textContent = `Automático (${codecAutomatico})`;
  for (const [campo, prop] of [['res','res'],['fps','fps'],['bitrate','mbps'],['codec','codec']]) {
    const el = $('#op-' + campo); el.value = config[prop];
    el.onchange = () => { config[prop] = campo === 'codec' ? el.value : Number(el.value); guardar('video', config); aplicar(); };
  }
  $('#op-svc').checked = config.svc;
  $('#op-svc').onchange = e => { config.svc = e.target.checked; guardar('video', config); aplicar(); };
  // P4: ao vivo, resolução e fps mudam na hora; Mbps, codec e SVC republicam (~1 s parado).
  async function aplicar() {
    resumo();
    if (!aoVivo) return;
    try {
      const [novo, republicou] = await chamar('ajustar_transmissao', {config});
      info = novo; resumo();
      aviso(republicou ? 'Ajustes aplicados. Quem assiste viu cerca de 1 segundo parado.' : 'Ajustes aplicados.');
    } catch (e) { aviso(e); }
  }
  function resumo() {
    $('.tx-info').textContent = `${info?.codec || (config.codec === 'auto' ? codecAutomatico : config.codec.toUpperCase())} · ${config.res}p · ${config.fps} fps · até ${config.mbps} Mbps` +
      (aoVivo ? '\nResolução e fps mudam na hora. Mbps e codec deixam a imagem parada por ~1 s.' : '\nAjustes para a próxima transmissão.');
  } resumo();
  pop.addEventListener('beforetoggle', e => {
    if (e.newState !== 'open') return;
    const b = $('[popovertarget=pop-tx]').getBoundingClientRect();
    const meia = Math.min(420, innerWidth - 32) / 2;
    pop.style.left = `${Math.max(16 + meia, Math.min(innerWidth - 16 - meia, b.left + b.width / 2))}px`;
    pop.style.bottom = `${innerHeight - b.top + 12}px`;
  });
}
function desenharFontes() {
  const dlg = $('#dlg-fonte');
  for (const [valor, tipoFonte] of [['janelas','janela'],['telas','monitor']]) {
    const grade = $(`.fontes[data-valor=${valor}]`, dlg); grade.replaceChildren();
    for (const f of fontes.filter(f => f.tipo === tipoFonte)) {
      const b = document.createElement('button'); b.type = 'button'; b.className = 'fonte';
      b.setAttribute('role','radio'); b.setAttribute('aria-checked', String(escolhida?.id === f.id));
      const mini = document.createElement('span'); mini.className = 'fonte-mini';
      const imagem = (src, classe) => { const img = document.createElement('img'); img.src = src; img.alt = ''; if (classe) img.className = classe; return img; };
      // P2: miniatura; se não houver, o ícone grande do programa; janela minimizada ganha a marca.
      if (f.miniatura) mini.append(imagem(f.miniatura));
      else if (f.icone) mini.append(imagem(f.icone, 'icone-grande'));
      else mini.textContent = 'Sem prévia';
      if (f.minimizada) { const marca = document.createElement('span'); marca.className = 'marca-minimizada'; marca.textContent = 'Minimizada'; mini.append(marca); }
      const nome = document.createElement('span'); nome.className = 'fonte-nome';
      if (f.icone) nome.append(imagem(f.icone, 'icone-app'));
      const texto = document.createElement('span'); texto.className = 'truncar'; texto.textContent = f.nome; nome.append(texto);
      const sub = document.createElement('span'); sub.className = 'fonte-sub num truncar';
      sub.textContent = f.minimizada ? 'Desminimize para transmitir com imagem' : f.aviso ? 'Miniatura indisponível' : `${f.largura}×${f.altura}`;
      b.title = f.aviso || f.nome;
      b.append(mini,nome,sub);
      b.onclick = () => { escolhida = f; tipo = f.tipo; $('#dlg-fonte').padraoSom?.(f); desenharFontes(); };
      grade.append(b);
    }
    if (!grade.children.length) { const p = document.createElement('p'); p.className = 'ajuda'; p.textContent = 'Nenhuma fonte encontrada.'; grade.append(p); }
    grade.hidden = tipoFonte !== tipo;
  }
  $('#confirmar-fonte').disabled = !escolhida || escolhida.tipo !== tipo;
}
async function abrirFontes() {
  if ($('#dlg-fonte')) return;
  $('#app').append($('#t-fonte').content.cloneNode(true));
  const dlg = $('#dlg-fonte'); escolhida = fonte; tipo = fonte?.tipo || 'janela';
  const fechar = () => { dlg.close(); dlg.remove(); };
  $('#cancelar-fonte').onclick = () => { if (!ocupado) fechar(); };
  dlg.addEventListener('cancel', e => { e.preventDefault(); if (!ocupado) fechar(); });
  const grupoTipo = $('[data-mostra=tipo]',dlg), grupoSom = $('[data-mostra=som]',dlg);
  for (const grupo of [grupoTipo,grupoSom]) {
    grupo.onclick = e => {
      const b = e.target.closest('[role=radio]'); if (!b) return;
      $$('[role=radio]', grupo).forEach(x => x.setAttribute('aria-checked', String(x === b)));
      if (grupo === grupoTipo) { tipo = b.dataset.valor === 'telas' ? 'monitor' : 'janela'; desenharFontes(); }
      else {
        $$('[data-de=som]',dlg).forEach(el => { el.hidden = el.dataset.valor !== b.dataset.valor; });
        if (e.isTrusted) dlg.somManual = true;
      }
    };
  }
  // P5: escolher uma janela já põe o som em "só esse app" (sem eco, sem vazar outros sons);
  // tela inteira volta para "tudo menos o Discord". Se a pessoa mexeu no som, fica como ela quis.
  dlg.padraoSom = (f) => {
    if (dlg.somManual) return;
    if (f.tipo === 'janela' && f.pid) {
      const sel = $('#processo-audio');
      if (![...sel.options].some(o => Number(o.value) === f.pid)) sel.add(new Option(`${f.nome} (esta janela)`, f.pid));
      sel.value = f.pid;
      $('[data-valor=app]', grupoSom).click();
    } else $('[data-valor=menos-discord]', grupoSom).click();
  };
  const modo = audio.modo === 'app' ? 'app' : audio.modo === 'sem_som' ? 'sem' : 'menos-discord';
  $(`[data-valor=${modo}]`,grupoSom).click();
  $(`[data-valor=${tipo === 'monitor' ? 'telas' : 'janelas'}]`,grupoTipo).click();
  const refresh = $('[aria-label="Atualizar a lista"]',dlg);
  async function carregar() {
    refresh.disabled = true; $('#confirmar-fonte').disabled = true;
    $('.erro',dlg).textContent = 'Buscando janelas e som…';
    try {
      const [lista, processos] = await Promise.all([chamar('listar_fontes'), chamar('listar_processos_audio')]);
      if (!dlg.isConnected) return;
      fontes = lista; escolhida = lista.find(f => f.id === escolhida?.id);
      const sel = $('#processo-audio'); sel.replaceChildren(new Option('Escolha o app', ''));
      processos.sort((a,b) => Number(b.ativo)-Number(a.ativo)).forEach(p => sel.add(new Option(`${p.nome}${p.ativo ? ' · tocando agora' : ''}`,p.pid)));
      if (audio.modo === 'app') sel.value = audio.pid;
      $('.erro',dlg).textContent = ''; desenharFontes();
    } catch (e) { if (dlg.isConnected) $('.erro',dlg).textContent = String(e); }
    finally { refresh.disabled = false; }
  }
  refresh.onclick = carregar;
  $('form',dlg).onsubmit = e => {
    e.preventDefault(); executar($('#confirmar-fonte'), $('.erro',dlg), async () => {
      if (!escolhida || escolhida.tipo !== tipo) throw 'Escolha uma janela ou tela.';
      const selecionada = escolhida;
      const modo = $('[aria-checked=true]', grupoSom).dataset.valor;
      const som = {modo: modo === 'app' ? 'app' : modo === 'sem' ? 'sem_som' : 'tudo_menos_discord'};
      if (modo === 'app') { som.pid = Number($('#processo-audio').value); if (!som.pid) throw 'Escolha o app que vai com o som.'; }
      if (aoVivo) {
        // Registra cada alteração confirmada, inclusive se a seguinte falhar.
        if (fonte.id !== selecionada.id) await chamar('trocar_fonte', {fonte: selecionada.id}); fonte = selecionada;
        $('.fonte-no-ar .quem').textContent = fonte.nome;
        if (audio.modo !== som.modo || audio.pid !== som.pid) await chamar('trocar_audio', {audio: som}); audio = som;
      } else {
        info = await chamar('iniciar_envio', {fonte: selecionada.id, config, audio: som});
        fonte = selecionada; audio = som; aoVivo = true; assistindo = false;
      }
      fechar(); mostrarSala();
    });
  };
  dlg.showModal(); await carregar();
}
function estatisticas(s) {
  const valores = $$('.medidor-valor'), subs = $$('.medidor-sub');
  const dados = [[`${numero(s.fps_envio,0)} fps`, `captura ${numero(s.fps_captura,0)} / pedido ${s.fps_pedido}`],
    [`${numero(s.mbps)} Mbps`, `até ${config.mbps} Mbps`],
    [`${numero(s.perda_pct)} %`, `ping ${numero(s.ping_ms,0)} ms`],
    [s.som_kbps == null ? 'Sem som' : `${numero(s.som_kbps,0)} kbps`, s.som_kbps == null ? '' : 'Opus estéreo'],
    [s.codec, `${s.largura}×${s.altura}`]];
  dados.forEach(([valor,sub],i) => { valores[i].textContent = valor; subs[i].textContent = sub; });
  $$('.medidor').forEach((el,i) => el.classList.toggle('ruim', (i===0 && ['pc','sem_imagem','fonte_fechada'].includes(s.gargalo)) || (i===1 && ['rede','conexao'].includes(s.gargalo))));
  $('.ao-vivo .num').textContent = new Date(s.no_ar_s * 1000).toISOString().slice(11,19);
  $('#aviso-tx').textContent = s.aviso || ''; $('#aviso-tx').hidden = !s.aviso;
  $('.tx-info').textContent = `${s.codec} · ${s.encoder || 'medindo codificador'}${s.hardware ? ' (placa de vídeo)' : ' (processador)'}\n${s.largura}×${s.altura} · ${s.fps_pedido} fps · até ${config.mbps} Mbps\nPare o envio para mudar estes ajustes.`;
}
async function atualizar() {
  if (!sala || atualizando || ocupado) return;
  atualizando = true;
  try {
    const p = await chamar('estado_sala');
    if (!sala) return;
    if (p.conexao === 'encerrada') {
      await $('#player-app')?.contentWindow.fecharPlayer?.();
      $('#player-app')?.remove(); assistindo = false;
      await chamar('sair_sala'); aoVivo = false; sala = null; info = null; fonte = null;
      window.identidadeEnvioNativo = null; inicio(); aviso('Essa sala foi encerrada. Entre em outra sala para continuar.');
      return;
    }
    preencherSala(p);
    if (aoVivo) {
      const s = await chamar('estatisticas_transmissao'); if (aoVivo) estatisticas(s);
    }
  } catch (e) {
    if ($('#status-conexao')) { $('#status-conexao').textContent = 'Sem conexão'; $('#status-conexao').className = 'status ruim'; }
    if ($('#aviso-tx')) { $('#aviso-tx').textContent = String(e); $('#aviso-tx').hidden = false; }
  } finally { atualizando = false; }
}
let avisoAtualizacao = null; // aviso de versão nova (F5.2)
try {
  const padroes = await chamar('padroes_video'); config = {...padroes.config, ...ler('video', {})};
  codecAutomatico = padroes.codec_auto.toUpperCase();
  linkPendente = await chamar('pegar_link');
  if (apelido) inicio(); else entrada();
  if (linkPendente && !apelido) aviso('Digite seu apelido para entrar na sala do link.');
  setInterval(atualizar, 1000); setInterval(verLink, 500);
  // Procura versão nova ao abrir, a cada 15 min e ao voltar para a janela (no máximo 1x a cada 5 min).
  verAtualizacao(); setInterval(verAtualizacao, 15 * 60e3);
  let ultimaBusca = Date.now();
  addEventListener('focus', () => { if (Date.now() - ultimaBusca > 5 * 60e3) { ultimaBusca = Date.now(); verAtualizacao(); } });
} catch (e) { aviso('Não foi possível iniciar o app: ' + e); }

// F5.2: confere se há versão nova ao abrir e a cada 6 h. Só instala fora de transmissão.
async function verAtualizacao() {
  let nova; try { nova = await chamar('verificar_atualizacao'); } catch { return; }
  if (!nova || avisoAtualizacao) return;
  const el = document.createElement('p'); el.className = 'toast'; el.setAttribute('role', 'status');
  const texto = document.createElement('span'); texto.textContent = `Versão ${nova.versao} disponível.`;
  const botao = document.createElement('button'); botao.type = 'button'; botao.textContent = 'Atualizar';
  botao.onclick = async () => {
    if (aoVivo) { aviso('Pare a transmissão antes de atualizar.'); return; }
    botao.disabled = true; texto.textContent = 'Baixando a atualização…';
    try { await chamar('instalar_atualizacao'); }
    catch (e) { texto.textContent = String(e); botao.disabled = false; }
  };
  el.append(texto, botao); $('#toasts').append(el); avisoAtualizacao = el;
}

// O player avisa quando o layout muda (inclusive pela tecla G) para o seletor do app acompanhar.
function marcarLayout(layout) {
  for (const b of $$('[data-layout]')) b.setAttribute('aria-checked', String(b.dataset.layout === layout));
}
window.layoutPlayer = marcarLayout;
// Ao vivo, o player ocupa o palco até os números do envio; sem envio, até a doca.
function encaixarPlayer() {
  const corpo = $('.sala-corpo'), base = $('.medidores') || $('.doca');
  if (!corpo || !base) return;
  const espaco = Math.max(0, corpo.getBoundingClientRect().bottom - base.getBoundingClientRect().top + 14);
  corpo.style.setProperty('--base-player', espaco + 'px');
}
// Recalcula quando a janela muda e quando a doca cresce (aviso do envio aparecendo, por exemplo).
const observaPlayer = new ResizeObserver(encaixarPlayer);
function mostrarPlayer() {
  $('#player-app')?.remove();
  if (!assistindo) return;
  observaPlayer.disconnect();
  for (const alvo of [document.documentElement, $('.doca'), $('.medidores')]) if (alvo) observaPlayer.observe(alvo);
  requestAnimationFrame(encaixarPlayer);
  const iframe = document.createElement('iframe');
  iframe.id = 'player-app'; iframe.title = 'Transmissões da sala';
  iframe.allow = 'autoplay; fullscreen; picture-in-picture';
  iframe.src = 'player/index.html';
  $('.sala-corpo').prepend(iframe);
}
