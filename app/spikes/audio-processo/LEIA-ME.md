# Prova de áudio por processo (F1.2)

Usa o *process loopback* do WASAPI (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`).
Grava WAV PCM 48 kHz, estéreo, 16 bits. Não injeta nada no processo alvo.

Na raiz do repositório:

```powershell
cargo build --locked --manifest-path app/spikes/audio-processo/Cargo.toml
$e = 'app/spikes/audio-processo/target/debug/spike-audio-processo.exe'
& $e --incluir jogo.exe                 # só o som do jogo (e dos filhos)
& $e --excluir Discord.exe              # tudo, menos o Discord
& $e --incluir 1234 --segundos 5        # por PID
& $e --listar                           # PID, pai e nome de todos os processos
& $e --analisar dev/estado/audio/incluir.wav 440,880   # amplitude por frequência
```

- Saída padrão: `dev/estado/audio/<incluir|excluir>.wav`, mais um `.txt` com as
  métricas (duração, pacotes, pico). `--saida arquivo.wav` muda o caminho.
- Por nome, o alvo é a **raiz da árvore**: o processo com aquele nome cujo pai
  não tem o mesmo nome. Os filhos entram pela árvore (o Windows faz isso).
  Se houver mais de uma árvore independente com o mesmo nome, dá erro e pede o PID.
- O `DiscordSystemHelper.exe` não é filho do `Discord.exe` e fica fora da árvore.
  Pelo que se viu, ele não toca o áudio da chamada; conferir na F1.2h.

## Ensaio automático

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File app/spikes/audio-processo/ensaio.ps1
```

Toca 440 Hz (`tom-a.exe`) e 880 Hz (`tom-b.exe`, filho de um powershell) em
volume baixo por ~14 s, grava três modos em paralelo e mede as frequências.
O binário `tom.exe` é só um gerador de seno para testes.
