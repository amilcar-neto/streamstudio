# Prova WGC (F1.1)

Requer Windows com Windows.Graphics.Capture, Rust MSVC e Build Tools C++.
Não injeta código na janela capturada.

Na raiz do repositório:

```powershell
cargo build --locked --manifest-path app/spikes/wgc/Cargo.toml
& app/spikes/wgc/target/debug/spike-wgc.exe --listar
& app/spikes/wgc/target/debug/spike-wgc.exe --janela 'trecho único do título' --saida dev/estado/wgc
```

O diretório de saída recebe `frame.png` (primeiro quadro) e `metricas.txt`.
Os arquivos com esses nomes são substituídos a cada execução bem-sucedida;
use um diretório novo para cada ensaio. A captura dura 10 segundos, com uma
pequena margem de inicialização e encerramento contabilizada na duração.
Zero quadros, janela fechada e seleção inexistente ou ambígua retornam erro.
Não minimize nem feche a janela durante o ensaio.

Para repetir com conteúdo neutro e animado, execute em outro terminal:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File app/spikes/wgc/janela-teste.ps1
```

Selecione `StreamStudio - teste WGC F1.1` e feche a janela ao terminar.
A janela é uma superfície WinForms de teste, sem proposta visual para o app.

FPS significa callbacks recebidos divididos pelo tempo total. WGC pode deixar
de entregar quadros quando a imagem não muda; uma janela estática não mede
a capacidade máxima do pipeline. O primeiro PNG também custa tempo de captura.
Não há codificação de vídeo, transmissão nem áudio nesta prova.
