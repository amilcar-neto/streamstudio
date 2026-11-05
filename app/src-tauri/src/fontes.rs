//! Descoberta nativa; miniaturas são transitórias e não são gravadas em disco.
use std::{collections::BTreeMap, io::Cursor, sync::mpsc, time::Duration};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Serialize;
use windows_capture::{capture::{Context, GraphicsCaptureApiHandler}, frame::Frame,
    graphics_capture_api::InternalCaptureControl, monitor::Monitor, settings::*, window::Window};
use windows::{core::Interface, Win32::{Foundation::CloseHandle, Media::Audio::*,
    System::{Com::*, Diagnostics::ToolHelp::*}}};

type Erro = Box<dyn std::error::Error + Send + Sync>;

#[derive(Serialize)]
pub struct Fonte {
    pub id: String,
    pub tipo: &'static str,
    pub nome: String,
    pub pid: Option<u32>,
    pub largura: u32,
    pub altura: u32,
    pub miniatura: Option<String>,
    pub aviso: Option<String>,
    /// Ícone do programa (PNG 48x48), para janelas.
    pub icone: Option<String>,
    /// A janela está minimizada: a miniatura, se houver, é a última vista.
    pub minimizada: bool,
}

struct Miniatura(mpsc::SyncSender<Result<String, String>>);
impl GraphicsCaptureApiHandler for Miniatura {
    type Flags = mpsc::SyncSender<Result<String, String>>;
    type Error = Erro;
    fn new(ctx: Context<Self::Flags>) -> Result<Self, Erro> { Ok(Self(ctx.flags)) }
    fn on_frame_arrived(&mut self, frame: &mut Frame, controle: InternalCaptureControl) -> Result<(), Erro> {
        let resultado = (|| -> Result<String, Erro> {
            let (w, h) = (frame.width(), frame.height());
            let buffer = frame.buffer()?;
            let mut contiguo = Vec::new();
            let dados = buffer.as_nopadding_buffer(&mut contiguo);
            let imagem = image::RgbaImage::from_raw(w, h, dados.to_vec()).ok_or("Quadro inválido")?;
            let pequena = image::DynamicImage::ImageRgba8(imagem).thumbnail(320, 180);
            let mut png = Cursor::new(Vec::new());
            pequena.write_to(&mut png, image::ImageFormat::Png)?;
            Ok(format!("data:image/png;base64,{}", STANDARD.encode(png.into_inner())))
        })().map_err(|e| format!("Miniatura indisponível: {e}"));
        let _ = self.0.try_send(resultado);
        controle.stop();
        Ok(())
    }
}

fn miniatura(item: impl TryInto<GraphicsCaptureItemType> + Send + 'static) -> Result<String, String> {
    let (tx, rx) = mpsc::sync_channel(1);
    let settings = Settings::new(item, CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::Default, SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default, DirtyRegionSettings::Default, ColorFormat::Rgba8, tx);
    let controle = Miniatura::start_free_threaded(settings).map_err(|e| e.to_string())?;
    let resultado = rx.recv_timeout(Duration::from_millis(800))
        .unwrap_or_else(|_| Err("A fonte não entregou uma miniatura no prazo.".into()));
    controle.stop().map_err(|e| e.to_string())?;
    resultado
}

/// Reduz para caber em 320x180 e devolve como data URL PNG.
fn png_de(img: crate::gdi::Imagem, max: (u32, u32)) -> Option<String> {
    let imagem = image::RgbaImage::from_raw(img.largura, img.altura, img.rgba)?;
    let pequena = image::DynamicImage::ImageRgba8(imagem).thumbnail(max.0, max.1);
    let mut png = Cursor::new(Vec::new());
    pequena.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(format!("data:image/png;base64,{}", STANDARD.encode(png.into_inner())))
}

/// Última miniatura vista de cada janela: aparece quando ela é minimizada depois (P2).
fn guardadas() -> &'static std::sync::Mutex<std::collections::HashMap<usize, String>> {
    static G: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<usize, String>>> = std::sync::OnceLock::new();
    G.get_or_init(Default::default)
}

/// Miniatura de uma janela (P2): primeiro o GDI (instantâneo, sem borda); se vier preto,
/// o WGC; se estiver minimizada, a última guardada. Mais o ícone do programa.
fn miniatura_janela(hwnd: usize) -> (Option<String>, Option<String>, bool, Option<String>) {
    let minimizada = unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(windows::Win32::Foundation::HWND(hwnd as *mut _)).as_bool() };
    let icone = crate::gdi::icone(hwnd, 48).and_then(|i| png_de(i, (48, 48)));
    if minimizada {
        let guardada = guardadas().lock().unwrap().get(&hwnd).cloned();
        return (guardada, icone, true, None);
    }
    let mut aviso = None;
    let mini = match crate::gdi::janela(hwnd).filter(|i| !i.preta()) {
        Some(i) => png_de(i, (320, 180)),
        None => miniatura(Window::from_raw_hwnd(hwnd as *mut _)).map_err(|e| aviso = Some(e)).ok(),
    };
    if let Some(m) = &mini { guardadas().lock().unwrap().insert(hwnd, m.clone()); }
    (mini, icone, false, aviso)
}

/// Janela "fantasma" (app da loja suspenso, janelas internas do Windows): o DWM esconde
/// e ela nem aparece no Alt+Tab. Não dá para capturar.
fn escondida(hwnd: usize) -> bool {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    let mut oculta = 0u32;
    unsafe {
        DwmGetWindowAttribute(windows::Win32::Foundation::HWND(hwnd as *mut _), DWMWA_CLOAKED,
            &mut oculta as *mut _ as *mut _, std::mem::size_of::<u32>() as u32).is_ok() && oculta != 0
    }
}

fn retangulo_monitor(m: &Monitor) -> Option<(i32, i32, u32, u32)> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(HMONITOR(m.as_raw_hmonitor()), &mut info).as_bool() }.then(|| {
        let r = info.rcMonitor;
        (r.left, r.top, (r.right - r.left) as u32, (r.bottom - r.top) as u32)
    })
}

pub fn fontes() -> Result<Vec<Fonte>, String> {
    let mut fontes = Vec::new();
    for m in Monitor::enumerate().map_err(|e| e.to_string())? {
        // BitBlt da área do monitor: sem borda e sem abrir captura.
        let gdi = retangulo_monitor(&m).and_then(|(x, y, w, h)| crate::gdi::area(x, y, w, h)).and_then(|i| png_de(i, (320, 180)));
        let resultado = match gdi { Some(p) => Ok(p), None => miniatura(m) };
        fontes.push(Fonte { id: format!("monitor:{}", m.as_raw_hmonitor() as usize), tipo: "monitor",
            nome: m.name().unwrap_or_else(|_| "Monitor".into()), pid: None,
            largura: m.width().unwrap_or(0), altura: m.height().unwrap_or(0),
            miniatura: resultado.as_ref().ok().cloned(), aviso: resultado.err(), icone: None, minimizada: false });
    }
    // Janelas em paralelo, com prazo: uma janela travada não segura a lista.
    let (tx, rx) = mpsc::channel();
    let mut pendentes = 0;
    for j in Window::enumerate().map_err(|e| e.to_string())? {
        let Ok(pid) = j.process_id() else { continue };
        if pid == std::process::id() { continue; }
        let Ok(nome) = j.title() else { continue };
        if nome.trim().is_empty() { continue; }
        let hwnd = j.as_raw_hwnd() as usize;
        if escondida(hwnd) { continue; }
        let (largura, altura) = (j.width().unwrap_or(0).max(0) as u32, j.height().unwrap_or(0).max(0) as u32);
        let tx = tx.clone();
        pendentes += 1;
        std::thread::spawn(move || {
            let (miniatura, icone, minimizada, aviso) = miniatura_janela(hwnd);
            let _ = tx.send((pendentes, Fonte { id: format!("janela:{hwnd}"), tipo: "janela", nome, pid: Some(pid),
                largura, altura, miniatura, aviso, icone, minimizada }));
        });
    }
    drop(tx);
    let prazo = std::time::Instant::now() + Duration::from_secs(3);
    let mut janelas = Vec::new();
    while janelas.len() < pendentes {
        let Ok(f) = rx.recv_timeout(prazo.saturating_duration_since(std::time::Instant::now())) else { break };
        janelas.push(f);
    }
    // Mesma ordem do Windows (de cima para baixo), como antes.
    janelas.sort_by_key(|(ordem, _)| *ordem);
    fontes.extend(janelas.into_iter().map(|(_, f)| f));
    Ok(fontes)
}

#[tauri::command]
pub async fn listar_fontes() -> Result<Vec<Fonte>, String> {
    tauri::async_runtime::spawn_blocking(fontes).await.map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct ProcessoAudio {
    pub pid: u32,
    pub nome: String,
    pub ativo: bool,
}

struct Com;
impl Drop for Com { fn drop(&mut self) { unsafe { CoUninitialize() } } }

pub fn processos_audio() -> Result<Vec<ProcessoAudio>, String> {
    unsafe { processos_audio_com().map_err(|e| format!("Não foi possível listar as sessões de áudio: {e}")) }
}

unsafe fn processos_audio_com() -> windows::core::Result<Vec<ProcessoAudio>> {
    CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    let _com = Com;
    let enumerador: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let dispositivos = enumerador.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
    let mut nomes = BTreeMap::new();
    let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?;
    let mut entrada = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut ok = Process32FirstW(snapshot, &mut entrada).is_ok();
    while ok {
        let fim = entrada.szExeFile.iter().position(|c| *c == 0).unwrap_or(entrada.szExeFile.len());
        nomes.insert(entrada.th32ProcessID, String::from_utf16_lossy(&entrada.szExeFile[..fim]));
        ok = Process32NextW(snapshot, &mut entrada).is_ok();
    }
    let _ = CloseHandle(snapshot);
    let mut processos = BTreeMap::new();
    for i in 0..dispositivos.GetCount()? {
        let dispositivo = dispositivos.Item(i)?;
        let gerenciador: IAudioSessionManager2 = dispositivo.Activate(CLSCTX_ALL, None)?;
        let sessoes = gerenciador.GetSessionEnumerator()?;
        for j in 0..sessoes.GetCount()? {
            let sessao = sessoes.GetSession(j)?;
            if sessao.GetState()? == AudioSessionStateExpired { continue; }
            let controle: IAudioSessionControl2 = sessao.cast()?;
            let Ok(pid) = controle.GetProcessId() else { continue };
            if pid == 0 || controle.IsSystemSoundsSession() == windows::core::HRESULT(0) { continue; }
            let ativo = sessao.GetState()? == AudioSessionStateActive;
            let processo = processos.entry(pid).or_insert_with(|| ProcessoAudio {
                pid, nome: nomes.get(&pid).cloned().unwrap_or_else(|| format!("Processo {pid}")), ativo: false });
            processo.ativo |= ativo;
        }
    }
    let mut lista: Vec<_> = processos.into_values().collect();
    lista.sort_by_key(|p| (p.nome.to_lowercase(), p.pid));
    Ok(lista)
}

#[tauri::command]
pub async fn listar_processos_audio() -> Result<Vec<ProcessoAudio>, String> {
    tauri::async_runtime::spawn_blocking(processos_audio).await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod testes {
    use super::*;
    use std::{path::PathBuf, process::{Child, Command}};

    struct Filho(Child);
    impl Drop for Filho {
        fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
    }

    // Requer desktop Windows desbloqueado e o gerador da F1.2 compilado.
    #[tokio::test]
    #[ignore = "Ensaio nativo com janela e áudio reais; executar manualmente"]
    async fn descoberta_nativa() {
        let raiz = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let janela = raiz.join("app/spikes/wgc/janela-teste.ps1");
        let _janela = Filho(Command::new("powershell").args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
            .arg(janela).spawn().unwrap());
        let tom = raiz.join("app/spikes/audio-processo/target/debug/tom.exe");
        let mut tom = Filho(Command::new(tom).args(["440", "40", "0.01"]).spawn().unwrap());
        std::thread::sleep(Duration::from_secs(3));
        let audio = listar_processos_audio().await.unwrap();
        assert!(audio.iter().any(|p| p.pid == tom.0.id() && p.ativo), "O tom precisa aparecer ativo pelo PID exato");
        assert!(audio.iter().all(|p| p.pid != 0));
        let inicio = std::time::Instant::now();
        let fontes = listar_fontes().await.unwrap();
        assert!(fontes.iter().any(|f| f.tipo == "monitor" && f.miniatura.is_some()));
        let janela = fontes.iter().find(|f| f.nome == "StreamStudio - teste WGC F1.1").expect("Janela de teste ausente");
        let png = STANDARD.decode(janela.miniatura.as_ref().expect("Miniatura da janela ausente").strip_prefix("data:image/png;base64,").unwrap()).unwrap();
        let imagem = image::load_from_memory(&png).unwrap();
        assert!(imagem.width() <= 320 && imagem.height() <= 180);
        assert!(imagem.width() > 0 && imagem.height() > 0);
        let saida = raiz.join("dev/estado/f23");
        std::fs::create_dir_all(&saida).unwrap();
        std::fs::write(saida.join("janela.png"), png).unwrap();
        let resumo = serde_json::json!({"fontes": fontes.len(), "miniaturas": fontes.iter().filter(|f| f.miniatura.is_some()).count(),
            "avisos": fontes.iter().filter(|f| f.aviso.is_some()).count(), "processos_audio": audio.len(),
            "sem_miniatura": fontes.iter().filter(|f| f.miniatura.is_none()).map(|f| serde_json::json!({"nome": f.nome.chars().take(30).collect::<String>(), "minimizada": f.minimizada, "icone": f.icone.is_some(), "aviso": f.aviso})).collect::<Vec<_>>(),
            "largura_miniatura": imagem.width(), "altura_miniatura": imagem.height(), "duracao_listagem_s": inicio.elapsed().as_secs_f64()});
        println!("{resumo}");
        std::fs::write(saida.join("resumo.json"), serde_json::to_vec_pretty(&resumo).unwrap()).unwrap();
        tom.0.kill().unwrap();
        tom.0.wait().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(!listar_processos_audio().await.unwrap().iter().any(|p| p.pid == tom.0.id() && p.ativo));
    }
}
/// Renova só a prévia escolhida, sem capturar miniaturas de todas as janelas.
#[tauri::command]
pub async fn miniatura_fonte(fonte: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(id) = fonte.strip_prefix("janela:").and_then(|s| s.parse::<usize>().ok()) {
            for janela in Window::enumerate().map_err(|e| e.to_string())? {
                if janela.as_raw_hwnd() as usize == id { return miniatura(janela); }
            }
        } else if let Some(id) = fonte.strip_prefix("monitor:").and_then(|s| s.parse::<usize>().ok()) {
            for monitor in Monitor::enumerate().map_err(|e| e.to_string())? {
                if monitor.as_raw_hmonitor() as usize == id { return miniatura(monitor); }
            }
        }
        Err("A fonte não está mais disponível.".into())
    }).await.map_err(|e| e.to_string())?
}
