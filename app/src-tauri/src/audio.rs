//! F2.5: som do PC → LiveKit como `screen_share_audio`.
//! Captura por WASAPI process loopback (prova F1.2), PCM 48 kHz estéreo, Opus a 192 kbps.
//! Modos: "só o app escolhido" (árvore do processo), "tudo menos o Discord" e "sem som".
use std::{
    collections::{HashMap, BTreeSet, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use livekit::{
    options::{AudioEncoding, TrackPublishOptions},
    prelude::*,
    webrtc::{
        audio_frame::AudioFrame,
        audio_source::{native::NativeAudioSource, AudioSourceOptions, RtcAudioSource},
    },
};
use serde::{Deserialize, Serialize};
use windows::{
    core::{implement, Interface, IUnknown, Ref, HRESULT},
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Media::Audio::*,
        System::{
            Com::{
                StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0},
                CoInitializeEx, CoUninitialize, BLOB, COINIT_MULTITHREADED,
            },
            Diagnostics::ToolHelp::*,
            Threading::{CreateEventW, SetEvent, WaitForSingleObject},
            Variant::VT_BLOB,
        },
    },
};

pub const TAXA: u32 = 48_000;
pub const CANAIS: u32 = 2;
/// Igual ao `audioPreset` do site (`public/app.js`, `OPCOES_AUDIO`).
pub const BITRATE: u64 = 192_000;
/// 10 ms por quadro, como o SDK pede quando não há fila.
const AMOSTRAS_QUADRO: usize = (TAXA / 100 * CANAIS) as usize;
/// Executáveis do Discord (normal, PTB e Canary).
const DISCORD: [&str; 3] = ["Discord.exe", "DiscordPTB.exe", "DiscordCanary.exe"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "modo", rename_all = "snake_case")]
pub enum ModoAudio {
    /// Tudo o que toca no PC, menos o Discord (e o próprio app).
    #[default]
    TudoMenosDiscord,
    /// Só o processo escolhido (o PID vem de `listar_processos_audio`) e os filhos dele.
    App { pid: u32 },
    SemSom,
}

// ---------- processos ----------

#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: u32,
    pub pai: u32,
    pub exe: String,
}

pub fn processos() -> Result<Vec<Proc>, String> {
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

/// Sobe pelos pais enquanto o pai for o mesmo executável. A lista de áudio (F2.3) pode trazer
/// um processo filho (navegadores e o Discord tocam som num filho); a captura usa a árvore da raiz.
pub fn raiz_do_app(procs: &[Proc], pid: u32) -> Option<u32> {
    let por_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut atual = *por_pid.get(&pid)?;
    for _ in 0..64 {
        match por_pid.get(&atual.pai) {
            Some(pai) if pai.pid != atual.pid && pai.exe.eq_ignore_ascii_case(&atual.exe) => atual = pai,
            _ => break,
        }
    }
    Some(atual.pid)
}

/// Raiz da árvore do Discord, se ele estiver aberto. Só dá para excluir uma árvore por captura:
/// com mais de uma versão aberta (ex.: normal e PTB), fica a primeira da lista `DISCORD`.
pub fn raiz_discord(procs: &[Proc]) -> Option<u32> {
    let por_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    DISCORD.iter().find_map(|nome| {
        procs
            .iter()
            .filter(|p| p.exe.eq_ignore_ascii_case(nome))
            .find(|p| !por_pid.get(&p.pai).is_some_and(|pai| pai.pid != p.pid && pai.exe.eq_ignore_ascii_case(nome)))
            .map(|p| p.pid)
    })
}

/// Alvo do process loopback para o modo pedido: (pid, incluir?).
/// Sem Discord aberto, exclui o próprio app, o que na prática captura tudo.
fn alvo(modo: ModoAudio, procs: &[Proc]) -> Result<Option<(u32, bool)>, String> {
    match modo {
        ModoAudio::SemSom => Ok(None),
        ModoAudio::App { pid } => raiz_do_app(procs, pid)
            .map(|r| Some((r, true)))
            .ok_or_else(|| "Esse programa foi fechado. Atualize a lista e escolha de novo.".into()),
        ModoAudio::TudoMenosDiscord => Ok(Some((raiz_discord(procs).unwrap_or(std::process::id()), false))),
    }
}

// ---------- WASAPI ----------

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Aviso(HANDLE);

impl IActivateAudioInterfaceCompletionHandler_Impl for Aviso_Impl {
    fn ActivateCompleted(&self, _op: Ref<IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        unsafe { SetEvent(self.0) }
    }
}

struct Evento(HANDLE);
impl Drop for Evento {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn erro(etapa: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("{etapa}: {e} (0x{:08X})", e.code().0)
}

unsafe fn ativar(pid: u32, incluir: bool) -> windows::core::Result<IAudioClient> {
    let modo = if incluir {
        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
    } else {
        PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
    };
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS { TargetProcessId: pid, ProcessLoopbackMode: modo },
        },
    };
    // ManuallyDrop: o Drop do PROPVARIANT (PropVariantClear) tentaria liberar o blob da pilha (F1.2).
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
    let evento = Evento(CreateEventW(None, false, false, None)?);
    let aviso: IActivateAudioInterfaceCompletionHandler = Aviso(evento.0).into();
    let op = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*pv), &aviso)?;
    if WaitForSingleObject(evento.0, 5000) != WAIT_OBJECT_0 {
        return Err(windows::core::Error::new(HRESULT(0x8000_000Au32 as i32), "a ativação demorou mais de 5 s"));
    }
    let mut hr = HRESULT(0);
    let mut unk: Option<IUnknown> = None;
    op.GetActivateResult(&mut hr, &mut unk)?;
    hr.ok()?;
    unk.ok_or_else(|| windows::core::Error::new(HRESULT(0x8000_4005u32 as i32), "sem interface"))?.cast()
}

/// Cliente de captura já iniciado, com o evento que avisa quando há pacotes.
struct Cliente {
    cliente: IAudioClient,
    captura: IAudioCaptureClient,
    evento: Evento,
}

impl Cliente {
    unsafe fn abrir(pid: u32, incluir: bool) -> Result<Self, String> {
        let cliente = ativar(pid, incluir).map_err(erro("Ativar a captura de som"))?;
        // Process loopback não tem GetMixFormat: o formato é escolhido aqui e o Windows converte.
        let bits = 16u16;
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: CANAIS as u16,
            nSamplesPerSec: TAXA,
            nAvgBytesPerSec: TAXA * CANAIS * bits as u32 / 8,
            nBlockAlign: CANAIS as u16 * bits / 8,
            wBitsPerSample: bits,
            cbSize: 0,
        };
        let fluxo = AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM;
        cliente.Initialize(AUDCLNT_SHAREMODE_SHARED, fluxo, 200_000, 0, &fmt, None).map_err(erro("Preparar a captura de som"))?;
        let evento = Evento(CreateEventW(None, false, false, None).map_err(erro("CreateEventW"))?);
        cliente.SetEventHandle(evento.0).map_err(erro("SetEventHandle"))?;
        let captura: IAudioCaptureClient = cliente.GetService().map_err(erro("GetService"))?;
        cliente.Start().map_err(erro("Iniciar a captura de som"))?;
        Ok(Self { cliente, captura, evento })
    }

    /// Espera até 100 ms e junta os pacotes que chegaram em `saida`.
    unsafe fn ler(&self, saida: &mut Vec<i16>) -> Result<(), String> {
        WaitForSingleObject(self.evento.0, 100);
        self.ler_pronto(saida)
    }
    unsafe fn ler_pronto(&self, saida: &mut Vec<i16>) -> Result<(), String> {
        loop {
            let n = self.captura.GetNextPacketSize().map_err(erro("GetNextPacketSize"))?;
            if n == 0 {
                return Ok(());
            }
            let mut dados = std::ptr::null_mut();
            let mut quadros = 0u32;
            let mut flags = 0u32;
            self.captura.GetBuffer(&mut dados, &mut quadros, &mut flags, None, None).map_err(erro("GetBuffer"))?;
            let total = quadros as usize * CANAIS as usize;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                saida.extend(std::iter::repeat_n(0i16, total));
            } else {
                saida.extend_from_slice(std::slice::from_raw_parts(dados as *const i16, total));
            }
            self.captura.ReleaseBuffer(quadros).map_err(erro("ReleaseBuffer"))?;
        }
    }
}

impl Drop for Cliente {
    fn drop(&mut self) {
        unsafe {
            let _ = self.cliente.Stop();
        }
    }
}

// ---------- captura → LiveKit ----------

/// Números do som, lidos pelas estatísticas (F2.7).
#[derive(Default)]
pub struct ContadoresAudio {
    pub aviso: std::sync::Mutex<Option<String>>,
    /// Quadros de 10 ms entregues ao SDK.
    pub quadros: AtomicU64,
    /// Maior amostra (valor absoluto) do último segundo, de 0 a 32767.
    pub pico: AtomicU32,
    /// PID usado agora no process loopback (0 = nenhum).
    pub alvo: AtomicU32,
}

pub struct CapturaAudio {
    parar: Arc<AtomicBool>,
    fio: Option<JoinHandle<()>>,
    pub contadores: Arc<ContadoresAudio>,
}

impl CapturaAudio {
    /// Abre a captura numa thread própria. O modo geral mistura sessões permitidas e envia
    /// silêncio nas falhas de descoberta; não usa exclusão por PID com fallback global.
    pub fn iniciar(modo: ModoAudio, fonte: NativeAudioSource) -> Result<Self, String> {
        let parar = Arc::new(AtomicBool::new(false));
        let contadores = Arc::new(ContadoresAudio::default());
        let rt = tokio::runtime::Handle::current();
        let (pronto, resposta) = mpsc::sync_channel::<Result<(), String>>(1);
        let (p, c) = (parar.clone(), contadores.clone());
        let fio = std::thread::Builder::new()
            .name("captura-som".into())
            .spawn(move || unsafe {
                if let Err(e) = CoInitializeEx(None, COINIT_MULTITHREADED).ok() {
                    let _ = pronto.send(Err(format!("COM: {e}")));
                    return;
                }
                laco(modo, &fonte, &rt, &p, &c, Some(pronto));
                CoUninitialize();
            })
            .map_err(|e| e.to_string())?;
        let mut captura = Self { parar, fio: Some(fio), contadores };
        match resposta.recv() {
            Ok(Ok(())) => Ok(captura),
            Ok(Err(e)) => {
                captura.encerrar();
                Err(e)
            }
            Err(_) => Err("A captura de som terminou sem avisar.".into()),
        }
    }

    pub fn encerrar(&mut self) {
        self.parar.store(true, Ordering::Relaxed);
        if let Some(f) = self.fio.take() {
            let _ = f.join();
        }
    }
}

impl Drop for CapturaAudio {
    fn drop(&mut self) {
        self.encerrar();
    }
}

unsafe fn laco(
    modo: ModoAudio,
    fonte: &NativeAudioSource,
    rt: &tokio::runtime::Handle,
    parar: &AtomicBool,
    c: &ContadoresAudio,
    mut pronto: Option<mpsc::SyncSender<Result<(), String>>>,
) {
    if modo == ModoAudio::TudoMenosDiscord {
        laco_mistura(fonte, rt, parar, c, pronto);
        return;
    }
    let abrir = || -> Result<(Cliente, u32), String> {
        let (pid, incluir) = alvo(modo, &processos()?)?.ok_or("Modo sem som.")?;
        Ok((Cliente::abrir(pid, incluir)?, pid))
    };
    let (mut cliente, mut pid) = match abrir() {
        Ok(x) => x,
        Err(e) => {
            if let Some(p) = pronto.take() {
                let _ = p.send(Err(e));
            }
            return;
        }
    };
    if let Some(p) = pronto.take() {
        let _ = p.send(Ok(()));
    }
    c.alvo.store(pid, Ordering::Relaxed);
    let mut buffer: Vec<i16> = Vec::with_capacity(AMOSTRAS_QUADRO * 4);
    let mut pico_desde = Instant::now();
    let mut pico = 0u32;
    while !parar.load(Ordering::Relaxed) {
        if let Err(e) = cliente.ler(&mut buffer) {
            // Ex.: a saída de som do Windows mudou. Tenta abrir de novo em seguida.
            eprintln!("captura de som: {e}");
            std::thread::sleep(Duration::from_millis(200));
            if let Ok((novo, p)) = abrir() {
                (cliente, pid) = (novo, p);
                c.alvo.store(pid, Ordering::Relaxed);
            }
            buffer.clear();
            continue;
        }
        let mut usados = 0;
        while buffer.len() - usados >= AMOSTRAS_QUADRO {
            let pedaco = &buffer[usados..usados + AMOSTRAS_QUADRO];
            pico = pico.max(pedaco.iter().map(|v| v.unsigned_abs() as u32).max().unwrap_or(0));
            let quadro = AudioFrame {
                data: pedaco.into(),
                sample_rate: TAXA,
                num_channels: CANAIS,
                samples_per_channel: TAXA / 100,
            };
            if rt.block_on(fonte.capture_frame(&quadro)).is_ok() {
                c.quadros.fetch_add(1, Ordering::Relaxed);
            }
            usados += AMOSTRAS_QUADRO;
        }
        buffer.drain(..usados);
        if pico_desde.elapsed() >= Duration::from_secs(1) {
            c.pico.store(pico, Ordering::Relaxed);
            (pico, pico_desde) = (0, Instant::now());
        }
    }
}

fn descendente(procs: &[Proc], mut pid: u32, raiz: u32) -> bool {
    for _ in 0..128 {
        if pid == raiz { return true; }
        let Some(p) = procs.iter().find(|p| p.pid == pid) else { return false };
        if p.pai == pid { return false; }
        pid = p.pai;
    }
    // Uma árvore cíclica/incompleta não é autorização para incluir som.
    true
}

fn permitido_na_mistura(procs: &[Proc], pid: u32, meu_pid: u32) -> bool {
    if !procs.iter().any(|p| p.pid == meu_pid) { return false; }
    let Some(p) = procs.iter().find(|p| p.pid == pid) else { return false };
    // Não capturar árvores de lançadores: elas podem iniciar apps excluídos depois.
    let exe = p.exe.to_ascii_lowercase();
    if ["explorer.exe", "steam.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "update.exe", "svchost.exe", "services.exe"].contains(&exe.as_str()) { return false; }
    !procs.iter().filter(|p| p.pid == meu_pid || p.exe.to_ascii_lowercase().starts_with("discord"))
        .any(|p| descendente(procs, pid, p.pid) || descendente(procs, p.pid, pid))
}

fn alvos_mistura(procs: &[Proc], sessoes: &[u32], meu_pid: u32) -> BTreeSet<u32> {
    let permitidos: BTreeSet<_> = sessoes.iter().copied().filter(|&pid| permitido_na_mistura(procs, pid, meu_pid)).collect();
    // Uma captura da árvore já contém suas sessões filhas: não somar duas vezes.
    permitidos.iter().copied().filter(|&pid| !permitidos.iter().any(|&outro| outro != pid && descendente(procs, pid, outro))).collect()
}

fn misturar(filas: &mut [VecDeque<i16>], atraso: usize) -> Vec<i16> {
    let mut soma = vec![0i64; AMOSTRAS_QUADRO];
    for fila in filas {
        // Descarta atraso excessivo em vez de acumular segundos de som.
        while fila.len() > atraso + AMOSTRAS_QUADRO * 8 { fila.pop_front(); }
        if fila.len() >= atraso + AMOSTRAS_QUADRO {
            for amostra in &mut soma { *amostra += fila.pop_front().unwrap() as i64; }
        }
    }
    soma.into_iter().map(|x| x.clamp(i16::MIN as i64, i16::MAX as i64) as i16).collect()
}

/// Modo geral usa somente capturas de inclusão de sessões permitidas. O Discord e o WebView
/// nunca são o fallback de uma captura do sistema inteiro. Falha de descoberta envia silêncio.
unsafe fn laco_mistura(
    fonte: &NativeAudioSource, rt: &tokio::runtime::Handle, parar: &AtomicBool,
    c: &ContadoresAudio, mut pronto: Option<mpsc::SyncSender<Result<(), String>>>,
) {
    let mut fontes: Vec<(u32, Cliente)> = Vec::new();
    let mut filas: Vec<VecDeque<i16>> = Vec::new();
    let mut descobrir_em = Instant::now();
    let mut revisar_em = Instant::now();
    let mut procs = Vec::new();
    let mut proximo = Instant::now();
    let mut pico = 0;
    let mut pico_em = Instant::now();
    // Retém 100 ms para descartar buffers quando a árvore muda na revisão de 50 ms.
    let atraso = AMOSTRAS_QUADRO * 10;
    while !parar.load(Ordering::Relaxed) {
        let agora = Instant::now();
        if agora >= revisar_em {
            revisar_em = agora + Duration::from_millis(50);
            match processos() {
                Ok(p) => procs = p,
                Err(e) => {
                    fontes.clear(); filas.clear();
                    *c.aviso.lock().unwrap() = Some(format!("Som pausado: não foi possível conferir os aplicativos. {e}"));
                    descobrir_em = agora + Duration::from_millis(200);
                }
            }
            for i in (0..fontes.len()).rev() {
                if !permitido_na_mistura(&procs, fontes[i].0, std::process::id()) { fontes.remove(i); filas.remove(i); }
            }
        }
        if agora >= descobrir_em {
            descobrir_em = agora + Duration::from_secs(1);
            match processos().and_then(|p| crate::fontes::processos_audio().map(|s| (p, s))) {
                Ok((p, sessoes)) => {
                    procs = p;
                    let ids: Vec<_> = sessoes.iter().map(|s| s.pid).collect();
                    let alvos = alvos_mistura(&procs, &ids, std::process::id());
                    for i in (0..fontes.len()).rev() {
                        if !alvos.contains(&fontes[i].0) { fontes.remove(i); filas.remove(i); }
                    }
                    let mut falhas = Vec::new();
                    for pid in alvos {
                        if fontes.iter().any(|f| f.0 == pid) { continue; }
                        match Cliente::abrir(pid, true) {
                            Ok(cliente) => { fontes.push((pid, cliente)); filas.push(VecDeque::new()); }
                            Err(_) => falhas.push(pid),
                        }
                    }
                    *c.aviso.lock().unwrap() = if falhas.is_empty() { None } else { Some("Alguns aplicativos ficaram sem som. Escolha ‘Só o app escolhido’ para conferir.".into()) };
                    if let Some(p) = pronto.take() { let _ = p.send(Ok(())); }
                }
                Err(e) => {
                    fontes.clear(); filas.clear();
                    *c.aviso.lock().unwrap() = Some(format!("Som pausado: não foi possível conferir os aplicativos. {e}"));
                    if let Some(p) = pronto.take() { let _ = p.send(Err(e)); return; }
                }
            }
        }
        for i in (0..fontes.len()).rev() {
            let mut buffer = Vec::new();
            if fontes[i].1.ler_pronto(&mut buffer).is_err() {
                fontes.remove(i); filas.remove(i);
                *c.aviso.lock().unwrap() = Some("Um aplicativo ficou sem som. Tentando capturar novamente…".into());
                descobrir_em = Instant::now() + Duration::from_millis(200);
            } else { filas[i].extend(buffer); }
        }
        // A ativação de um novo cliente pode demorar; confira a árvore novamente antes de usar buffers.
        if Instant::now() >= revisar_em { continue; }
        let mistura = misturar(&mut filas, atraso);
        pico = pico.max(mistura.iter().map(|v| v.unsigned_abs() as u32).max().unwrap_or(0));
        let quadro = AudioFrame { data: mistura.into(), sample_rate: TAXA, num_channels: CANAIS, samples_per_channel: TAXA / 100 };
        if rt.block_on(fonte.capture_frame(&quadro)).is_ok() { c.quadros.fetch_add(1, Ordering::Relaxed); }
        c.alvo.store(0, Ordering::Relaxed);
        if pico_em.elapsed() >= Duration::from_secs(1) { c.pico.store(pico, Ordering::Relaxed); pico = 0; pico_em = Instant::now(); }
        proximo += Duration::from_millis(10);
        // Recupera atrasos pequenos sem perder quadros a cada arredondamento do sleep do Windows.
        if proximo + Duration::from_millis(500) < Instant::now() { proximo = Instant::now(); }
        std::thread::sleep(proximo.saturating_duration_since(Instant::now()));
    }
}

/// Faixa de som publicada: a fonte do SDK e a captura que a alimenta.
pub struct Som {
    pub faixa: LocalAudioTrack,
    pub captura: CapturaAudio,
    pub(crate) fonte: NativeAudioSource,
    pub modo: ModoAudio,
}

pub fn opcoes_publicacao() -> TrackPublishOptions {
    TrackPublishOptions {
        source: TrackSource::ScreenshareAudio,
        audio_encoding: Some(AudioEncoding { max_bitrate: BITRATE }),
        // como o site: sem DTX, para música e jogo não cortarem nos trechos baixos
        dtx: false,
        ..Default::default()
    }
}

impl Som {
    /// Publica a faixa `som` na sala e começa a capturar. `SemSom` não publica nada.
    pub async fn iniciar(sala: &Room, modo: ModoAudio) -> Result<Option<Self>, String> {
        if modo == ModoAudio::SemSom {
            return Ok(None);
        }
        // Som do PC: nada de cancelar eco, reduzir ruído ou ajustar ganho.
        let opcoes = AudioSourceOptions { echo_cancellation: false, noise_suppression: false, auto_gain_control: false };
        let fonte = NativeAudioSource::new(opcoes, TAXA, CANAIS, 0);
        let faixa = LocalAudioTrack::create_audio_track("som", RtcAudioSource::Native(fonte.clone()));
        // Abre a captura antes de publicar: se o programa sumiu, o erro aparece sem mexer na sala.
        let captura = CapturaAudio::iniciar(modo, fonte.clone())?;
        sala.local_participant()
            .publish_track(LocalTrack::Audio(faixa.clone()), opcoes_publicacao())
            .await
            .map_err(|e| format!("O servidor recusou o som: {e}"))?;
        Ok(Some(Self { faixa, captura, fonte, modo }))
    }

    /// Troca o modo sem despublicar a faixa (F2.6). `SemSom` é tratado por quem chama.
    /// Se o modo novo não abrir, volta ao anterior e devolve o erro.
    pub fn trocar(&mut self, modo: ModoAudio) -> Result<(), String> {
        if self.modo == modo && self.captura.fio.as_ref().is_some_and(|f| !f.is_finished()) {
            return Ok(());
        }
        // Para a antiga antes: as duas juntas mandariam som em dobro por um instante.
        self.captura.encerrar();
        match CapturaAudio::iniciar(modo, self.fonte.clone()) {
            Ok(c) => {
                self.captura = c;
                self.modo = modo;
                Ok(())
            }
            Err(e) => {
                if let Ok(c) = CapturaAudio::iniciar(self.modo, self.fonte.clone()) {
                    self.captura = c;
                }
                Err(e)
            }
        }
    }


    /// Bytes de som já enviados (para a taxa da F2.7).
    pub async fn bytes_enviados(&self) -> u64 {
        self.faixa
            .get_stats()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| match s {
                livekit::webrtc::stats::RtcStats::OutboundRtp(o) => Some(o.sent.bytes_sent),
                _ => None,
            })
            .sum()
    }

    pub async fn parar(mut self, sala: &Room) {
        self.captura.encerrar();
        let _ = tokio::time::timeout(Duration::from_secs(2), sala.local_participant().unpublish_track(&self.faixa.sid())).await;
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn p(pid: u32, pai: u32, exe: &str) -> Proc {
        Proc { pid, pai, exe: exe.into() }
    }

    #[test]
    fn mistura_exclui_discord_webview_e_arvores_sobrepostas() {
        let procs = [p(1,0,"explorer.exe"), p(10,1,"jogo.exe"), p(11,10,"jogo.exe"),
            p(20,1,"Discord.exe"), p(21,20,"audio.exe"), p(30,1,"DiscordPTB.exe"),
            p(40,1,"streamstudio.exe"), p(41,40,"msedgewebview2.exe")];
        assert_eq!(alvos_mistura(&procs, &[1,10,11,20,21,30,40,41,999],40), BTreeSet::from([10]));
        let mut mudou = procs.to_vec(); mudou.push(p(50,10,"DiscordCanary.exe"));
        assert_eq!(alvos_mistura(&mudou,&[10,11,50],40), BTreeSet::from([11]));
    }

    #[test]
    fn mistura_soma_canais_limita_pico_e_retem_buffer_de_seguranca() {
        let mut filas = [VecDeque::from(vec![20000; AMOSTRAS_QUADRO*2]), VecDeque::from(vec![20000; AMOSTRAS_QUADRO*2])];
        let quadro = misturar(&mut filas, AMOSTRAS_QUADRO);
        assert!(quadro.iter().all(|&x| x==32767));
        assert!(misturar(&mut filas,AMOSTRAS_QUADRO).iter().all(|&x| x==0));
    }

    #[tokio::test(flavor="multi_thread")]
    #[ignore="Ensaio de mistura WASAPI com tons e mudanças de processo controladas"]
    async fn ensaio_mistura_segura() {
        use futures_util::StreamExt;
        use livekit::webrtc::audio_stream::native::NativeAudioStream;
        let raiz = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let pasta = raiz.join(std::env::var("ENSAIO_MISTURA_PASTA").unwrap_or_else(|_| "dev/estado/bugs-audio".into()));
        struct Filho(std::process::Child);
        impl Drop for Filho { fn drop(&mut self) { let _=self.0.kill(); let _=self.0.wait(); } }
        let _proprio = Filho(std::process::Command::new(raiz.join("app/spikes/audio-processo/target/debug/tom.exe"))
            .args(["3400","40","0.02"]).spawn().unwrap());
        let fonte = NativeAudioSource::new(AudioSourceOptions::default(),TAXA,CANAIS,0);
        let faixa = LocalAudioTrack::create_audio_track("ensaio",RtcAudioSource::Native(fonte.clone()));
        let mut fluxo = NativeAudioStream::new(faixa.rtc_track(),TAXA as i32,CANAIS as i32);
        let captura = CapturaAudio::iniciar(ModoAudio::TudoMenosDiscord,fonte.clone()).unwrap();
        let mut resultados=Vec::new();
        for fase in 0..3 {
            if fase>0 {
                std::fs::write(pasta.join(format!("pedido-{fase}")),b"pronto").unwrap();
                for _ in 0..200 {
                    if pasta.join(format!("feito-{fase}")).exists() { break; }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                assert!(pasta.join(format!("feito-{fase}")).exists(),"O executor deve abrir/reiniciar o Discord simulado");
                let pid:u32=std::fs::read_to_string(pasta.join(format!("feito-{fase}"))).unwrap().trim().parse().unwrap();
                let procs=processos().unwrap();
                assert!(!descendente(&procs,pid,std::process::id()),"O Discord simulado deve ser externo à árvore do teste");
                assert!(!permitido_na_mistura(&procs,pid,std::process::id()));
            }
            let inicio=Instant::now(); let mut dados=Vec::new();
            while inicio.elapsed()<Duration::from_secs(3) {
                let quadro=tokio::time::timeout(Duration::from_secs(2),fluxo.next()).await.unwrap().unwrap();
                if inicio.elapsed()>Duration::from_secs(1) { dados.extend(quadro.data.iter().step_by(2).map(|&x|x as f64)); }
            }
            let amplitude=|hz:f64| {
                // Blocos curtos evitam cancelar fases entre trechos separados por silêncio.
                let mut total=0.0; let mut blocos=0;
                for bloco in dados.chunks_exact(4800) {
                    let (mut re,mut im)=(0.0,0.0);
                    for (i,x) in bloco.iter().enumerate() { let a=2.0*std::f64::consts::PI*hz*i as f64/TAXA as f64;re+=x*a.cos();im+=x*a.sin(); }
                    total+=(re*re+im*im).sqrt()*2.0/bloco.len() as f64;blocos+=1;
                }
                total/(blocos as f64).max(1.0)
            };
            let (jogo,musica,discord,proprio)=(amplitude(1200.0),amplitude(1600.0),amplitude(2300.0),amplitude(3400.0));
            println!("fase {fase}: jogo={jogo:.3}, música={musica:.3}, Discord={discord:.3}, próprio app={proprio:.3}");
            assert!(jogo>50.0 && musica>50.0,"Os dois sons permitidos precisam chegar");
            assert!(discord<jogo*0.02 && proprio<jogo*0.02,"Som excluído não pode aparecer na mistura");
            resultados.push(serde_json::json!({"fase":fase,"jogo":jogo,"musica":musica,"discord":discord,"proprio":proprio}));
        }
        let contadores=captura.contadores.clone();
        let mut som=Som{faixa,captura,fonte,modo:ModoAudio::TudoMenosDiscord};
        som.trocar(ModoAudio::TudoMenosDiscord).unwrap();
        assert!(Arc::ptr_eq(&contadores,&som.captura.contadores),"B13: o mesmo modo deve conservar a captura");
        som.captura.encerrar(); fluxo.close();
        std::fs::write(pasta.join("resultado.json"),serde_json::to_vec_pretty(&resultados).unwrap()).unwrap();
    }

    #[test]
    fn modos_em_json() {
        let m: ModoAudio = serde_json::from_str(r#"{"modo":"app","pid":42}"#).unwrap();
        assert_eq!(m, ModoAudio::App { pid: 42 });
        assert_eq!(serde_json::from_str::<ModoAudio>(r#"{"modo":"sem_som"}"#).unwrap(), ModoAudio::SemSom);
        assert_eq!(serde_json::to_string(&ModoAudio::default()).unwrap(), r#"{"modo":"tudo_menos_discord"}"#);
    }

    #[test]
    fn raiz_sobe_pelo_mesmo_executavel() {
        let procs = [
            p(1, 0, "explorer.exe"),
            p(10, 1, "chrome.exe"),
            p(11, 10, "chrome.exe"),
            p(12, 11, "chrome.exe"),
            p(20, 1, "jogo.exe"),
        ];
        assert_eq!(raiz_do_app(&procs, 12), Some(10));
        assert_eq!(raiz_do_app(&procs, 20), Some(20));
        assert_eq!(raiz_do_app(&procs, 99), None);
        // pai com PID reaproveitado apontando para si mesmo não trava
        assert_eq!(raiz_do_app(&[p(5, 5, "a.exe")], 5), Some(5));
    }

    #[test]
    fn acha_a_raiz_do_discord() {
        let procs = [
            p(1, 0, "explorer.exe"),
            p(30, 1, "Discord.exe"),
            p(31, 30, "Discord.exe"),
            p(32, 30, "discord.exe"),
            p(40, 1, "DiscordSystemHelper.exe"),
        ];
        assert_eq!(raiz_discord(&procs), Some(30));
        assert_eq!(raiz_discord(&procs[..1]), None);
        assert_eq!(raiz_discord(&[p(50, 1, "DiscordPTB.exe"), p(51, 50, "DiscordPTB.exe")]), Some(50));
        assert_eq!(alvo(ModoAudio::TudoMenosDiscord, &procs).unwrap(), Some((30, false)));
        assert_eq!(alvo(ModoAudio::TudoMenosDiscord, &procs[..1]).unwrap(), Some((std::process::id(), false)));
        assert_eq!(alvo(ModoAudio::App { pid: 31 }, &procs).unwrap(), Some((30, true)));
        assert!(alvo(ModoAudio::App { pid: 777 }, &procs).is_err());
        assert_eq!(alvo(ModoAudio::SemSom, &procs).unwrap(), None);
    }

    #[test]
    fn opcoes_iguais_ao_site() {
        let o = opcoes_publicacao();
        assert_eq!(o.source, TrackSource::ScreenshareAudio);
        assert_eq!(o.audio_encoding.unwrap().max_bitrate, 192_000);
        assert!(!o.dtx);
    }
}
