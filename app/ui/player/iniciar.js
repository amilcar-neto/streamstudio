// A falha do primeiro acesso acontece antes de o módulo registrar seus controles.
// Cada nova tentativa recarrega o quadro para não duplicar listeners nem conexões.
window.fecharPlayer = async () => {};
try {
  await import('./app.js?v=app');
} catch (erro) {
  const quadro = document.createElement('main'); quadro.className = 'entrada-corpo';
  quadro.setAttribute('role', 'alert');
  const titulo = document.createElement('h1'); titulo.textContent = 'Não foi possível abrir as transmissões.';
  const motivo = document.createElement('p'); motivo.className = 'erro'; motivo.textContent = String(erro?.message || erro);
  const botao = document.createElement('button'); botao.type = 'button'; botao.className = 'btn btn-primario';
  botao.textContent = 'Tentar novamente'; botao.onclick = () => location.reload();
  quadro.append(titulo, motivo, botao); document.body.replaceChildren(quadro);
}
