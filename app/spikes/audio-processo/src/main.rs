//! Prova F1.2: áudio por processo com WASAPI process loopback.
//!
//! --incluir <exe|pid>  grava só o som do processo (e dos filhos dele)
//! --excluir <exe|pid>  grava tudo, menos o som do processo (e dos filhos)
//! --analisar <wav> <hz,hz,...>  mede a energia de cada frequência no WAV

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient,
    IAudioClient, PROCESS_LOOPBACK_MODE, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::Media::Audio::WAVE_FORMAT_PCM;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{BLOB, COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{IUnknown, Interface, Ref, implement};

const TAXA: u32 = 48_000;
const CANAIS: u16 = 2;
const BITS: u16 = 16;

fn main() {
    if let Err(e) = rodar() {
        eprintln!("erro: {e}");
        std::process::exit(1);
    }
}

fn uso() -> String {
    "uso:\n  spike-audio-processo --incluir <exe|pid> [--segundos 10] [--saida arquivo.wav]\n  \
     spike-audio-processo --excluir <exe|pid> [--segundos 10] [--saida arquivo.wav]\n  \
     spike-audio-processo --listar\n  \
     spike-audio-processo --analisar arquivo.wav 440,880"
        .into()
}

fn rodar() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut modo = None;
    let mut alvo = String::new();
    let mut segundos = 10u64;
    let mut saida: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        let valor = |i: usize| args.get(i + 1).cloned().ok_or_else(|| format!("falta valor para {}\n{}", args[i], uso()));
        match args[i].as_str() {
            "--incluir" => {
                modo = Some(PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE);
                alvo = valor(i)?;
                i += 1;
            }
            "--excluir" => {
                modo = Some(PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE);
                alvo = valor(i)?;
                i += 1;
            }
            "--segundos" => {
                segundos = valor(i)?.parse().map_err(|_| "segundos inválidos")?;
                i += 1;
            }
            "--saida" => {
                saida = Some(valor(i)?.into());
                i += 1;
            }
            "--listar" => return listar(),
            "--analisar" => {
                let wav = valor(i)?;
                let freqs = args.get(i + 2).ok_or_else(uso)?;
                return analisar(Path::new(&wav), freqs);
            }
            _ => return Err(uso()),
        }
        i += 1;
    }
    let modo = modo.ok_or_else(uso)?;
    let procs = processos()?;
    let (pid, desc) = resolver_alvo(&procs, &alvo)?;
    let arvore = descendentes(&procs, pid);
    let nome_modo = if modo == PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE { "incluir" } else { "excluir" };
    let saida = saida.unwrap_or_else(|| PathBuf::from(format!("dev/estado/audio/{nome_modo}.wav")));
    if let Some(p) = saida.parent() {
        fs::create_dir_all(p).map_err(|e| format!("não consegui criar {}: {e}", p.display()))?;
    }
    println!("modo: {nome_modo}; alvo: {desc}; árvore: {} processo(s)", arvore.len());
    let r = gravar(pid, modo, Duration::from_secs(segundos))?;
    escrever_wav(&saida, &r.amostras)?;
    let quadros = r.amostras.len() / CANAIS as usize;
    let pico = r.amostras.iter().map(|v| (*v as i32).abs()).max().unwrap_or(0);
    let metricas = format!(
        "modo: {nome_modo}\nalvo: {desc}\narvore: {:?}\nformato: {TAXA} Hz, {CANAIS} canais, {BITS} bits PCM\n\
         duracao_pedida_s: {segundos}\nduracao_real_s: {:.3}\nquadros_gravados: {quadros}\naudio_gravado_s: {:.3}\n\
         pacotes: {}\npacotes_silenciosos: {}\npico: {pico}\n",
        arvore,
        r.duracao.as_secs_f64(),
        quadros as f64 / TAXA as f64,
        r.pacotes,
        r.silenciosos
    );
    fs::write(saida.with_extension("txt"), &metricas).map_err(|e| e.to_string())?;
    print!("{metricas}");
    println!("wav: {}", saida.display());
    Ok(())
}

// ---------- processos ----------

struct Proc {
    pid: u32,
    pai: u32,
    exe: String,
}

fn processos() -> Result<Vec<Proc>, String> {
    let mut v = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(|e| e.to_string())?;
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
            v.push(Proc { pid: e.th32ProcessID, pai: e.th32ParentProcessID, exe: String::from_utf16_lossy(&e.szExeFile[..n]) });
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    Ok(v)
}

/// Aceita um PID ou um nome de executável. Para nome, escolhe a raiz da
/// árvore: o processo com esse nome cujo pai não tem o mesmo nome. Assim
/// "Discord.exe" pega o processo principal, e os filhos entram pela árvore.
fn resolver_alvo(procs: &[Proc], alvo: &str) -> Result<(u32, String), String> {
    if let Ok(pid) = alvo.parse::<u32>() {
        let p = procs.iter().find(|p| p.pid == pid).ok_or(format!("PID {pid} não existe"))?;
        return Ok((pid, format!("{} (PID {pid})", p.exe)));
    }
    let mesmo = |p: &Proc| p.exe.eq_ignore_ascii_case(alvo) || p.exe.eq_ignore_ascii_case(&format!("{alvo}.exe"));
    let pids: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let raizes: Vec<&Proc> = procs
        .iter()
        .filter(|p| mesmo(p))
        .filter(|p| !pids.get(&p.pai).is_some_and(|pai| mesmo(pai) && pai.pid != p.pid))
        .collect();
    match raizes.as_slice() {
        [] => Err(format!("nenhum processo chamado {alvo}")),
        [p] => Ok((p.pid, format!("{} (PID {})", p.exe, p.pid))),
        varios => Err(format!(
            "{} árvores independentes de {alvo} (PIDs {:?}); use o PID",
            varios.len(),
            varios.iter().map(|p| p.pid).collect::<Vec<_>>()
        )),
    }
}

fn descendentes(procs: &[Proc], raiz: u32) -> Vec<u32> {
    let mut v = vec![raiz];
    let mut i = 0;
    while i < v.len() {
        let atual = v[i];
        for p in procs {
            if p.pai == atual && p.pid != atual && !v.contains(&p.pid) {
                v.push(p.pid);
            }
        }
        i += 1;
    }
    v
}

fn listar() -> Result<(), String> {
    let mut procs = processos()?;
    procs.sort_by(|a, b| a.exe.to_lowercase().cmp(&b.exe.to_lowercase()));
    for p in &procs {
        println!("{:>7}  pai {:>7}  {}", p.pid, p.pai, p.exe);
    }
    Ok(())
}

// ---------- captura ----------

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Aviso(HANDLE);

impl IActivateAudioInterfaceCompletionHandler_Impl for Aviso_Impl {
    fn ActivateCompleted(&self, _op: Ref<IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        unsafe { SetEvent(self.0) }
    }
}

struct Gravacao {
    amostras: Vec<i16>,
    duracao: Duration,
    pacotes: u64,
    silenciosos: u64,
}

fn ativar(pid: u32, modo: PROCESS_LOOPBACK_MODE) -> windows::core::Result<IAudioClient> {
    unsafe {
        let params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS { TargetProcessId: pid, ProcessLoopbackMode: modo },
            },
        };
        // ManuallyDrop: o windows-rs implementa Drop (PropVariantClear) para
        // PROPVARIANT, que tentaria liberar o blob apontando para a pilha.
        let pv = std::mem::ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: &params as *const _ as *mut u8,
                        },
                    },
                }),
            },
        });
        let evento = CreateEventW(None, false, false, None)?;
        let aviso: IActivateAudioInterfaceCompletionHandler = Aviso(evento).into();
        let op = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*pv), &aviso)?;
        let espera = WaitForSingleObject(evento, 5000);
        let _ = CloseHandle(evento);
        if espera != WAIT_OBJECT_0 {
            return Err(windows::core::Error::new(windows::core::HRESULT(0x8000_000Au32 as i32), "ativação demorou mais de 5 s"));
        }
        let mut hr = windows::core::HRESULT(0);
        let mut unk: Option<IUnknown> = None;
        op.GetActivateResult(&mut hr, &mut unk)?;
        hr.ok()?;
        unk.ok_or_else(|| windows::core::Error::new(windows::core::HRESULT(0x8000_4005u32 as i32), "sem interface"))?.cast()
    }
}

fn gravar(pid: u32, modo: PROCESS_LOOPBACK_MODE, duracao: Duration) -> Result<Gravacao, String> {
    let ctx = |etapa: &'static str| move |e: windows::core::Error| format!("{etapa}: {e} (0x{:08X})", e.code().0);
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().map_err(ctx("CoInitializeEx"))?;
        let cliente = ativar(pid, modo).map_err(ctx("ActivateAudioInterfaceAsync (process loopback)"))?;
        // Process loopback não tem GetMixFormat: o formato é escolhido aqui
        // e o Windows converte (AUTOCONVERTPCM).
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: CANAIS,
            nSamplesPerSec: TAXA,
            nAvgBytesPerSec: TAXA * CANAIS as u32 * BITS as u32 / 8,
            nBlockAlign: CANAIS * BITS / 8,
            wBitsPerSample: BITS,
            cbSize: 0,
        };
        cliente
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                200_000, // 20 ms
                0,
                &fmt,
                None,
            )
            .map_err(ctx("IAudioClient::Initialize"))?;
        let evento = CreateEventW(None, false, false, None).map_err(ctx("CreateEventW"))?;
        cliente.SetEventHandle(evento).map_err(ctx("SetEventHandle"))?;
        let cap: IAudioCaptureClient = cliente.GetService().map_err(ctx("GetService"))?;
        let mut g = Gravacao { amostras: Vec::with_capacity((TAXA as u64 * duracao.as_secs() * 2) as usize), duracao: Duration::ZERO, pacotes: 0, silenciosos: 0 };
        let inicio = Instant::now();
        cliente.Start().map_err(ctx("Start"))?;
        while inicio.elapsed() < duracao {
            WaitForSingleObject(evento, 100);
            loop {
                let n = cap.GetNextPacketSize().map_err(ctx("GetNextPacketSize"))?;
                if n == 0 {
                    break;
                }
                let mut dados = std::ptr::null_mut();
                let mut quadros = 0u32;
                let mut flags = 0u32;
                cap.GetBuffer(&mut dados, &mut quadros, &mut flags, None, None).map_err(ctx("GetBuffer"))?;
                let total = quadros as usize * CANAIS as usize;
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    g.silenciosos += 1;
                    g.amostras.extend(std::iter::repeat_n(0i16, total));
                } else {
                    g.amostras.extend_from_slice(std::slice::from_raw_parts(dados as *const i16, total));
                }
                g.pacotes += 1;
                cap.ReleaseBuffer(quadros).map_err(ctx("ReleaseBuffer"))?;
            }
        }
        let _ = cliente.Stop();
        g.duracao = inicio.elapsed();
        let _ = CloseHandle(evento);
        Ok(g)
    }
}

// ---------- WAV ----------

fn escrever_wav(caminho: &Path, amostras: &[i16]) -> Result<(), String> {
    let bytes = (amostras.len() * 2) as u32;
    let mut f = fs::File::create(caminho).map_err(|e| format!("{}: {e}", caminho.display()))?;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + bytes).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&CANAIS.to_le_bytes());
    h.extend_from_slice(&TAXA.to_le_bytes());
    h.extend_from_slice(&(TAXA * CANAIS as u32 * 2).to_le_bytes());
    h.extend_from_slice(&(CANAIS * 2).to_le_bytes());
    h.extend_from_slice(&BITS.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&bytes.to_le_bytes());
    f.write_all(&h).map_err(|e| e.to_string())?;
    let corpo: Vec<u8> = amostras.iter().flat_map(|v| v.to_le_bytes()).collect();
    f.write_all(&corpo).map_err(|e| e.to_string())
}

/// Lê só WAVs PCM 16 bits estéreo 48 kHz (o que esta prova grava).
fn analisar(caminho: &Path, freqs: &str) -> Result<(), String> {
    let b = fs::read(caminho).map_err(|e| format!("{}: {e}", caminho.display()))?;
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[36..40] != b"data" {
        return Err("WAV não reconhecido (esperado o formato gravado por esta prova)".into());
    }
    let taxa = u32::from_le_bytes(b[24..28].try_into().unwrap()) as f64;
    let canais = u16::from_le_bytes(b[22..24].try_into().unwrap()) as usize;
    // mistura os canais em mono, normalizado em -1..1
    let mono: Vec<f64> = b[44..]
        .chunks_exact(2 * canais)
        .map(|q| q.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]) as f64 / 32768.0).sum::<f64>() / canais as f64)
        .collect();
    let rms = (mono.iter().map(|v| v * v).sum::<f64>() / mono.len().max(1) as f64).sqrt();
    println!("arquivo: {}  duração: {:.2} s  RMS total: {:.5}", caminho.display(), mono.len() as f64 / taxa, rms);
    for f in freqs.split(',') {
        let hz: f64 = f.trim().parse().map_err(|_| format!("frequência inválida: {f}"))?;
        // Goertzel: amplitude do seno nessa frequência
        let w = 2.0 * std::f64::consts::PI * hz / taxa;
        let c = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0, 0.0);
        for x in &mono {
            let s = x + c * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        let pot = s1 * s1 + s2 * s2 - c * s1 * s2;
        let amp = 2.0 * pot.max(0.0).sqrt() / mono.len().max(1) as f64;
        println!("  {hz:>7.1} Hz  amplitude {amp:.5}");
    }
    Ok(())
}
