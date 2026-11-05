//! F2.4: fonte escolhida (WGC) → LiveKit como `screen_share`.
//! Padrões iguais aos do site (`public/app.js`, `TX_PADRAO` e `opcoesVideo`):
//! 1080p, 60 fps, 12 Mbps, sem simulcast, preferência por manter o fps.
//! H.264 só é o padrão se houver codificador por hardware; senão VP9 (decisão da F1.5).
//! Diferença do site: SVC desligado por padrão (no SDK Rust ele mandava ~1 Mbps e caía de
//! resolução; ver `ia/resultados/F2.4.md`). Fica como opção (`svc`).
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use livekit::{
    options::{TrackPublishOptions, VideoCodec, VideoEncoding},
    prelude::*,
    webrtc::{
        native::yuv_helper,
        rtp_parameters::DegradationPreference,
        rtp_sender::VideoEncoderBackend,
        stats::RtcStats,
        video_frame::{I420Buffer, VideoFrame, VideoRotation},
        video_source::{native::NativeVideoSource, RtcVideoSource, VideoResolution},
    },
};
use serde::{Deserialize, Serialize};
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::*,
    window::Window,
};

type Erro = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    /// H.264 se houver codificador por hardware, senão VP9.
    #[default]
    Auto,
    H264,
    Vp8,
    Vp9,
    Av1,
}

/// Ajustes de vídeo, com os mesmos valores aceitos pelo site.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigVideo {
    pub codec: Codec,
    /// Altura máxima: 720, 1080, 1440 ou 2160. Fonte maior é reduzida; menor fica como está.
    pub res: u32,
    /// 30 ou 60.
    pub fps: u32,
    /// Bitrate máximo em Mbps, de 1 a 50.
    pub mbps: u32,
    /// Camadas SVC (L3T3_KEY) no VP9/AV1, como o site. Ignorado em H.264/VP8.
    /// Desligado por padrão: ver o comentário do topo.
    pub svc: bool,
}

impl Default for ConfigVideo {
    fn default() -> Self {
        Self { codec: Codec::Auto, res: 1080, fps: 60, mbps: 12, svc: false }
    }
}

impl ConfigVideo {
    pub fn validar(&self) -> Result<(), String> {
        if ![720, 1080, 1440, 2160].contains(&self.res) {
            return Err(format!("Resolução inválida: {}p.", self.res));
        }
        if ![30, 60].contains(&self.fps) {
            return Err(format!("Quadros por segundo inválidos: {}.", self.fps));
        }
        if !(1..=50).contains(&self.mbps) {
            return Err(format!("Bitrate inválido: {} Mbps.", self.mbps));
        }
        Ok(())
    }
}

/// Há codificador de vídeo por hardware disponível para o SDK neste PC?
pub fn tem_encoder_hardware() -> bool {
    VideoEncoderBackend::list_available().into_iter().any(|b| {
        matches!(b, VideoEncoderBackend::Hardware | VideoEncoderBackend::Nvenc | VideoEncoderBackend::Vaapi)
    })
}

/// Codec que será usado de fato, e se deve pedir o codificador por hardware.
fn resolver_codec(codec: Codec, hardware: bool) -> (VideoCodec, VideoEncoderBackend) {
    let backend = if hardware { VideoEncoderBackend::Hardware } else { VideoEncoderBackend::Auto };
    match codec {
        Codec::Auto if hardware => (VideoCodec::H264, backend),
        Codec::Auto | Codec::Vp9 => (VideoCodec::VP9, VideoEncoderBackend::Auto),
        Codec::H264 => (VideoCodec::H264, backend),
        Codec::Vp8 => (VideoCodec::VP8, VideoEncoderBackend::Auto),
        Codec::Av1 => (VideoCodec::AV1, VideoEncoderBackend::Auto),
    }
}

fn opcoes_publicacao(codec: VideoCodec, backend: VideoEncoderBackend, cfg: &ConfigVideo) -> TrackPublishOptions {
    let svc = cfg.svc && matches!(codec, VideoCodec::VP9 | VideoCodec::AV1);
    TrackPublishOptions {
        source: TrackSource::Screenshare,
        video_codec: codec,
        video_encoder: backend,
        video_encoding: Some(VideoEncoding { max_bitrate: cfg.mbps as u64 * 1_000_000, max_framerate: cfg.fps as f64 }),
        simulcast: false,
        scalability_mode: svc.then(|| "L3T3_KEY".to_string()),
        degradation_preference: Some(DegradationPreference::MaintainFramerate),
        ..Default::default()
    }
}

/// Tamanho de saída: reduz mantendo a proporção até caber na altura máxima; sempre par (I420).
pub fn tamanho_saida(largura: u32, altura: u32, altura_max: u32) -> (u32, u32) {
    let (w, h) = if altura > altura_max {
        ((largura as u64 * altura_max as u64 / altura as u64) as u32, altura_max)
    } else {
        (largura, altura)
    };
    ((w & !1).max(2), (h & !1).max(2))
}

/// "monitor:<hmonitor>" ou "janela:<hwnd>", como vem de `listar_fontes` (F2.3).
/// Os handles são transitórios: confere se a fonte ainda existe.
#[derive(Debug, Clone, Copy)]
enum Alvo {
    Monitor(Monitor),
    Janela(Window),
}

fn achar_fonte(id: &str) -> Result<Alvo, String> {
    let (tipo, numero) = id.split_once(':').ok_or("Fonte inválida.")?;
    let numero: usize = numero.parse().map_err(|_| "Fonte inválida.")?;
    let sumiu = || "Essa fonte não está mais disponível. Atualize a lista.".to_string();
    match tipo {
        "monitor" => Monitor::enumerate()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|m| m.as_raw_hmonitor() as usize == numero)
            .map(Alvo::Monitor)
            .ok_or_else(sumiu),
        "janela" => {
            let j = Window::from_raw_hwnd(numero as *mut std::ffi::c_void);
            if j.is_valid() { Ok(Alvo::Janela(j)) } else { Err(sumiu()) }
        }
        _ => Err("Fonte inválida.".into()),
    }
}

fn hwnd_de(alvo: Alvo) -> usize {
    match alvo {
        Alvo::Janela(j) => j.as_raw_hwnd() as usize,
        Alvo::Monitor(_) => 0,
    }
}

fn tamanho_fonte(alvo: Alvo) -> (u32, u32) {
    match alvo {
        Alvo::Monitor(m) => (m.width().unwrap_or(1920), m.height().unwrap_or(1080)),
        Alvo::Janela(j) => (j.width().unwrap_or(1920).max(2) as u32, j.height().unwrap_or(1080).max(2) as u32),
    }
}

/// Números da captura, lidos pela interface e pelas estatísticas (F2.7).
#[derive(Default)]
pub struct Contadores {
    pub capturados: AtomicU64,
    pub enviados: AtomicU64,
    pub largura: AtomicU64,
    pub altura: AtomicU64,
    /// A janela capturada foi fechada (ou o monitor sumiu).
    pub fechada: AtomicBool,
    /// Recorte de janela congelado porque outra janela está por cima e o WGC não abriu (P3/P8).
    pub coberta: AtomicBool,
    /// Janela coberta transmitida pelo WGC enquanto isso (P8).
    pub reserva_wgc: AtomicBool,
}

struct Compartilhado {
    fonte: NativeVideoSource,
    /// Relógio dos quadros: continua o mesmo quando a fonte é trocada (F2.6).
    inicio: Instant,
    /// Captura que vale agora. Na troca, quadros da captura antiga são ignorados.
    geracao: AtomicU64,
    /// Entre quadros, em nanossegundos; muda ao vivo (P4).
    intervalo_ns: AtomicU64,
    /// Altura máxima enviada; muda ao vivo (P4).
    altura_max: std::sync::atomic::AtomicU32,
    contadores: Arc<Contadores>,
    /// Último quadro da captura para a prévia do app; preparado fora da thread de captura.
    previa: std::sync::Mutex<Option<Arc<QuadroPrevia>>>,
    previa_pedido: AtomicU64,
    previa_memoria: std::sync::Mutex<Option<Arc<crate::previa_memoria::Memoria>>>,
}

/// Retém o quadro já entregue ao SDK. Redução e cópia acontecem fora da captura.
struct QuadroPrevia {
    buffer: std::sync::Mutex<I420Buffer>,
    largura: u32,
    altura: u32,
    instante: u64,
}

impl QuadroPrevia {
    fn gerar(&self, largura: u32, altura: u32) -> Previa {
        Previa::no_tamanho(&mut self.buffer.lock().unwrap(), self.largura, self.altura,
            largura, altura, self.instante)
    }
}

/// Quadro I420 pequeno para a prévia ao vivo do app.
#[derive(Clone)]
pub struct Previa { pub largura: u32, pub altura: u32, instante: u64, y: Vec<u8>, u: Vec<u8>, v: Vec<u8> }

/// Tamanho usado pelos ensaios históricos que gravam JPEG.
#[cfg(test)]
const PREVIA_MAX: (u32, u32) = (960, 540);

impl Previa {
    #[cfg(test)]
    fn de(i420: &mut I420Buffer, w: u32, h: u32) -> Self {
        Self::no_tamanho(i420, w, h, PREVIA_MAX.0, PREVIA_MAX.1, 0)
    }

    fn no_tamanho(i420: &mut I420Buffer, w: u32, h: u32, max_w: u32, max_h: u32, instante: u64) -> Self {
        let escala = (max_w as f64 / w as f64).min(max_h as f64 / h as f64).min(1.0);
        let (pw, ph) = (((w as f64 * escala) as u32 & !1).max(2), ((h as f64 * escala) as u32 & !1).max(2));
        let copiar = |b: &I420Buffer| {
            let (sy, su, sv) = b.strides();
            let (y, u, v) = b.data();
            let plano = |d: &[u8], passo: u32, largura: u32, linhas: u32| -> Vec<u8> {
                let mut bytes = Vec::with_capacity((largura * linhas) as usize);
                for l in 0..linhas {
                    bytes.extend_from_slice(&d[(l * passo) as usize..(l * passo + largura) as usize]);
                }
                bytes
            };
            Previa { largura: pw, altura: ph, instante, y: plano(y, sy, pw, ph), u: plano(u, su, pw / 2, ph / 2), v: plano(v, sv, pw / 2, ph / 2) }
        };
        if (pw, ph) == (w, h) { copiar(i420) } else { copiar(&i420.scale(pw as i32, ph as i32)) }
    }

    /// Cabeçalho LE: largura, altura, instante e modo (0=cru, 1=zlib). Planos Y, U e V.
    fn binario(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(20 + self.y.len() + self.u.len() + self.v.len());
        bytes.extend_from_slice(&self.largura.to_le_bytes());
        bytes.extend_from_slice(&self.altura.to_le_bytes());
        bytes.extend_from_slice(&self.instante.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&self.y);
        bytes.extend_from_slice(&self.u);
        bytes.extend_from_slice(&self.v);
        // O navegador descomprime DEFLATE nativamente. Não quantiza os planos.
        // Conteúdo de alta entropia não compensa: evita o compressor nessa situação.
        let mut vistos = [false; 256];
        for &v in bytes[20..].iter().step_by(53).take(2048) { vistos[v as usize] = true; }
        if vistos.iter().filter(|&&v| v).count() > 160 { return bytes; }
        use std::io::Write;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        if encoder.write_all(&bytes[20..]).is_err() { return bytes; }
        let Ok(comprimido) = encoder.finish() else { return bytes; };
        if comprimido.len() > (bytes.len() - 20) * 3 / 4 { return bytes; }
        let mut pacote = Vec::with_capacity(20 + comprimido.len());
        pacote.extend_from_slice(&bytes[..20]);
        pacote[16..20].copy_from_slice(&1u32.to_le_bytes());
        pacote.extend_from_slice(&comprimido);
        pacote
    }

    #[cfg(test)]
    /// Converte para RGB (BT.601, faixa limitada, como o libyuv gerou) e comprime em JPEG.
    pub fn jpeg(&self) -> Result<Vec<u8>, String> {
        let (w, h) = (self.largura as usize, self.altura as usize);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for l in 0..h {
            for c in 0..w {
                let y = 1.164 * (self.y[l * w + c] as f32 - 16.0);
                let i = (l / 2) * (w / 2) + c / 2;
                let (u, v) = (self.u[i] as f32 - 128.0, self.v[i] as f32 - 128.0);
                rgb.push((y + 1.596 * v).clamp(0.0, 255.0) as u8);
                rgb.push((y - 0.392 * u - 0.813 * v).clamp(0.0, 255.0) as u8);
                rgb.push((y + 2.017 * u).clamp(0.0, 255.0) as u8);
            }
        }
        let mut saida = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut saida, 80)
            .encode(&rgb, self.largura, self.altura, image::ExtendedColorType::Rgb8)
            .map_err(|e| e.to_string())?;
        Ok(saida)
    }
}

impl Compartilhado {
    fn intervalo(&self) -> Duration { Duration::from_nanos(self.intervalo_ns.load(Ordering::Relaxed)) }
    fn altura_max(&self) -> u32 { self.altura_max.load(Ordering::Relaxed) }
    fn definir(&self, fps: u32, altura: u32) {
        self.intervalo_ns.store((1e9 / fps as f64) as u64, Ordering::Relaxed);
        self.altura_max.store(altura, Ordering::Relaxed);
    }
}

struct Captura {
    c: Arc<Compartilhado>,
    geracao: u64,
    entrega: Entrega,
}

/// Ritmo e conversão comuns a todos os métodos de captura (WGC, DXGI, GDI).
struct Entrega {
    proximo: Instant,
}

impl Entrega {
    fn nova() -> Self { Self { proximo: Instant::now() } }

    /// Limita o fps: o WGC entrega na taxa do monitor (60, 144...). Tolera 1/4 de quadro de atraso.
    fn na_hora(&mut self, c: &Compartilhado) -> bool {
        let agora = Instant::now();
        let intervalo = c.intervalo();
        if agora + intervalo / 4 < self.proximo {
            return false;
        }
        self.proximo = if agora > self.proximo + intervalo { agora + intervalo } else { self.proximo + intervalo };
        true
    }

    /// BGRA → I420 → reduz → LiveKit e prévia solicitada pela interface.
    fn enviar(&mut self, c: &Compartilhado, bgra: &[u8], passo: u32, largura: u32, altura: u32) {
        let (w, h) = (largura & !1, altura & !1);
        if w < 2 || h < 2 {
            return;
        }
        let mut i420 = I420Buffer::new(w, h);
        let (sy, su, sv) = i420.strides();
        let (y, u, v) = i420.data_mut();
        // No libyuv, "ARGB" é a ordem B,G,R,A na memória, que é o Bgra8 do Windows.
        yuv_helper::argb_to_i420(bgra, passo, y, sy, u, su, v, sv, w as i32, h as i32);
        let (sw, sh) = tamanho_saida(w, h, c.altura_max());
        if (sw, sh) != (w, h) {
            i420 = i420.scale(sw as i32, sh as i32);
        }
        let mut quadro = VideoFrame::new(VideoRotation::VideoRotation0, i420);
        quadro.timestamp_us = c.inicio.elapsed().as_micros() as i64;
        if c.fonte.capture_frame(&quadro) {
            c.contadores.enviados.fetch_add(1, Ordering::Relaxed);
        }
        // Retém apenas o último I420, depois de entregá-lo ao SDK. Nenhum redimensionamento
        // ou empacotamento da prévia compete com a entrega na thread de captura.
        let agora = c.inicio.elapsed().as_millis() as u64 + 1;
        let pedido = c.previa_pedido.load(Ordering::Relaxed);
        if pedido > 0 && agora.saturating_sub(pedido) < 500 {
            *c.previa.lock().unwrap() = Some(Arc::new(QuadroPrevia {
                buffer: std::sync::Mutex::new(quadro.buffer), largura: sw, altura: sh,
                instante: quadro.timestamp_us as u64 + 1,
            }));
        }
        c.contadores.largura.store(sw as u64, Ordering::Relaxed);
        c.contadores.altura.store(sh as u64, Ordering::Relaxed);
    }
}

impl GraphicsCaptureApiHandler for Captura {
    type Flags = (Arc<Compartilhado>, u64);
    type Error = Erro;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Erro> {
        let (c, geracao) = ctx.flags;
        Ok(Self { c, geracao, entrega: Entrega::nova() })
    }

    fn on_frame_arrived(&mut self, frame: &mut Frame, _: InternalCaptureControl) -> Result<(), Erro> {
        if self.c.geracao.load(Ordering::Relaxed) != self.geracao {
            return Ok(());
        }
        self.c.contadores.capturados.fetch_add(1, Ordering::Relaxed);
        if !self.entrega.na_hora(&self.c) {
            return Ok(());
        }
        let (w, h) = (frame.width(), frame.height());
        let mut dados = frame.buffer()?;
        let passo = dados.row_pitch();
        self.entrega.enviar(&self.c, dados.as_raw_buffer(), passo, w, h);
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Erro> {
        if self.c.geracao.load(Ordering::Relaxed) == self.geracao {
            self.c.contadores.fechada.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}

type FlagsCaptura = (Arc<Compartilhado>, u64);

/// Captura em andamento, de qualquer método. Parar sempre aguarda o fim (B1).
enum Controle {
    Wgc(CaptureControl<Captura, Erro>),
    /// Laço próprio (DXGI ou GDI), que pode ter caído para o WGC.
    Laco { parar: Arc<AtomicBool>, fio: std::thread::JoinHandle<()> },
}

/// Parada bloqueante: chamar fora da thread assíncrona.
trait Parar: Send + 'static {
    fn parar(self) -> Result<(), String>;
}
impl<C: GraphicsCaptureApiHandler + Send + 'static, E: Send + std::fmt::Debug + 'static> Parar for CaptureControl<C, E> {
    fn parar(self) -> Result<(), String> { self.stop().map_err(|e| format!("{e:?}")) }
}
impl Parar for Controle {
    fn parar(self) -> Result<(), String> {
        match self {
            Controle::Wgc(c) => c.parar(),
            Controle::Laco { parar, fio } => {
                parar.store(true, Ordering::Relaxed);
                fio.join().map_err(|_| "A captura terminou com erro.".to_string())
            }
        }
    }
}

/// Método de captura em uso (P3), para as estatísticas e o relatório.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Metodo { WgcSemBorda, Wgc, Dxgi }


fn wgc(alvo: Alvo, c: Arc<Compartilhado>, geracao: u64, borda: DrawBorderSettings) -> Result<CaptureControl<Captura, Erro>, String> {
    fn com<T: TryInto<GraphicsCaptureItemType>>(item: T, c: FlagsCaptura, borda: DrawBorderSettings) -> Settings<FlagsCaptura, T> {
        Settings::new(item, CursorCaptureSettings::WithCursor, borda, SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default, DirtyRegionSettings::Default, ColorFormat::Bgra8, c)
    }
    let r = match alvo {
        Alvo::Monitor(m) => Captura::start_free_threaded(com(m, (c, geracao), borda)),
        Alvo::Janela(j) => Captura::start_free_threaded(com(j, (c, geracao), borda)),
    };
    r.map_err(|e| format!("Não foi possível capturar essa fonte: {e}"))
}

/// O Windows deixa tirar a borda do WGC? (Windows 11; no 10 19045 não.)
fn wgc_sem_borda() -> bool {
    windows_capture::graphics_capture_api::GraphicsCaptureApi::is_border_settings_supported().unwrap_or(false)
}

/// Escolhe o método (P3, decidido com o humano):
/// - Windows 11: WGC sem borda para tudo.
/// - Windows 10: tela inteira pelo DXGI; janela em tela cheia ou maximizada pelo DXGI
///   recortado (sem borda; congela se outra janela ficar por cima); outras janelas pelo
///   WGC com a borda amarela (só quem transmite vê).
/// `STREAMSTUDIO_SO_WGC=1` força o método antigo (comparação e problemas).
fn iniciar_captura(alvo: Alvo, c: Arc<Compartilhado>, geracao: u64) -> Result<(Controle, Metodo), String> {
    if std::env::var_os("STREAMSTUDIO_SO_WGC").is_none() {
        if wgc_sem_borda() {
            if let Ok(w) = wgc(alvo, c.clone(), geracao, DrawBorderSettings::WithoutBorder) {
                return Ok((Controle::Wgc(w), Metodo::WgcSemBorda));
            }
        }
        let dxgi = match alvo {
            Alvo::Monitor(m) => Some((m, 0)),
            Alvo::Janela(j) => monitor_da_janela_grande(j).map(|m| (m, j.as_raw_hwnd() as usize)),
        };
        if let Some((m, janela)) = dxgi {
            if let Ok(controle) = laco_dxgi(m, janela, c.clone(), geracao) {
                return Ok((controle, Metodo::Dxgi));
            }
        }
    }
    Ok((Controle::Wgc(wgc(alvo, c, geracao, DrawBorderSettings::Default)?), Metodo::Wgc))
}

/// Contorno visível da janela (sem a moldura invisível de redimensionar do Windows 10).
fn contorno(hwnd: usize) -> Option<windows::Win32::Foundation::RECT> {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    let mut r = windows::Win32::Foundation::RECT::default();
    unsafe {
        DwmGetWindowAttribute(windows::Win32::Foundation::HWND(hwnd as *mut _), DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut _ as *mut _, std::mem::size_of::<windows::Win32::Foundation::RECT>() as u32).ok()?;
    }
    (r.right > r.left && r.bottom > r.top).then_some(r)
}

/// Monitor da janela se ela está em tela cheia ou maximizada; senão `None` (vai pelo WGC).
fn monitor_da_janela_grande(j: Window) -> Option<Monitor> {
    use windows::Win32::{Graphics::Gdi::*, UI::WindowsAndMessaging::IsZoomed};
    let hwnd = j.as_raw_hwnd() as usize;
    let r = crate::gdi::retangulo(hwnd)?;
    unsafe {
        let h = windows::Win32::Foundation::HWND(hwnd as *mut _);
        let m = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(m, &mut info).as_bool() { return None; }
        let t = info.rcMonitor;
        let tela_cheia = r.left <= t.left && r.top <= t.top && r.right >= t.right && r.bottom >= t.bottom;
        (tela_cheia || IsZoomed(h).as_bool()).then(|| Monitor::from_raw_hmonitor(m.0))
    }
}

/// Alguma janela "de verdade" (como as do Alt+Tab) está por cima e encostando nesta?
/// Overlays transparentes, menus, dicas e a barra de tarefas não contam.
fn janela_coberta(hwnd: usize, area: &windows::Win32::Foundation::RECT) -> bool {
    use windows::Win32::{Foundation::HWND, Graphics::Dwm::*, UI::WindowsAndMessaging::*};
    unsafe {
        let mut atual = GetWindow(HWND(hwnd as *mut _), GW_HWNDPREV).unwrap_or_default();
        let mut voltas = 0;
        while !atual.is_invalid() && voltas < 500 {
            voltas += 1;
            let estilo = GetWindowLongW(atual, GWL_EXSTYLE) as u32;
            let conta = IsWindowVisible(atual).as_bool() && !IsIconic(atual).as_bool()
                && estilo & (WS_EX_TOOLWINDOW.0 | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0) == 0
                && (GetWindow(atual, GW_OWNER).map(|d| d.is_invalid()).unwrap_or(true) || estilo & WS_EX_APPWINDOW.0 != 0)
                && {
                    let mut oculta = 0u32;
                    !(DwmGetWindowAttribute(atual, DWMWA_CLOAKED, &mut oculta as *mut _ as *mut _, 4).is_ok() && oculta != 0)
                };
            if conta {
                if let Some(r) = contorno(atual.0 as usize) {
                    if r.left < area.right && r.right > area.left && r.top < area.bottom && r.bottom > area.top {
                        return true;
                    }
                }
            }
            atual = GetWindow(atual, GW_HWNDPREV).unwrap_or_default();
        }
        false
    }
}

/// Origem do monitor na área de trabalho (para posicionar cursor e recorte).
fn retangulo_do_monitor(m: Monitor) -> windows::Win32::Foundation::RECT {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { let _ = GetMonitorInfoW(HMONITOR(m.as_raw_hmonitor()), &mut info); }
    info.rcMonitor
}

/// Inicia um laço numa thread e espera ele confirmar que abriu (ou o erro).
fn laco(nome: &str, corpo: impl FnOnce(Arc<AtomicBool>, std::sync::mpsc::SyncSender<Result<(), String>>) + Send + 'static) -> Result<Controle, String> {
    let parar = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let p = parar.clone();
    let fio = std::thread::Builder::new().name(nome.into()).spawn(move || corpo(p, tx)).map_err(|e| e.to_string())?;
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => Ok(Controle::Laco { parar, fio }),
        Ok(Err(e)) => { let _ = fio.join(); Err(e) }
        Err(_) => { parar.store(true, Ordering::Relaxed); Err("A captura demorou demais para começar.".into()) }
    }
}

/// Tela pelo DXGI Desktop Duplication: sem borda; o cursor é desenhado por nós.
/// Com `janela` ≠ 0, recorta a janela (tela cheia ou maximizada). Se outra janela fica por
/// cima, o recorte vazaria a outra: passa para o WGC da própria janela, que continua
/// capturando por baixo (com a borda amarela só enquanto isso), e volta ao recorte quando
/// ela fica visível (P8). Minimizada congela: o Windows não desenha janela minimizada.
fn laco_dxgi(m: Monitor, janela: usize, c: Arc<Compartilhado>, geracao: u64) -> Result<Controle, String> {
    use windows_capture::dxgi_duplication_api::{DxgiDuplicationApi, DxgiDuplicationFormat, Error as ErroDxgi};
    let hmonitor = m.as_raw_hmonitor() as usize;
    laco("captura-dxgi", move |parar, pronto| {
        let m = Monitor::from_raw_hmonitor(hmonitor as *mut _);
        let abrir = || DxgiDuplicationApi::new_options(m, &[DxgiDuplicationFormat::Bgra8]);
        let mut dup = match abrir() {
            Ok(d) => { let _ = pronto.send(Ok(())); d }
            Err(e) => { let _ = pronto.send(Err(format!("DXGI: {e}"))); return; }
        };
        let tela = retangulo_do_monitor(m);
        let mut entrega = Entrega::nova();
        let mut cursor = crate::gdi::Cursor::novo();
        let mut reserva: Option<CaptureControl<Captura, Erro>> = None;
        let mut visivel_desde: Option<Instant> = None;
        while !parar.load(Ordering::Relaxed) && c.geracao.load(Ordering::Relaxed) == geracao {
            // Recorte da janela no monitor, em coordenadas do monitor (x, y, largura, altura).
            let mut recorte = None;
            if janela != 0 {
                if !Window::from_raw_hwnd(janela as *mut _).is_valid() {
                    c.contadores.fechada.store(true, Ordering::Relaxed);
                    break;
                }
                let minimizada = unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(windows::Win32::Foundation::HWND(janela as *mut _)).as_bool() };
                let area = contorno(janela);
                let coberta = !minimizada && area.as_ref().map_or(true, |a| janela_coberta(janela, a));
                if coberta && reserva.is_none() {
                    reserva = wgc(Alvo::Janela(Window::from_raw_hwnd(janela as *mut _)), c.clone(), geracao, DrawBorderSettings::Default).ok();
                }
                visivel_desde = if coberta || minimizada { None } else { Some(visivel_desde.unwrap_or_else(Instant::now)) };
                // Só volta ao recorte depois de um tempo visível: um Alt+Tab rápido não pisca a borda.
                if visivel_desde.is_some_and(|t| t.elapsed() >= Duration::from_millis(400)) {
                    if let Some(r) = reserva.take() { let _ = r.parar(); }
                }
                // O aviso de "coberta" só aparece se o WGC não abriu e a imagem congelou.
                c.contadores.coberta.store(coberta && reserva.is_none(), Ordering::Relaxed);
                c.contadores.reserva_wgc.store(reserva.is_some(), Ordering::Relaxed);
                if minimizada || coberta || reserva.is_some() {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                let a = area.unwrap();
                let (x0, y0) = (a.left.max(tela.left), a.top.max(tela.top));
                let (x1, y1) = (a.right.min(tela.right), a.bottom.min(tela.bottom));
                if x1 - x0 < 2 || y1 - y0 < 2 { std::thread::sleep(Duration::from_millis(50)); continue; }
                recorte = Some(((x0 - tela.left) as u32, (y0 - tela.top) as u32, (x1 - x0) as u32, (y1 - y0) as u32));
            }
            let espera = (c.intervalo().as_millis() as u32).max(1);
            match dup.acquire_next_frame(espera) {
                Ok(mut quadro) => {
                    c.contadores.capturados.fetch_add(1, Ordering::Relaxed);
                    if !entrega.na_hora(&c) { continue; }
                    let Ok(mut buf) = quadro.buffer() else { continue };
                    let (w, h, passo) = (buf.width(), buf.height(), buf.row_pitch());
                    let dados = buf.as_raw_buffer();
                    cursor.desenhar(dados, passo as usize, w, h, tela.left, tela.top);
                    match recorte {
                        None => entrega.enviar(&c, dados, passo, w, h),
                        Some((x, y, rw, rh)) => {
                            let (rw, rh) = (rw.min(w.saturating_sub(x)), rh.min(h.saturating_sub(y)));
                            let inicio = (y * passo + x * 4) as usize;
                            entrega.enviar(&c, &dados[inicio..], passo, rw, rh);
                        }
                    }
                }
                Err(ErroDxgi::Timeout) => {}
                // Troca de resolução, tela do UAC, tela cheia exclusiva: recria a duplicação.
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(200));
                    if let Ok(d) = abrir() { dup = d; }
                }
            }
        }
        if let Some(r) = reserva.take() { let _ = r.parar(); }
        c.contadores.reserva_wgc.store(false, Ordering::Relaxed);
    })
}

/// O que foi combinado com o SDK ao publicar.
#[derive(Debug, Clone, Serialize)]
pub struct InfoTransmissao {
    pub codec: String,
    pub hardware: bool,
    pub largura: u32,
    pub altura: u32,
    pub fps: u32,
    pub mbps: u32,
    pub identidade: String,
}

/// Números do envio lidos das estatísticas do WebRTC (base das estatísticas da F2.7).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Envio {
    pub encoder: String,
    pub hardware: bool,
    pub largura: u32,
    pub altura: u32,
    pub fps: f64,
    pub quadros_enviados: u32,
    pub bytes_enviados: u64,
    /// Por que o WebRTC está baixando a qualidade: `none`, `cpu`, `bandwidth` ou `other`.
    pub limitacao: String,
    /// Fração de pacotes perdidos no último relatório de quem recebe (0 a 1).
    pub perda_fracao: f64,
    pub ping_ms: Option<f64>,
}

/// Leitura anterior, para calcular taxas por segundo (F2.7).
struct Leitura {
    quando: Instant,
    fonte: u64,
    quadros: u32,
    bytes: u64,
    bytes_som: Option<u64>,
}

pub struct Transmissao {
    pub cancelamento: Arc<crate::conexao::Cancelamento>,
    /// Identifica esta transmissão para o supervisor de reconexão (F2.8).
    pub id: u64,
    pub conexao: Arc<crate::conexao::EstadoConexao>,
    pub sala: Arc<Room>,
    faixa: LocalVideoTrack,
    opcoes: TrackPublishOptions,
    compartilhado: Arc<Compartilhado>,
    /// HWND da janela transmitida (0 se for monitor), para saber se foi minimizada.
    janela: usize,
    no_ar_desde: Instant,
    anterior: Option<Leitura>,
    sem_quadro_s: f64,
    encoder_atrasado_s: f64,
    /// Início da transmissão ou da última troca de fonte (para a medição inicial de banda).
    fonte_desde: Instant,
    /// Faixa de som (F2.5); `None` no modo sem som.
    pub som: Option<crate::audio::Som>,
    captura: Option<Controle>,
    /// Método de captura em uso (P3).
    pub metodo: Metodo,
    pub contadores: Arc<Contadores>,
    pub info: InfoTransmissao,
    /// Ajustes em uso (P4: mudam ao vivo).
    cfg: ConfigVideo,
}

pub struct Recuperacao {
    antiga: Arc<Room>,
    fonte: NativeVideoSource,
    opcoes: TrackPublishOptions,
    som: Option<livekit::webrtc::audio_source::native::NativeAudioSource>,
}
pub struct PublicacaoRecuperada {
    sala: Option<Arc<Room>>,
    faixa: LocalVideoTrack,
    som: Option<LocalAudioTrack>,
}
impl Drop for PublicacaoRecuperada {
    fn drop(&mut self) {
        if let Some(sala) = self.sala.take() {
            tauri::async_runtime::spawn(async move { let _ = tokio::time::timeout(Duration::from_secs(2), sala.close()).await; });
        }
    }
}
impl Recuperacao {
    pub async fn executar(self, url: &str, token: &str, estado: Arc<crate::conexao::EstadoConexao>) -> Result<PublicacaoRecuperada, String> {
        let _ = tokio::time::timeout(Duration::from_secs(2), self.antiga.close()).await;
        let sala = Arc::new(crate::conexao::conectar(url, token, estado.clone()).await?);
        estado.definir(crate::conexao::Conexao::Caiu);
        let faixa = LocalVideoTrack::create_video_track(estado.marca_envio(), RtcVideoSource::Native(self.fonte));
        // O guard fecha a sala nova até em cancelamento ou falha parcial de publicação.
        let mut p = PublicacaoRecuperada { sala: Some(sala.clone()), faixa: faixa.clone(), som: None };
        sala.local_participant().publish_track(LocalTrack::Video(faixa), self.opcoes).await.map_err(|e| e.to_string())?;
        if let Some(fonte) = self.som {
            let faixa = LocalAudioTrack::create_audio_track("som", livekit::webrtc::audio_source::RtcAudioSource::Native(fonte));
            sala.local_participant().publish_track(LocalTrack::Audio(faixa.clone()), crate::audio::opcoes_publicacao()).await.map_err(|e| e.to_string())?;
            p.som = Some(faixa);
        }
        Ok(p)
    }
}

async fn desfazer_inicio(sala: &Room, propria: bool, faixa: &LocalVideoTrack) {
    let _ = tokio::time::timeout(Duration::from_secs(2), sala.local_participant().unpublish_track(&faixa.sid())).await;
    if propria { let _ = tokio::time::timeout(Duration::from_secs(2), sala.close()).await; }
}

/// Só entrega o controle à transmissão depois de o som iniciar. Em falha, aguarda a parada
/// fora da thread assíncrona: descartar CaptureControl não encerra a captura do Windows.
async fn concluir_inicio<P: Parar, S>(captura: P, som: Result<S, String>) -> Result<(P, S), String> {
    match som {
        Ok(som) => Ok((captura, som)),
        Err(erro) => match tokio::task::spawn_blocking(move || captura.parar()).await {
            Ok(Ok(())) => Err(erro),
            Ok(Err(e)) => Err(format!("{erro} Não foi possível concluir a parada do vídeo: {e}")),
            Err(e) => Err(format!("{erro} A tarefa de parada do vídeo falhou: {e}")),
        },
    }
}

impl Transmissao {
    /// Conecta na sala com o token (de `/api/token`), publica vídeo e som e começa a capturar.
    pub async fn iniciar(
        url_livekit: &str,
        token: &str,
        fonte: &str,
        cfg: &ConfigVideo,
        audio: crate::audio::ModoAudio,
    ) -> Result<Self, String> {
        Self::iniciar_com_sala(url_livekit, token, fonte, cfg, audio, None).await
    }

    pub async fn iniciar_com_sala(
        url_livekit: &str, token: &str, fonte: &str, cfg: &ConfigVideo,
        audio: crate::audio::ModoAudio,
        existente: Option<(Arc<Room>, Arc<crate::conexao::EstadoConexao>)>,
    ) -> Result<Self, String> {
        cfg.validar()?;
        let alvo = achar_fonte(fonte)?;
        let (w, h) = tamanho_fonte(alvo);
        let (sw, sh) = tamanho_saida(w, h, cfg.res);
        let hardware = tem_encoder_hardware();
        let (codec, backend) = resolver_codec(cfg.codec, hardware);

        let sala_propria = existente.is_none();
        let (sala, conexao) = match existente {
            Some(par) => par,
            None => {
                let conexao = Arc::new(crate::conexao::EstadoConexao::default());
                let sala = Arc::new(crate::conexao::conectar(url_livekit, token, conexao.clone()).await?);
                (sala, conexao)
            }
        };
        let opcoes = opcoes_publicacao(codec, backend, cfg);

        // is_screencast = false, igual ao `contentHint = 'motion'` do site. Com true, o VP9 com
        // SVC L3T3_KEY não envia nenhum quadro neste SDK (ensaio da F2.4).
        let fonte_lk = NativeVideoSource::new(VideoResolution { width: sw, height: sh }, false);
        let faixa = LocalVideoTrack::create_video_track(conexao.marca_envio(), RtcVideoSource::Native(fonte_lk.clone()));
        if let Err(e) = sala
            .local_participant()
            .publish_track(LocalTrack::Video(faixa.clone()), opcoes.clone())
            .await
        {
            desfazer_inicio(&sala, sala_propria, &faixa).await;
            return Err(format!("O servidor recusou a transmissão: {e}"));
        }

        let contadores = Arc::new(Contadores::default());
        let compartilhado = Arc::new(Compartilhado {
            fonte: fonte_lk,
            inicio: Instant::now(),
            geracao: AtomicU64::new(0),
            intervalo_ns: AtomicU64::new((1e9 / cfg.fps as f64) as u64),
            altura_max: cfg.res.into(),
            contadores: contadores.clone(),
            previa: Default::default(), previa_pedido: AtomicU64::new(0), previa_memoria: Default::default(),
        });
        let (captura, metodo) = match iniciar_captura(alvo, compartilhado.clone(), 0) {
            Ok(c) => c,
            Err(e) => {
                desfazer_inicio(&sala, sala_propria, &faixa).await;
                return Err(e);
            }
        };
        let som = crate::audio::Som::iniciar(&sala, audio).await;
        let (captura, som) = match concluir_inicio(captura, som).await {
            Ok(par) => par,
            Err(e) => {
                desfazer_inicio(&sala, sala_propria, &faixa).await;
                return Err(e);
            }
        };
        let info = InfoTransmissao {
            codec: codec.as_str().into(),
            hardware: backend == VideoEncoderBackend::Hardware,
            largura: sw,
            altura: sh,
            fps: cfg.fps,
            mbps: cfg.mbps,
            identidade: sala.local_participant().identity().to_string(),
        };
        static PROXIMO_ID: AtomicU64 = AtomicU64::new(1);
        Ok(Self {
            cancelamento: Arc::default(),
            id: PROXIMO_ID.fetch_add(1, Ordering::Relaxed),
            conexao,
            sala,
            faixa,
            opcoes,
            compartilhado,
            janela: hwnd_de(alvo),
            no_ar_desde: Instant::now(),
            anterior: None,
            sem_quadro_s: 0.0,
            encoder_atrasado_s: 0.0,
            fonte_desde: Instant::now(),
            som,
            captura: Some(captura),
            metodo,
            cfg: cfg.clone(),
            contadores,
            info,
        })
    }

    /// Entra de novo na sala com um token novo e republica vídeo e som com as mesmas fontes.
    /// A captura continua rodando o tempo todo.
    pub fn preparar_reconexao(&self) -> Recuperacao {
        Recuperacao { antiga: self.sala.clone(), fonte: self.compartilhado.fonte.clone(), opcoes: self.opcoes.clone(), som: self.som.as_ref().map(|s| s.fonte.clone()) }
    }
    pub fn aplicar_reconexao(&mut self, p: &mut PublicacaoRecuperada) {
        self.sala = p.sala.take().expect("Sala recuperada");
        self.faixa = p.faixa.clone();
        if let (Some(s), Some(faixa)) = (self.som.as_mut(), p.som.take()) { s.faixa = faixa; }
        self.info.identidade = self.sala.local_participant().identity().to_string();
        self.anterior = None;
        self.fonte_desde = Instant::now();
    }

    pub async fn envio(&self) -> Envio {
        let mut e = Envio::default();
        let mut rtt_remoto = None;
        for s in self.faixa.get_stats().await.unwrap_or_default() {
            match s {
                RtcStats::RemoteInboundRtp(r) => {
                    e.perda_fracao = r.remote_inbound.fraction_lost.max(0.0);
                    if r.remote_inbound.round_trip_time_measurements > 0 {
                        rtt_remoto = Some(r.remote_inbound.round_trip_time * 1000.0);
                    }
                }
                RtcStats::CandidatePair(c) if c.candidate_pair.nominated && c.candidate_pair.current_round_trip_time > 0.0 => {
                    e.ping_ms = Some(c.candidate_pair.current_round_trip_time * 1000.0);
                }
                // sem simulcast há uma saída só, com ou sem SVC
                RtcStats::OutboundRtp(o) => {
                    e.encoder = o.outbound.encoder_implementation;
                    e.hardware = o.outbound.power_efficient_encoder;
                    e.largura = o.outbound.frame_width;
                    e.altura = o.outbound.frame_height;
                    e.fps = o.outbound.frames_per_second;
                    e.quadros_enviados = o.outbound.frames_sent;
                    e.bytes_enviados = o.sent.bytes_sent;
                    e.limitacao = format!("{:?}", o.outbound.quality_limitation_reason).to_lowercase();
                }
                _ => {}
            }
        }
        e.ping_ms = e.ping_ms.or(rtt_remoto);
        e
    }

    /// Troca a janela ou o monitor sem despublicar a faixa (como o `replaceTrack` do site).
    /// A captura nova começa antes de a antiga parar; se ela falhar, nada muda.
    pub async fn trocar_fonte(&mut self, fonte: &str) -> Result<(), String> {
        let alvo = achar_fonte(fonte)?;
        let c = &self.compartilhado;
        let nova = c.geracao.load(Ordering::Relaxed) + 1;
        let (captura, metodo) = iniciar_captura(alvo, c.clone(), nova)?;
        self.metodo = metodo;
        c.geracao.store(nova, Ordering::Relaxed);
        c.contadores.fechada.store(false, Ordering::Relaxed);
        let (w, h) = tamanho_fonte(alvo);
        (self.info.largura, self.info.altura) = tamanho_saida(w, h, c.altura_max());
        self.janela = hwnd_de(alvo);
        self.sem_quadro_s = 0.0;
        self.encoder_atrasado_s = 0.0;
        self.fonte_desde = Instant::now();
        if let Some(antiga) = self.captura.replace(captura) {
            let _ = tokio::task::spawn_blocking(move || antiga.parar()).await;
        }
        Ok(())
    }

    /// Estatísticas para o painel (F2.7). Chamar a cada ~1 s: as taxas são a diferença
    /// desde a chamada anterior (a primeira devolve taxas zeradas).
    pub async fn estatisticas(&mut self) -> crate::estatisticas::Estatisticas {
        use crate::estatisticas::{avaliar, Estatisticas, Sinais};
        let e = self.envio().await;
        let agora = Instant::now();
        let fonte = self.contadores.enviados.load(Ordering::Relaxed);
        let bytes_som = match &self.som {
            Some(s) => Some(s.bytes_enviados().await),
            None => None,
        };
        let leitura = Leitura { quando: agora, fonte, quadros: e.quadros_enviados, bytes: e.bytes_enviados, bytes_som };
        let (mut fps_captura, mut mbps, mut som_kbps) = (0.0, 0.0, None);
        if let Some(a) = self.anterior.take() {
            let dt = agora.duration_since(a.quando).as_secs_f64().max(0.001);
            fps_captura = fonte.saturating_sub(a.fonte) as f64 / dt;
            mbps = leitura.bytes.saturating_sub(a.bytes) as f64 * 8.0 / dt / 1e6;
            if let (Some(agora_som), Some(antes_som)) = (leitura.bytes_som, a.bytes_som) {
                som_kbps = Some(agora_som.saturating_sub(antes_som) as f64 * 8.0 / dt / 1e3);
            }
            self.sem_quadro_s = if leitura.quadros == a.quadros { self.sem_quadro_s + dt } else { 0.0 };
            let atrasado = fps_captura >= 10.0 && e.fps < fps_captura * crate::estatisticas::FRACAO_ENCODER_ATRASADO;
            self.encoder_atrasado_s = if atrasado { self.encoder_atrasado_s + dt } else { 0.0 };
        }
        self.anterior = Some(leitura);
        let minimizada = self.janela != 0
            && unsafe {
                windows::Win32::UI::WindowsAndMessaging::IsIconic(windows::Win32::Foundation::HWND(self.janela as *mut _)).as_bool()
            };
        let (gargalo, aviso) = avaliar(&Sinais {
            conectado: self.conexao.atual() == crate::conexao::Conexao::Conectado,
            limitacao: &e.limitacao,
            segundos_sem_quadro: self.sem_quadro_s,
            minimizada,
            coberta: self.contadores.coberta.load(Ordering::Relaxed),
            fechada: self.contadores.fechada.load(Ordering::Relaxed),
            segundos_no_ar: self.fonte_desde.elapsed().as_secs_f64(),
            segundos_encoder_atrasado: self.encoder_atrasado_s,
            codec: &self.info.codec,
        });
        let aviso = if self.conexao.atual() == crate::conexao::Conexao::Encerrada {
            Some("Essa sala foi encerrada. Saia e entre em outra sala.".into())
        } else { aviso.or_else(|| self.som.as_ref().and_then(|s| s.captura.contadores.aviso.lock().unwrap().clone())) };
        Estatisticas {
            no_ar_s: self.no_ar_desde.elapsed().as_secs(),
            largura: e.largura,
            altura: e.altura,
            codec: self.info.codec.clone(),
            encoder: e.encoder,
            hardware: e.hardware,
            fps_pedido: self.info.fps,
            fps_captura,
            fps_envio: e.fps,
            mbps,
            perda_pct: e.perda_fracao * 100.0,
            ping_ms: e.ping_ms,
            limitacao: e.limitacao,
            som_kbps,
            som_pico: self.som.as_ref().map(|s| s.captura.contadores.pico.load(Ordering::Relaxed) as f64 / 32767.0),
            gargalo,
            aviso,
            conexao: self.conexao.atual(),
            reentradas: self.conexao.reentradas.load(Ordering::Relaxed),
        }
    }

    /// Troca o modo de som. Publica ou despublica a faixa `som` só quando entra ou sai do "sem som".
    /// P4: muda os ajustes sem parar. Resolução e fps valem no próximo quadro. Mbps, codec e
    /// SVC exigem publicar o vídeo de novo: a faixa nova entra antes de a antiga sair, e quem
    /// assiste vê cerca de 1 s parado. Devolve se precisou republicar.
    pub async fn ajustar(&mut self, cfg: &ConfigVideo) -> Result<bool, String> {
        cfg.validar()?;
        self.compartilhado.definir(cfg.fps, cfg.res);
        self.info.fps = cfg.fps;
        let republicar = (cfg.codec, cfg.mbps, cfg.svc) != (self.cfg.codec, self.cfg.mbps, self.cfg.svc);
        if republicar {
            let (codec, backend) = resolver_codec(cfg.codec, tem_encoder_hardware());
            let opcoes = opcoes_publicacao(codec, backend, cfg);
            let faixa = LocalVideoTrack::create_video_track(self.conexao.marca_envio(), RtcVideoSource::Native(self.compartilhado.fonte.clone()));
            self.sala.local_participant().publish_track(LocalTrack::Video(faixa.clone()), opcoes.clone()).await
                .map_err(|e| format!("O servidor recusou o vídeo com os ajustes novos: {e}"))?;
            let antiga = std::mem::replace(&mut self.faixa, faixa);
            let _ = tokio::time::timeout(Duration::from_secs(2), self.sala.local_participant().unpublish_track(&antiga.sid())).await;
            self.opcoes = opcoes;
            self.info.codec = codec.as_str().into();
            self.info.hardware = backend == VideoEncoderBackend::Hardware;
            self.info.mbps = cfg.mbps;
            // As taxas da F2.7 comparam com a faixa anterior; recomeça a conta.
            self.anterior = None;
        }
        self.cfg = cfg.clone();
        Ok(republicar)
    }

    pub async fn trocar_audio(&mut self, modo: crate::audio::ModoAudio) -> Result<(), String> {
        use crate::audio::{ModoAudio, Som};
        match (self.som.take(), modo) {
            (Some(s), ModoAudio::SemSom) => {
                s.parar(&self.sala).await;
                Ok(())
            }
            (Some(mut s), m) => {
                let r = s.trocar(m);
                self.som = Some(s);
                r
            }
            (None, m) => {
                self.som = Som::iniciar(&self.sala, m).await?;
                Ok(())
            }
        }
    }

    /// Para a captura, despublica a faixa e sai da sala.
    pub async fn parar(self) {
        let (sala, _) = self.parar_mantendo_sala().await;
        let _ = tokio::time::timeout(Duration::from_secs(2), sala.close()).await;
    }

    /// A interface para o envio sem retirar a pessoa da sala.
    pub async fn parar_mantendo_sala(mut self) -> (Arc<Room>, Arc<crate::conexao::EstadoConexao>) {
        self.cancelamento.cancelar();
        self.conexao.liberar_supervisor();
        let previa=self.compartilhado.previa_memoria.lock().unwrap().take();
        if let Some(m)=previa { let _=tokio::task::spawn_blocking(move || m.encerrar()).await; }
        if let Some(c) = self.captura.take() {
            let _ = tokio::task::spawn_blocking(move || c.parar()).await;
        }
        if let Some(s) = self.som.take() {
            s.parar(&self.sala).await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), self.sala.local_participant().unpublish_track(&self.faixa.sid())).await;
        (self.sala, self.conexao)
    }
}

/// Transmissão ativa do app (no máximo uma).
#[derive(Default)]
pub struct Estado(pub crate::conexao::Ativa);

#[derive(Serialize)]
pub struct Padroes {
    pub config: ConfigVideo,
    pub codec_auto: String,
    pub hardware: bool,
}

#[tauri::command]
pub fn padroes_video() -> Padroes {
    let hardware = tem_encoder_hardware();
    Padroes { config: ConfigVideo::default(), codec_auto: resolver_codec(Codec::Auto, hardware).0.as_str().into(), hardware }
}

/// `servidor` vazio usa o padrão (`https://stream.exemplo.com/stream`).
#[tauri::command]
pub async fn iniciar_transmissao(
    estado: tauri::State<'_, Estado>,
    servidor: Option<String>,
    chave: String,
    apelido: String,
    fonte: String,
    config: Option<ConfigVideo>,
    audio: Option<crate::audio::ModoAudio>,
) -> Result<InfoTransmissao, String> {
    let mut atual = estado.0.lock().await;
    if let Some(t) = atual.take() {
        t.parar().await;
    }
    let api = match servidor.filter(|s| !s.trim().is_empty()) {
        Some(s) => crate::api::ClienteApi::novo(&s).map_err(|e| e.to_string())?,
        None => crate::api::ClienteApi::padrao(),
    };
    let acesso = api.token(&chave, &apelido).await.map_err(|e| e.to_string())?;
    let url = api.url_livekit();
    let t = Transmissao::iniciar(&url, &acesso.token, &fonte, &config.unwrap_or_default(), audio.unwrap_or_default()).await?;
    let info = t.info.clone();
    let supervisor = crate::conexao::Acesso { api, url, chave: acesso.chave, apelido };
    tauri::async_runtime::spawn(crate::conexao::supervisionar(estado.0.clone(), t.id, t.conexao.clone(), supervisor));
    *atual = Some(t);
    Ok(info)
}

#[tauri::command]
pub async fn trocar_fonte(estado: tauri::State<'_, Estado>, fonte: String) -> Result<InfoTransmissao, String> {
    let mut atual = estado.0.lock().await;
    let t = atual.as_mut().ok_or("Não há transmissão em andamento.")?;
    if t.conexao.em_recuperacao() || t.conexao.atual() != crate::conexao::Conexao::Conectado { return Err("Aguarde a reconexão ou pare o envio.".into()); }
    t.trocar_fonte(&fonte).await?;
    Ok(t.info.clone())
}

#[tauri::command]
pub async fn trocar_audio(estado: tauri::State<'_, Estado>, audio: crate::audio::ModoAudio) -> Result<(), String> {
    let mut atual = estado.0.lock().await;
    let t = atual.as_mut().ok_or("Não há transmissão em andamento.")?;
    if t.conexao.em_recuperacao() || t.conexao.atual() != crate::conexao::Conexao::Conectado { return Err("Aguarde a reconexão ou pare o envio.".into()); }
    t.trocar_audio(audio).await
}

/// P4: ajustes de vídeo durante o envio. Devolve as informações novas e se republicou.
#[tauri::command]
pub async fn ajustar_transmissao(estado: tauri::State<'_, Estado>, config: ConfigVideo) -> Result<(InfoTransmissao, bool), String> {
    let mut atual = estado.0.lock().await;
    let t = atual.as_mut().ok_or("Não há transmissão em andamento.")?;
    if t.conexao.em_recuperacao() || t.conexao.atual() != crate::conexao::Conexao::Conectado { return Err("Aguarde a reconexão para mudar os ajustes.".into()); }
    let republicou = t.ajustar(&config).await?;
    Ok((t.info.clone(), republicou))
}

#[tauri::command]
pub async fn estatisticas_transmissao(estado: tauri::State<'_, Estado>) -> Result<crate::estatisticas::Estatisticas, String> {
    let mut atual = estado.0.lock().await;
    Ok(atual.as_mut().ok_or("Não há transmissão em andamento.")?.estatisticas().await)
}

/// Negocia uma vista de memória somente leitura com o WebView2 principal.
#[tauri::command]
pub async fn preparar_previa(estado: tauri::State<'_, Estado>, janela: tauri::WebviewWindow, largura: u32, altura: u32) -> Result<(), String> {
    let c = estado.0.lock().await.as_ref().ok_or("Não há transmissão em andamento.")?.compartilhado.clone();
    if largura == 0 || altura == 0 {
        let anterior=c.previa_memoria.lock().unwrap().take();
        if let Some(m)=anterior { tauri::async_runtime::spawn_blocking(move || m.encerrar()).await.map_err(|e|e.to_string())?; }
        return Ok(());
    }
    let capacidade = (largura.clamp(2,4096) as usize*altura.clamp(2,4096) as usize*6).min(4096*4096*3/2);
    let memoria = crate::previa_memoria::preparar(janela, capacidade).await?;
    memoria.configurar(largura.clamp(2,4096),altura.clamp(2,4096));
    c.previa_pedido.store(c.inicio.elapsed().as_millis() as u64+1,Ordering::Relaxed);
    let fraco=Arc::downgrade(&c); let destino=memoria.clone();
    let tarefa=std::thread::Builder::new().name("previa-envio".into()).spawn(move || {
        let mut anterior=0; let mut tamanho_anterior=(0,0);
        while !destino.parada() {
            let Some(c)=fraco.upgrade() else { break };
            let agora=c.inicio.elapsed().as_millis() as u64+1;
            let ativo=agora.saturating_sub(c.previa_pedido.load(Ordering::Relaxed))<500;
            if ativo {
                let quadro=c.previa.lock().unwrap().clone();
                let tamanho=destino.tamanho();
                if let Some(quadro)=quadro {
                    if quadro.instante!=anterior || tamanho!=tamanho_anterior {
                        let mut p=quadro.gerar(tamanho.0,tamanho.1);
                        p.instante=c.inicio.elapsed().as_micros() as u64+1;
                        if destino.gravar(p.largura,p.altura,p.instante,&[&p.y,&p.u,&p.v]).is_ok() {
                            anterior=quadro.instante; tamanho_anterior=tamanho;
                        }
                    }
                }
            }
            drop(c);
            std::thread::park_timeout(Duration::from_millis(if ativo {5} else {50}));
        }
    }).map_err(|e|e.to_string())?;
    memoria.registrar(tarefa);
    let anterior=c.previa_memoria.lock().unwrap().replace(memoria);
    if let Some(m)=anterior { tauri::async_runtime::spawn_blocking(move || m.encerrar()).await.map_err(|e|e.to_string())?; }
    Ok(())
}

#[tauri::command]
pub async fn manter_previa(estado: tauri::State<'_, Estado>, largura:u32, altura:u32) -> Result<(),String> {
    let c=estado.0.lock().await.as_ref().ok_or("Não há transmissão em andamento.")?.compartilhado.clone();
    c.previa_pedido.store(c.inicio.elapsed().as_millis() as u64+1,Ordering::Relaxed);
    if let Some(m)=c.previa_memoria.lock().unwrap().as_ref() { m.configurar(largura.clamp(2,4096),altura.clamp(2,4096)); }
    Ok(())
}

/// Último quadro I420, sem JPEG/base64. Resposta vazia quando ainda não chegou outro quadro.
#[tauri::command]
pub async fn previa_envio(estado: tauri::State<'_, Estado>, largura: u32, altura: u32, ultimo: u64) -> Result<tauri::ipc::Response, String> {
    let c = estado.0.lock().await.as_ref().ok_or("Não há transmissão em andamento.")?.compartilhado.clone();
    c.previa_pedido.store(c.inicio.elapsed().as_millis() as u64 + 1, Ordering::Relaxed);
    let previa = c.previa.lock().unwrap().as_ref().filter(|p| p.instante != ultimo).cloned();
    let bytes = if let Some(previa) = previa {
        tauri::async_runtime::spawn_blocking(move || previa.gerar(largura.clamp(2,4096),altura.clamp(2,4096)).binario())
            .await.map_err(|e|e.to_string())?
    } else { Vec::new() };
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub async fn parar_transmissao(estado: tauri::State<'_, Estado>) -> Result<(), String> {
    if let Some(t) = estado.0.lock().await.take() {
        t.parar().await;
    }
    Ok(())
}

#[cfg(test)]
mod testes {
    use super::*;

    #[derive(Default)]
    struct CapturaNeutra;
    impl GraphicsCaptureApiHandler for CapturaNeutra {
        type Flags = ();
        type Error = String;
        fn new(_: Context<()>) -> Result<Self, String> { Ok(Self) }
        fn on_frame_arrived(&mut self, _: &mut Frame, _: InternalCaptureControl) -> Result<(), String> { Ok(()) }
    }

    fn controle_neutro() -> (CaptureControl<CapturaNeutra, String>, Arc<AtomicBool>) {
        use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, PeekMessageW, MSG, PM_NOREMOVE};
        let terminou = Arc::new(AtomicBool::new(false));
        let fim = terminou.clone();
        let (pronto, espera) = std::sync::mpsc::sync_channel(1);
        let fio = std::thread::spawn(move || {
            unsafe {
                let mut msg = MSG::default();
                // Cria a fila antes de liberar o teste; CaptureControl::stop envia WM_QUIT.
                let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                pronto.send(()).unwrap();
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {}
            }
            fim.store(true, Ordering::SeqCst);
            Ok(())
        });
        espera.recv_timeout(Duration::from_secs(2)).unwrap();
        let c = CaptureControl::new(fio, Arc::new(AtomicBool::new(false)), Arc::default());
        (c, terminou)
    }

    #[tokio::test]
    async fn falha_de_audio_para_e_aguarda_captura() {
        let (captura, terminou) = controle_neutro();
        let halt = captura.halt_handle();
        let Err(erro) = concluir_inicio(captura, Err::<(), _>("Falha de áudio de teste".into())).await
            else { panic!("A falha de áudio deve voltar ao chamador") };
        assert_eq!(erro, "Falha de áudio de teste");
        assert!(halt.load(Ordering::Relaxed), "A parada deve ser sinalizada");
        assert!(terminou.load(Ordering::SeqCst), "O retorno deve aguardar o fim da thread");
    }

    #[tokio::test]
    async fn audio_iniciado_preserva_captura() {
        let (captura, terminou) = controle_neutro();
        let halt = captura.halt_handle();
        let Ok((captura, som)) = concluir_inicio(captura, Ok::<_, String>(42)).await
            else { panic!("O início válido deve preservar a captura") };
        let preservada = !halt.load(Ordering::Relaxed) && !terminou.load(Ordering::SeqCst);
        tokio::task::spawn_blocking(move || captura.stop()).await.unwrap().unwrap();
        assert_eq!(som, 42);
        assert!(preservada);
    }

    /// Executar com a janela neutra da F1.1 aberta; não conecta à API ou ao LiveKit.
    #[test]
    fn previa_reduz_e_converte_cores() {
        // 1920x1080 cinza médio com um quadrado vermelho no canto: reduz para 960x540 e
        // as cores voltam certas depois do JPEG.
        let (w, h) = (1920u32, 1080u32);
        let mut i420 = I420Buffer::new(w, h);
        let (sy, su, sv) = i420.strides();
        let (y, u, v) = i420.data_mut();
        for l in 0..h { for c in 0..w { y[(l * sy + c) as usize] = if l < 200 && c < 200 { 81 } else { 126 }; } }
        for l in 0..h / 2 { for c in 0..w / 2 {
            let vermelho = l < 100 && c < 100;
            u[(l * su + c) as usize] = if vermelho { 90 } else { 128 };
            v[(l * sv + c) as usize] = if vermelho { 240 } else { 128 };
        } }
        let previa = Previa::de(&mut i420, w, h);
        assert_eq!((previa.largura, previa.altura), (960, 540));
        let jpeg = previa.jpeg().unwrap();
        let img = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap().to_rgb8();
        assert_eq!(img.dimensions(), (960, 540));
        let [r, g, b] = img.get_pixel(40, 40).0;
        assert!(r > 200 && g < 60 && b < 60, "vermelho virou {r},{g},{b}");
        let [r, g, b] = img.get_pixel(600, 400).0;
        assert!((r as i32 - g as i32).abs() < 8 && (g as i32 - b as i32).abs() < 8 && (110..150).contains(&r), "cinza virou {r},{g},{b}");
        // Fonte pequena não é ampliada.
        let mut pequena = I420Buffer::new(640, 360);
        assert_eq!(Previa::de(&mut pequena, 640, 360).largura, 640);
    }

    #[test]
    fn previa_binaria_preserva_planos_e_limita_ao_palco() {
        let mut fonte = I420Buffer::new(1920, 1080);
        let (y, u, v) = fonte.data_mut();
        y.fill(81); u.fill(90); v.fill(240);
        let previa = Previa::no_tamanho(&mut fonte, 1920, 1080, 1501, 801, 12345);
        assert_eq!((previa.largura, previa.altura), (1424, 800));
        let bytes = previa.binario();
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 1424);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 800);
        assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 12345);
        let area = 1424 * 800;
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 1);
        assert!(bytes.len() < area / 100);
        let mut planos = Vec::new();
        use std::io::Read;
        flate2::read::ZlibDecoder::new(&bytes[20..]).read_to_end(&mut planos).unwrap();
        assert_eq!(planos.len(), area * 3 / 2);
        assert!(planos[..area].iter().all(|&v| v == 81));
        assert!(planos[area..area*5/4].iter().all(|&v| v == 90));
        assert!(planos[area*5/4..].iter().all(|&v| v == 240));
        let detalhe = Previa { largura: 16, altura: 16, instante: 42,
            y: (0..256).map(|v| v as u8).collect(), u: (0..64).collect(), v: (64..128).collect() };
        let cru = detalhe.binario();
        assert_eq!(u32::from_le_bytes(cru[16..20].try_into().unwrap()), 0);
        assert_eq!(&cru[20..276], detalhe.y.as_slice());
        assert_eq!(cru.len(), 20 + 16*16*3/2);
        let inteira = Previa::no_tamanho(&mut fonte, 1920, 1080, 4096, 4096, 1);
        assert_eq!((inteira.largura, inteira.altura), (1920, 1080));
    }

    /// Com uma captura WGC real, a prévia sai da mesma captura e no tamanho pedido.
    /// A amostragem deste ensaio histórico ocorre a cada 20 ms.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "Ensaio WGC com janela neutra real; executar manualmente"]
    async fn previa_ao_vivo_wgc_real() {
        let janela = Window::from_contains_name("StreamStudio - teste WGC F1.1").expect("Abra a janela neutra da F1.1");
        let contadores = Arc::new(Contadores::default());
        let c = Arc::new(Compartilhado {
            fonte: NativeVideoSource::new(VideoResolution { width: 1920, height: 1080 }, false),
            inicio: Instant::now(), geracao: AtomicU64::new(0),
            intervalo_ns: AtomicU64::new(16_000_000), altura_max: 1080.into(), contadores: contadores.clone(), previa: Default::default(), previa_pedido: AtomicU64::new(1), previa_memoria: Default::default(),
        });
        let (captura, metodo) = iniciar_captura(Alvo::Janela(janela), c.clone(), 0).unwrap();
        println!("método: {metodo:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mut trocas = 0;
        let mut anterior: Option<Vec<u8>> = None;
        let inicio = Instant::now();
        while inicio.elapsed() < Duration::from_secs(3) {
            c.previa_pedido.store(c.inicio.elapsed().as_millis() as u64 + 1, Ordering::Relaxed);
            if let Some(p) = c.previa.lock().unwrap().as_ref().map(|p| p.gerar(960, 540)) {
                assert!(p.largura <= 960 && p.altura <= 540 && p.largura >= 2);
                if anterior.as_ref() != Some(&p.y) { trocas += 1; anterior = Some(p.y.clone()); }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let p = c.previa.lock().unwrap().as_ref().map(|p| p.gerar(960, 540)).expect("sem prévia");
        let jpeg = p.jpeg().unwrap();
        tokio::task::spawn_blocking(move || captura.parar()).await.unwrap().unwrap();
        println!("prévia {}x{}, {} quadros diferentes em 3 s, JPEG {} bytes, capturados {}",
            p.largura, p.altura, trocas, jpeg.len(), contadores.capturados.load(Ordering::Relaxed));
        assert!(trocas >= 20, "a prévia devia mudar ~10 vezes por segundo (janela animada); mudou {trocas}");
    }

    /// P3: métodos sem borda com fontes reais. Janela comum → GDI; monitor → DXGI.
    /// Mede quadros enviados por segundo com alvo de 60 fps e 1080p.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "Ensaio com janela neutra e monitor reais; executar manualmente"]
    async fn metodos_sem_borda_real() {
        let janela = Window::from_contains_name("StreamStudio - teste WGC F1.1").expect("Abra a janela neutra da F1.1");
        if let Ok(t) = std::env::var("ENSAIO_TAMANHO") {
            let (w, h) = t.split_once('x').map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap())).unwrap();
            unsafe { let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(windows::Win32::Foundation::HWND(janela.as_raw_hwnd()), 0, 0, w, h, true); }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // Não maximizada: WGC (com borda no Windows 10).
        assert!(monitor_da_janela_grande(janela).is_none());
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::*;
            let h = windows::Win32::Foundation::HWND(janela.as_raw_hwnd());
            let _ = ShowWindow(h, SW_MAXIMIZE);
            // O terminal do teste fica na frente; "sempre visível" tira a janela de baixo dele.
            let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
        let mut resultados = Vec::new();
        for (nome, alvo) in [("janela", Alvo::Janela(janela)), ("monitor", Alvo::Monitor(Monitor::primary().unwrap()))] {
            let contadores = Arc::new(Contadores::default());
            let c = Arc::new(Compartilhado {
                fonte: NativeVideoSource::new(VideoResolution { width: 1920, height: 1080 }, false),
                inicio: Instant::now(), geracao: AtomicU64::new(0),
                intervalo_ns: AtomicU64::new(16_666_667), altura_max: 1080.into(), contadores: contadores.clone(), previa: Default::default(), previa_pedido: AtomicU64::new(1), previa_memoria: Default::default(),
            });
            let (captura, metodo) = iniciar_captura(alvo, c.clone(), 0).unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
            // Sem faixa publicada o SDK não conta o quadro como enviado; mede as trocas da prévia
            // (a cada 100 ms) e os quadros capturados.
            let antes = contadores.capturados.load(Ordering::Relaxed);
            tokio::time::sleep(Duration::from_secs(3)).await;
            let fps = (contadores.capturados.load(Ordering::Relaxed) - antes) as f64 / 3.0;
            let tamanho = (contadores.largura.load(Ordering::Relaxed), contadores.altura.load(Ordering::Relaxed));
            println!("{nome}: coberta={}", contadores.coberta.load(Ordering::Relaxed));
            let previa = c.previa.lock().unwrap().as_ref().map(|p| p.gerar(960, 540)).expect("sem prévia");
            std::fs::create_dir_all("../../dev/estado/p3").unwrap();
            std::fs::write(format!("../../dev/estado/p3/{nome}.jpg"), previa.jpeg().unwrap()).unwrap();
            tokio::task::spawn_blocking(move || captura.parar()).await.unwrap().unwrap();
            println!("{nome}: método {metodo:?}, {fps:.1} quadros/s capturados, {}x{}", tamanho.0, tamanho.1);
            resultados.push((nome, metodo, fps));
        }
        assert_eq!(resultados[0].1, Metodo::Dxgi, "janela maximizada vai pelo recorte DXGI");
        assert_eq!(resultados[1].1, Metodo::Dxgi);
    }

    /// P8: janela maximizada (recorte DXGI) coberta por outra continua sendo transmitida
    /// pelo WGC, com a imagem dela e não a de cima, e volta ao recorte quando fica visível.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "Ensaio com janelas neutras reais; executar manualmente"]
    async fn janela_coberta_continua_real() {
        use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};
        let janela = Window::from_contains_name("StreamStudio - teste WGC F1.1").expect("Abra a janela neutra da F1.1");
        let h = HWND(janela.as_raw_hwnd());
        unsafe { let _ = ShowWindow(h, SW_MAXIMIZE); let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE); }
        tokio::time::sleep(Duration::from_millis(700)).await;
        let contadores = Arc::new(Contadores::default());
        let c = Arc::new(Compartilhado {
            fonte: NativeVideoSource::new(VideoResolution { width: 1920, height: 1080 }, false),
            inicio: Instant::now(), geracao: AtomicU64::new(0),
            intervalo_ns: AtomicU64::new(16_666_667), altura_max: 1080.into(), contadores: contadores.clone(), previa: Default::default(), previa_pedido: AtomicU64::new(1), previa_memoria: Default::default(),
        });
        let (captura, metodo) = iniciar_captura(Alvo::Janela(janela), c.clone(), 0).unwrap();
        assert_eq!(metodo, Metodo::Dxgi, "janela maximizada vai pelo recorte DXGI");
        // fps capturado em 2 s e a cor (U, V) de um canto da prévia: azul tem U alto e V baixo.
        let medir = || { let c = c.clone(); let contadores = contadores.clone(); async move {
            let antes = contadores.capturados.load(Ordering::Relaxed);
            let fim = Instant::now() + Duration::from_secs(2);
            while Instant::now() < fim {
                c.previa_pedido.store(c.inicio.elapsed().as_millis() as u64 + 1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let fps = (contadores.capturados.load(Ordering::Relaxed) - antes) as f64 / 2.0;
            let p = c.previa.lock().unwrap().as_ref().map(|p| p.gerar(320, 180)).expect("sem prévia");
            let (pw, ph) = (p.largura as usize / 2, p.altura as usize / 2);
            let i = (ph * 8 / 10) * pw + pw / 10;
            (fps, p.u[i], p.v[i], contadores.reserva_wgc.load(Ordering::Relaxed), contadores.coberta.load(Ordering::Relaxed))
        } };
        let visivel = medir().await;
        println!("visível: {visivel:?}");
        // Outra janela neutra, vermelha, maximizada e sempre visível por cima.
        let script = std::env::temp_dir().join("streamstudio-cobre-p8.ps1");
        std::fs::write(&script, include_str!("../../spikes/wgc/janela-teste.ps1")
            .replace("'StreamStudio - teste WGC F1.1'", "'StreamStudio - cobre P8'")
            .replace("MidnightBlue", "DarkRed")).unwrap();
        let mut cobre = std::process::Command::new("powershell").args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(&script).spawn().unwrap();
        let mut outra = None;
        for _ in 0..100 { if let Ok(w) = Window::from_contains_name("StreamStudio - cobre P8") { outra = Some(w); break; } tokio::time::sleep(Duration::from_millis(100)).await; }
        let outra = HWND(outra.expect("janela de cima não abriu").as_raw_hwnd());
        unsafe {
            let _ = SetWindowPos(h, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            let _ = ShowWindow(outra, SW_MAXIMIZE);
            let _ = SetWindowPos(outra, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let coberta = medir().await;
        println!("coberta: {coberta:?}");
        let _ = cobre.kill(); let _ = cobre.wait();
        unsafe { let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE); }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let de_novo = medir().await;
        println!("visível de novo: {de_novo:?}");
        tokio::task::spawn_blocking(move || captura.parar()).await.unwrap().unwrap();
        unsafe { let _ = SetWindowPos(h, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE); let _ = ShowWindow(h, SW_RESTORE); }
        let azul = |(_, u, v, _, _): (f64, u8, u8, bool, bool)| u > 140 && v < 135;
        assert!(!visivel.3 && azul(visivel) && visivel.0 >= 40.0, "visível: recorte DXGI com a janela azul");
        assert!(coberta.3 && !coberta.4, "coberta: deve estar no WGC, sem aviso de congelada");
        assert!(azul(coberta), "coberta: a imagem deve ser a da janela transmitida, não a de cima");
        assert!(coberta.0 >= 40.0, "coberta: a captura não pode parar ({} quadros/s)", coberta.0);
        assert!(!de_novo.3 && azul(de_novo) && de_novo.0 >= 40.0, "visível de novo: volta ao recorte DXGI");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "Ensaio WGC com janela neutra real; executar manualmente"]
    async fn falha_de_audio_encerra_wgc_real() {
        use livekit::webrtc::audio_source::{native::NativeAudioSource, AudioSourceOptions};
        let janela = Window::from_contains_name("StreamStudio - teste WGC F1.1").expect("Abra a janela neutra da F1.1");
        for tentativa in 1..=2 {
            let contadores = Arc::new(Contadores::default());
            let c = Arc::new(Compartilhado {
                fonte: NativeVideoSource::new(VideoResolution { width: 960, height: 540 }, false),
                inicio: Instant::now(), geracao: AtomicU64::new(0),
                intervalo_ns: AtomicU64::new(16_000_000), altura_max: 720.into(), contadores: contadores.clone(), previa: Default::default(), previa_pedido: AtomicU64::new(1), previa_memoria: Default::default(),
            });
            let captura = wgc(Alvo::Janela(janela), c, 0, DrawBorderSettings::Default).unwrap();
            let halt = captura.halt_handle();
            let callback = Arc::downgrade(&captura.callback());
            for _ in 0..100 {
                if contadores.capturados.load(Ordering::Relaxed) > 0 { break; }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(contadores.capturados.load(Ordering::Relaxed) > 0, "WGC deve entregar quadros antes da falha");
            let fonte_som = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 2, 0);
            let som = crate::audio::CapturaAudio::iniciar(crate::audio::ModoAudio::App { pid: u32::MAX }, fonte_som);
            let Err(erro) = concluir_inicio(captura, som).await else { panic!("PID inexistente deve falhar") };
            assert!(erro.contains("Esse programa foi fechado"), "Erro inesperado: {erro}");
            assert!(halt.load(Ordering::Relaxed));
            assert!(callback.upgrade().is_none(), "O handler WGC deve ser liberado");
            let antes = contadores.capturados.load(Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(antes, contadores.capturados.load(Ordering::Relaxed), "Não pode haver quadros após o erro");
            println!("B1 tentativa {tentativa}: {antes} quadros antes do erro; captura parada e handler liberado");
        }
    }

    #[test]
    fn padroes_iguais_ao_site() {
        let c = ConfigVideo::default();
        assert_eq!((c.res, c.fps, c.mbps, c.codec, c.svc), (1080, 60, 12, Codec::Auto, false));
        assert!(c.validar().is_ok());
        let parcial: ConfigVideo = serde_json::from_str(r#"{"fps":30,"codec":"h264"}"#).unwrap();
        assert_eq!((parcial.fps, parcial.codec, parcial.res, parcial.mbps), (30, Codec::H264, 1080, 12));
        for ruim in [r#"{"res":900}"#, r#"{"fps":144}"#, r#"{"mbps":0}"#, r#"{"mbps":80}"#] {
            assert!(serde_json::from_str::<ConfigVideo>(ruim).unwrap().validar().is_err(), "{ruim}");
        }
    }

    #[test]
    fn codec_automatico() {
        assert_eq!(resolver_codec(Codec::Auto, false), (VideoCodec::VP9, VideoEncoderBackend::Auto));
        assert_eq!(resolver_codec(Codec::Auto, true), (VideoCodec::H264, VideoEncoderBackend::Hardware));
        assert_eq!(resolver_codec(Codec::H264, false).0, VideoCodec::H264);
        assert_eq!(resolver_codec(Codec::Vp9, true), (VideoCodec::VP9, VideoEncoderBackend::Auto));
        let o = opcoes_publicacao(VideoCodec::VP9, VideoEncoderBackend::Auto, &ConfigVideo::default());
        assert_eq!(o.scalability_mode, None);
        let com_svc = ConfigVideo { svc: true, ..Default::default() };
        let v = opcoes_publicacao(VideoCodec::VP9, VideoEncoderBackend::Auto, &com_svc);
        assert_eq!(v.scalability_mode.as_deref(), Some("L3T3_KEY"));
        assert!(!o.simulcast);
        assert_eq!(o.source, TrackSource::Screenshare);
        let e = o.video_encoding.unwrap();
        assert_eq!((e.max_bitrate, e.max_framerate), (12_000_000, 60.0));
        let h = opcoes_publicacao(VideoCodec::H264, VideoEncoderBackend::Auto, &com_svc);
        assert_eq!(h.scalability_mode, None);
    }

    #[test]
    fn reduz_mantendo_proporcao() {
        assert_eq!(tamanho_saida(3840, 2160, 1080), (1920, 1080));
        assert_eq!(tamanho_saida(2560, 1440, 1080), (1920, 1080));
        assert_eq!(tamanho_saida(1920, 1080, 1440), (1920, 1080));
        assert_eq!(tamanho_saida(963, 571, 720), (962, 570));
        assert_eq!(tamanho_saida(3440, 1440, 1080), (2580, 1080));
    }

    #[test]
    fn fonte_invalida() {
        assert!(achar_fonte("nada").is_err());
        assert!(achar_fonte("janela:abc").is_err());
        assert!(achar_fonte("janela:1").is_err());
        assert!(achar_fonte("monitor:1").is_err());
    }
    /// Ensaio de ponta a ponta contra o site local (`dev/subir.ps1`) com uma janela real aberta.
    /// Variáveis: ENSAIO_TITULO (padrão: a fonte da F1.4), ENSAIO_CONFIG (JSON de `ConfigVideo`),
    /// ENSAIO_SEGUNDOS (40), ENSAIO_SAIDA (`dev/estado/f24`), ENSAIO_SERVIDOR (`http://localhost:8080`),
    /// ENSAIO_LIVEKIT (padrão: o mesmo do site, como o app faz), ENSAIO_CHAVE (sala já aberta),
    /// ENSAIO_AUDIO (JSON de `ModoAudio`, padrão sem som). Imprime o painel da F2.7 a cada segundo (`PAINEL`).
    /// Grava `sala.json` logo no começo (para um navegador entrar pelo convite) e `resumo.json` no fim.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "Ensaio com site local, LiveKit e janela real; executar manualmente"]
    async fn ensaio_transmissao_local() {
        let var = |n: &str, p: &str| std::env::var(n).unwrap_or_else(|_| p.to_string());
        let raiz = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let saida = raiz.join(var("ENSAIO_SAIDA", "dev/estado/f24"));
        std::fs::create_dir_all(&saida).unwrap();
        let cfg: ConfigVideo = serde_json::from_str(&var("ENSAIO_CONFIG", "{}")).unwrap();
        let segundos: u64 = var("ENSAIO_SEGUNDOS", "40").parse().unwrap();
        let janela = Window::from_contains_name(&var("ENSAIO_TITULO", "StreamStudio - fonte F1.4")).expect("Janela de ensaio não encontrada");
        let fonte = format!("janela:{}", janela.as_raw_hwnd() as usize);

        let api = crate::api::ClienteApi::novo(&var("ENSAIO_SERVIDOR", "http://localhost:8080")).unwrap();
        let carimbo = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        // A API limita 4 salas novas por hora: ENSAIO_CHAVE reaproveita uma sala aberta.
        let chave = match std::env::var("ENSAIO_CHAVE") {
            Ok(c) => c,
            Err(_) => api.criar_sala(&format!("Ensaio F2.4 {carimbo}"), "ensaio-local-123", 60).await.unwrap().chave,
        };
        let acesso = api.token(&chave, "app").await.unwrap();
        let mut sala = serde_json::to_value(&acesso.sala).unwrap();
        sala["chave"] = acesso.chave.clone().into();
        std::fs::write(saida.join("sala.json"), serde_json::to_vec_pretty(&sala).unwrap()).unwrap();

        let url = std::env::var("ENSAIO_LIVEKIT").unwrap_or_else(|_| api.url_livekit());
        let audio: crate::audio::ModoAudio = serde_json::from_str(&var("ENSAIO_AUDIO", r#"{"modo":"sem_som"}"#)).unwrap();
        let t = Transmissao::iniciar(&url, &acesso.token, &fonte, &cfg, audio).await.unwrap();
        println!("{:?}", t.info);
        let inicio = Instant::now();
        let mut amostras = Vec::new();
        let mut anterior = (0u64, 0u64, Instant::now());
        // F2.6: ENSAIO_TROCA_TITULO e/ou ENSAIO_TROCA_AUDIO trocam a fonte na metade do tempo.
        let troca_titulo = std::env::var("ENSAIO_TROCA_TITULO").ok();
        let troca_audio: Option<crate::audio::ModoAudio> =
            std::env::var("ENSAIO_TROCA_AUDIO").ok().map(|j| serde_json::from_str(&j).unwrap());
        let mut t = t;
        let mut troca = serde_json::Value::Null;
        let faixa_antes = t.faixa.sid();
        let mut paineis = Vec::new();
        let _ = t.estatisticas().await;
        while inicio.elapsed() < Duration::from_secs(segundos) {
            // painel a cada 1 s, como a interface fará; amostra completa a cada 5 s
            for _ in 0..5 {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let p = t.estatisticas().await;
                println!("PAINEL {}", serde_json::to_string(&p).unwrap());
                paineis.push(p);
            }
            if troca.is_null() && inicio.elapsed() >= Duration::from_secs(segundos / 2)
                && (troca_titulo.is_some() || troca_audio.is_some())
            {
                let antes = Instant::now();
                if let Some(titulo) = &troca_titulo {
                    let j = Window::from_contains_name(titulo).expect("Janela da troca não encontrada");
                    t.trocar_fonte(&format!("janela:{}", j.as_raw_hwnd() as usize)).await.unwrap();
                }
                if let Some(m) = troca_audio {
                    t.trocar_audio(m).await.unwrap();
                }
                troca = serde_json::json!({"s": inicio.elapsed().as_secs(), "duracao_s": antes.elapsed().as_secs_f64(),
                    "info": t.info.clone()});
                println!("TROCA {troca}");
                // marca, para o navegador saber quando medir de novo
                std::fs::write(saida.join("trocou"), troca.to_string()).unwrap();
            }
            let e = t.envio().await;
            let cap = t.contadores.capturados.load(Ordering::Relaxed);
            let env = t.contadores.enviados.load(Ordering::Relaxed);
            let dt = anterior.2.elapsed().as_secs_f64();
            let som = t.som.as_ref().map(|s| {
                let c = &s.captura.contadores;
                serde_json::json!({"quadros": c.quadros.load(Ordering::Relaxed), "pico": c.pico.load(Ordering::Relaxed),
                    "alvo": c.alvo.load(Ordering::Relaxed)})
            });
            let a = serde_json::json!({"s": inicio.elapsed().as_secs(), "fps_captura": (cap - anterior.0) as f64 / dt,
                "fps_fonte": (env - anterior.1) as f64 / dt, "envio": e, "som": som});
            println!("{a}");
            amostras.push(a);
            anterior = (cap, env, Instant::now());
        }
        assert_eq!(t.faixa.sid(), faixa_antes, "A troca não pode republicar o vídeo");
        let ultimo = t.envio().await;
        let info = t.info.clone();
        let parando = Instant::now();
        t.parar().await;
        let parar_s = parando.elapsed().as_secs_f64();
        let resumo = serde_json::json!({"config": cfg, "audio": audio, "info": info, "amostras": amostras, "parar_s": parar_s,
            "troca": troca, "paineis": paineis});
        std::fs::write(saida.join("resumo.json"), serde_json::to_vec_pretty(&resumo).unwrap()).unwrap();

        assert!(!ultimo.encoder.is_empty(), "Sem estatísticas de envio");
        assert!(ultimo.quadros_enviados > 0);
        // O encoder começa menor e sobe conforme a banda; o VP9 pode cortar alguns pixels (1080 → 1076).
        // Com troca, só valem as amostras depois dela (a fonte nova tem outro tamanho).
        let desde = troca["s"].as_u64().unwrap_or(0);
        let maior = amostras
            .iter()
            .filter(|a| a["s"].as_u64().unwrap_or(0) > desde)
            .filter_map(|a| a["envio"]["altura"].as_u64())
            .max()
            .unwrap_or(0) as u32;
        assert!(maior <= info.altura && maior + 8 >= info.altura, "Não chegou à resolução pedida: {maior} de {}", info.altura);
        assert!(parar_s < 10.0, "Parar demorou demais");
    }
    /// F2.8: transmite com o supervisor de reconexão ligado e imprime o painel a cada segundo.
    /// Quem roda derruba o proxy ou o LiveKit no meio (ver `ia/resultados/F2.8.md`).
    /// Variáveis como no ensaio acima: ENSAIO_TITULO, ENSAIO_SEGUNDOS, ENSAIO_SAIDA, ENSAIO_SERVIDOR.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "Ensaio com site local, LiveKit e janela real; executar manualmente"]
    async fn ensaio_reconexao() {
        let var = |n: &str, p: &str| std::env::var(n).unwrap_or_else(|_| p.to_string());
        let raiz = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let saida = raiz.join(var("ENSAIO_SAIDA", "dev/estado/f28"));
        std::fs::create_dir_all(&saida).unwrap();
        let segundos: u64 = var("ENSAIO_SEGUNDOS", "60").parse().unwrap();
        let janela = Window::from_contains_name(&var("ENSAIO_TITULO", "StreamStudio - fonte F1.4")).expect("Janela de ensaio não encontrada");
        let fonte = format!("janela:{}", janela.as_raw_hwnd() as usize);
        let api = crate::api::ClienteApi::novo(&var("ENSAIO_SERVIDOR", "http://localhost:8080")).unwrap();
        let carimbo = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let sala = api.criar_sala(&format!("Ensaio F2.8 {carimbo}"), "ensaio-local-123", 60).await.unwrap();
        let acesso = api.token(&sala.chave, "app").await.unwrap();
        std::fs::write(saida.join("sala.json"), serde_json::to_vec_pretty(&sala).unwrap()).unwrap();
        let url = api.url_livekit();
        let t = Transmissao::iniciar(&url, &acesso.token, &fonte, &ConfigVideo::default(), crate::audio::ModoAudio::SemSom)
            .await
            .unwrap();
        let (id, conexao) = (t.id, t.conexao.clone());
        let ativa: crate::conexao::Ativa = Default::default();
        *ativa.lock().await = Some(t);
        let acesso = crate::conexao::Acesso { api, url, chave: acesso.chave, apelido: "app".into() };
        let supervisor = tokio::spawn(crate::conexao::supervisionar(ativa.clone(), id, conexao.clone(), acesso));
        let inicio = Instant::now();
        let mut paineis = Vec::new();
        while inicio.elapsed() < Duration::from_secs(segundos) {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let p = match ativa.lock().await.as_mut() {
                Some(t) => t.estatisticas().await,
                None => break,
            };
            println!("PAINEL {}", serde_json::to_string(&p).unwrap());
            paineis.push(p);
        }
        let t = ativa.lock().await.take().unwrap();
        t.parar().await;
        // parar acorda o supervisor, que tem de terminar sozinho
        tokio::time::timeout(Duration::from_secs(5), supervisor).await.expect("O supervisor não terminou").unwrap();
        std::fs::write(saida.join("paineis.json"), serde_json::to_vec_pretty(&paineis).unwrap()).unwrap();
        let ultimo = paineis.last().unwrap();
        assert_eq!(ultimo.conexao, crate::conexao::Conexao::Conectado);
        assert!(ultimo.fps_envio > 0.0, "O vídeo não voltou");
    }
}
