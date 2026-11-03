# StreamStudio

Salas privadas pra transmitir tela, janela ou jogo pros amigos, com ou sem som, em até 4K/60 fps, direto do navegador. Sem cadastro. Está no ar e é usado pelo meu grupo de amigos.

## Destaques técnicos

- **Vídeo em tempo real com WebRTC:** servidor de mídia LiveKit (SFU). Resolução, fps, bitrate e codec mudam ao vivo, e dá pra trocar a tela transmitida sem a live cair.
- **Back-end sem dependências:** API de salas em Python usando só a biblioteca padrão. Gera tokens JWT do LiveKit, convites, senhas com PBKDF2, recebe webhook e apaga sozinha as salas vazias ou paradas.
- **Segurança:** limite de tentativas por IP real atrás do Cloudflare, tokens restritos a uma sala, CSP e serviços isolados pelo systemd.
- **Instalação em um comando:** `install.sh` idempotente monta LiveKit, chaves, systemd, firewall e Caddy, com backup e rollback.
- **Front-end sem framework nem build:** HTML, CSS e JavaScript puros (~1.400 linhas no `app.js`).

Feito com Claude Code, usando skills de design e de revisão de segurança, com as decisões de produto e os testes com os amigos feitos por mim.

Depois de instalar, o site fica em `https://<seu-domínio>/stream/`.

## Como funciona

1. Abre o site e confirma o apelido (fica salvo pra próxima vez).
2. **Entrar numa sala**: link de convite (entra direto), ou código + senha, ou nome da sala + senha.
   **Criar sala**: nome, senha e quanto tempo sem ninguém transmitindo até ela fechar (5 min a 1 h).
3. Na sala, **Transmitir** abre o seletor do navegador. Você aparece no palco igual os outros te veem.
4. **Sua transmissão** (painel ao lado): Filme / Jogo / Texto, resolução, fps, bitrate e codec mudam ao vivo. **Trocar tela ou janela** troca a fonte sem a live cair.
5. **Foco** (uma grande + miniaturas) ou **Mosaico** (todas lado a lado, cada uma com seu som).

A sala é **apagada** quando:
- todo mundo sai (depois de 60 s vazia, pra quem só recarregou a página);
- passa o tempo escolhido sem ninguém transmitindo (aparece uma contagem na tela);
- quem criou encerra (Ajustes da sala, a engrenagem, só aparece pra quem criou).

Atalhos na sala: **F** tela cheia · **T** teatro · **P** janela flutuante · **G** foco/mosaico · **M** mudo · **↑ ↓** volume · **I** estatísticas · **1-9** escolhe a transmissão.

## Dicas pra transmitir

- **Use Chrome ou Edge pra transmitir.** No Firefox a imagem trava quando a janela transmitida fica atrás de outra, e o fps cai (ele codifica só no processador). Pra assistir, qualquer navegador serve.
- **Janela minimizada não é capturada** (o Windows para de desenhá-la). Deixe a janela aberta atrás das outras, ou transmita a tela inteira.
- O volume do player é só o quanto **você** ouve. Não muda o que os outros ouvem. O som da sua própria live nunca toca pra você (daria eco).
- Jogo em tela cheia exclusiva pode aparecer preto: use janela sem bordas.
- Netflix/Prime/Disney+ ficam pretos por causa do DRM.
- O painel mostra se o PC ou a internet estão limitando, e avisa se a imagem parou.

## Segurança

- Senha de sala guardada só como hash (PBKDF2).
- 10 tentativas erradas a cada 15 min por pessoa (IP real atrás do Cloudflare, não dá pra forjar); 4 salas criadas por hora por IP; até 5 salas abertas.
- Convite e código são aleatórios; o dono pode trocar o link a qualquer momento.
- Token do LiveKit vale só pra uma sala e só deixa transmitir tela.
- Serviços travados pelo systemd; cabeçalhos com CSP só do próprio site e sem iframe.

## Instalar / atualizar no VPS

Requer Linux com systemd (Debian/Ubuntu), Python 3 e um domínio apontando para o servidor.

```bash
git clone https://github.com/amilcar-neto/streamstudio.git
cd streamstudio && sudo bash install.sh stream.exemplo.com /stream
```

Para atualizar, rode `git pull` e o mesmo `install.sh` (ele é idempotente).

Reinicia o LiveKit (quem estiver numa sala cai e reconecta). Pra mudar só o visual sem derrubar ninguém, basta trocar os arquivos de `/opt/streamamigos/public` e carimbar `__VERSAO__` e `__BASE__` no `index.html`.

Portas: **7882 UDP** (mídia) e **7881 TCP** (pra quem tem UDP bloqueado) liberadas também no firewall do provedor da VPS, se ele tiver um.

## Comandos úteis

| O quê | Comando |
|---|---|
| Limite de salas | `nano /etc/streamamigos/api.env` e `systemctl restart streamamigos-api` |
| Ver salas abertas | `cat /var/lib/streamamigos/salas.json` |
| Ver se está rodando | `systemctl status streamamigos-livekit streamamigos-api` |
| Ver erros | `journalctl -u streamamigos-api -n 50` |

## Créditos de terceiros

- [LiveKit client SDK](https://github.com/livekit/client-sdk-js) (`public/livekit-client.js`), licença Apache 2.0.
- [Phosphor Icons](https://phosphoricons.com), licença MIT.
- Fontes [Geist e Geist Mono](https://vercel.com/font) e [Bricolage Grotesque](https://github.com/ateliertriay/bricolage), licença SIL Open Font License 1.1.
