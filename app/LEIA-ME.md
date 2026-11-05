# StreamStudio para Windows

App para Windows que transmite sua tela para uma sala do StreamStudio.
Seus amigos assistem pelo navegador, como sempre, sem instalar nada.

Por que usar o app em vez de transmitir pelo site:

- **Jogo em tela cheia aparece na lista.** O navegador costuma esconder o jogo
  ou mostrar tudo preto.
- **Você escolhe o som.** Pode mandar tudo menos a chamada do Discord, ou só o
  som de um app (o jogo, por exemplo). Pelo navegador é tudo ou nada.

Quem só vai assistir não precisa do app.

## Requisitos

- **Windows 10 versão 2004 ou mais nova, ou Windows 11**, 64 bits.
  Para ver a sua versão: tecla Windows, digite `winver` e aperte Enter.
  Precisa aparecer "Versão 2004" ou maior (20H2, 21H2, 22H2...).
  O som separado por app não funciona em versões mais antigas.
- **WebView2**: já vem no Windows 10 atualizado e no Windows 11. Se faltar,
  o instalador baixa sozinho (precisa de internet).
- **Processador razoável.** O vídeo é codificado em VP9 pelo processador.
  No PC de teste (Windows 10, placa AMD RX 590) uma transmissão em 1080p usou
  cerca de 16% da CPU. Em PCs mais fracos, baixe a resolução ou o fps nos
  ajustes.
- Internet com boa velocidade de **envio** (upload). O padrão manda vários Mbps.

## Instalar

1. Baixe o instalador em **Criar sala → Baixar o app para transmitir**, no site.
2. Abra o arquivo baixado.
3. Se aparecer **"O Windows protegeu o computador"** (SmartScreen), clique em
   **Mais informações** e depois em **Executar assim mesmo**.
   Isso acontece porque o app não tem assinatura digital paga. É esperado.
4. Siga o instalador. Não pede senha de administrador: o app fica só no seu
   usuário do Windows.

Para desinstalar: **Configurações → Aplicativos**, procure
"StreamStudio" e clique em Desinstalar.

**Atualizações:** a partir da versão 0.2.0 o app se atualiza sozinho. Quando sai
versão nova, aparece o aviso "Versão X disponível" com o botão **Atualizar**: o app
baixa, confere a assinatura, instala e abre de novo. Durante uma transmissão ele
não atualiza; pare a transmissão antes. Quem tem a 0.1.0 precisa baixar o
instalador pelo site uma vez.

## Usar

1. Abra o app, digite seu apelido e **crie uma sala** ou **entre** com código
   e senha. Também dá para clicar em **Abrir no app** num link de convite do
   site: o app abre direto na sala.
2. Clique em **Transmitir**, escolha a janela (ou a tela inteira) e o som:
   - **Tudo menos o Discord:** mistura o som dos jogos e aplicativos, sem o
     Discord nem o próprio app. Sons do Windows e de alguns iniciadores ficam
     de fora. Aplicativos novos podem levar cerca de um segundo para entrar.
   - **Só um app:** só o som do app escolhido. Música, Discord e avisos do
     Windows ficam de fora. O app precisa estar tocando algo para aparecer na
     lista.
   - **Sem som.**
3. Mande o convite pelo botão **Convidar**. Quem criou a sala também vê a senha
   ali, para mandar código e senha.

Durante a transmissão, **Trocar fonte** muda a janela ou o som sem derrubar
ninguém. Resolução e fps mudam na hora pela engrenagem, mesmo ao vivo;
Mbps e codec também, com ~1 s de imagem parada. Se a internet cair, o app tenta voltar sozinho.

## Limitações conhecidas

**Jogos em tela cheia exclusiva.** O app captura a janela do jogo do mesmo
jeito que a gravação de tela do Windows. Quase todo jogo atual funciona. Alguns
jogos antigos, em modo "tela cheia exclusiva", podem aparecer pretos.
Solução: nas opções de vídeo do jogo, troque para **"janela sem bordas"**
(borderless, ou "tela cheia em janela").

**Borda amarela.** É o aviso do Windows de que uma janela está sendo
capturada. Ela só aparece na sua tela, nunca na transmissão.

- **Windows 11:** o app tira a borda sempre.
- **Windows 10:** sem borda para tela inteira, jogo em tela cheia e janela
  maximizada. Nesses casos o app recorta a tela; se outra janela ficar por
  cima da que você transmite, a transmissão **congela** até ela ficar visível
  de novo (para não vazar o que está na frente). Janela não maximizada fica com
  a borda.

**Janela minimizada não transmite imagem.** Deixe a janela aberta, mesmo que
atrás de outras. Janelas protegidas contra captura (alguns players de filme
pago) também saem pretas: quem bloqueia é o próprio player.

**Anti-cheat.** O app **não** injeta nada no jogo (diferente do "captura de
jogo" do OBS). Ele só usa a captura de tela oficial do Windows, a mesma da
Barra de Jogo. Por isso não deve acionar anti-cheat como Vanguard ou Easy
Anti-Cheat. Ainda assim, nenhum programa externo consegue garantir isso por
todos os jogos.

**Discord:**

- Só o Discord **instalado** fica de fora. Discord aberto no **navegador** entra
  junto com o som do navegador. Use o modo **Só um app** nesse caso.
- Discord normal, PTB e Canary ficam fora da mistura, inclusive quando abrem
  ou reiniciam durante o envio. Se a captura não conseguir conferir os apps,
  o som é pausado e o painel mostra o problema.
- O modo geral acrescenta cerca de 100 ms de espera ao som. Use **Só um app**
  se preferir capturar diretamente um jogo específico.

**Aviso de segurança do Windows (SmartScreen).** Aparece ao abrir o
instalador, porque o app não é assinado. Veja "Instalar".
Algum antivírus também pode estranhar um app novo e pouco baixado.

**Só Windows.** Não há versão para Mac ou Linux. Nesses sistemas, transmita
pelo site.

## Seus dados

O app guarda neste PC apenas o apelido, o servidor escolhido, os ajustes de
vídeo e, nas salas que você criou, a senha e a chave de dono. Nada disso vai
para outro lugar além do servidor da sala. O servidor padrão vem de
`SERVIDOR_PADRAO` em `src-tauri/src/api.rs` (troque pelo endereço do seu site
ao compilar); a engrenagem na tela inicial permite usar outro.

## Problemas comuns

| O que acontece | O que fazer |
|---|---|
| O jogo aparece preto | Mude o jogo para "janela sem bordas". Confira se a janela não está minimizada. |
| O app que eu quero não aparece em "Só um app" | Faça o app tocar algum som e clique em **Atualizar a lista** (setas girando). |
| Meus amigos escutam a própria voz | Use "Tudo menos o Discord"; se o Discord estiver no navegador, use "Só um app" e escolha o jogo. |
| O vídeo trava ou fica com poucos fps | Veja o aviso na parte de baixo: ele diz se o gargalo é o PC ou a internet. Baixe a resolução ou o fps. |
| Clicar em "Abrir no app" não faz nada | Abra o app uma vez pelo menu Iniciar; ele se registra para abrir os links. |

Quem desenvolve o app: veja [DESENVOLVIMENTO.md](DESENVOLVIMENTO.md).
