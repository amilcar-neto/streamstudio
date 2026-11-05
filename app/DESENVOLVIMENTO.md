# StreamStudio (app): compilar

Guia para quem quer compilar o app. O guia para usuários está em [LEIA-ME.md](LEIA-ME.md).

App [Tauri 2](https://v2.tauri.app) para Windows, com o núcleo em Rust e a
interface em HTML/CSS/JS puros (`ui/`), embutida no executável. As telas usam o
mesmo CSS, fontes e componentes do site (`public/`).

## Como é por dentro

- **Captura de vídeo:** Windows Graphics Capture (WGC) para janelas e telas; no
  Windows 10, DXGI Desktop Duplication para tela inteira e janela maximizada (sem
  a borda amarela). Nada é injetado nos jogos.
- **Captura de som:** WASAPI com *process loopback* (Windows 10 2004+): "só um app"
  ou "tudo menos o Discord", seguindo a árvore de processos.
- **Envio:** SDK Rust do LiveKit (WebRTC), VP9, H.264 (com placa de vídeo quando houver), VP8 ou AV1, com resolução, fps,
  bitrate e codec ajustáveis ao vivo.
- **Atualização automática:** plugin updater do Tauri, com instalador assinado.

Pastas: `src-tauri/` (núcleo e configuração), `ui/` (telas e player) e `spikes/`
(provas de conceito de captura usadas pelos testes).

## Antes de compilar

1. Troque o endereço do seu site em `SERVIDOR_PADRAO` (`src-tauri/src/api.rs`),
   em `PADRAO` (`ui/app.js`) e no campo `in-servidor` (`ui/index.html`).
2. Para a atualização automática, troque `plugins.updater.endpoints` e
   `plugins.updater.pubkey` em `src-tauri/tauri.conf.json` pelos seus
   (veja abaixo como gerar a chave).

## Compilar e abrir

Requisitos: Rust (toolchain MSVC), Build Tools C++ do Visual Studio e WebView2.
Na raiz do repositório:

```powershell
powershell -ExecutionPolicy Bypass -File app/compilar.ps1            # compila
powershell -ExecutionPolicy Bypass -File app/compilar.ps1 -Executar  # compila e abre
powershell -ExecutionPolicy Bypass -File app/compilar.ps1 -Testar    # testes
```

O script deixa dependências e artefatos em `dev/estado/` (fora do git) e usa
`cargo build --locked`. Os testes que conversam com a API sobem o
`api/token_server.py` com Python.

## Instalador

```powershell
npx @tauri-apps/cli@2 signer generate -w segredos/atualizacao.key   # uma vez; guarde a chave em lugar seguro
powershell -ExecutionPolicy Bypass -File app/compilar.ps1 -Instalador
```

Precisa de Node/npm na primeira vez (baixa a CLI do Tauri em `dev/bin/`, sem
instalar nada global). Gera um instalador NSIS por usuário e o `.sig` para a
atualização automática. A chave privada fica em `segredos/`, que o git ignora.
Sem ela, versões novas não passam na conferência de assinatura.

Ao lançar versão nova, suba a versão em `src-tauri/tauri.conf.json` **e**
`src-tauri/Cargo.toml`, publique o instalador e o `atualizacao.json` no seu site.
