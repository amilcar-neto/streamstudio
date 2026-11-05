use std::{path::PathBuf, sync::{Arc, Mutex}, time::{Duration, Instant}};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    encoder::ImageFormat,
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::*,
    window::Window,
};

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Default)]
struct Stats {
    frames: u64,
    width: u32,
    height: u32,
    closed: bool,
}

struct Capture {
    stats: Arc<Mutex<Stats>>,
    png: PathBuf,
}

impl GraphicsCaptureApiHandler for Capture {
    type Flags = (Arc<Mutex<Stats>>, PathBuf);
    type Error = Error;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Error> {
        Ok(Self { stats: ctx.flags.0, png: ctx.flags.1 })
    }

    fn on_frame_arrived(&mut self, frame: &mut Frame, _: InternalCaptureControl) -> Result<(), Error> {
        let mut stats = self.stats.lock().unwrap();
        if stats.frames == 0 {
            frame.save_as_image(&self.png, ImageFormat::Png)?;
        }
        stats.frames += 1;
        stats.width = frame.width();
        stats.height = frame.height();
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Error> {
        self.stats.lock().unwrap().closed = true;
        Ok(())
    }
}

fn main() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--listar"] {
        for monitor in Monitor::enumerate()? {
            println!("Monitor {}: {} ({}x{}, {} Hz)", monitor.index()?, monitor.name().unwrap_or_default(), monitor.width()?, monitor.height()?, monitor.refresh_rate()?);
        }
        for window in Window::enumerate()? {
            println!("Janela PID {}: {}", window.process_id()?, window.title()?);
        }
        return Ok(());
    }
    if args.len() != 4 || args[0] != "--janela" || args[2] != "--saida" {
        return Err("Uso: spike-wgc --listar | --janela <trecho do título> --saida <diretório>".into());
    }
    let matches: Vec<_> = Window::enumerate()?.into_iter()
        .filter(|w| w.title().is_ok_and(|title| title.contains(&args[1])))
        .collect();
    if matches.len() != 1 {
        return Err(format!("Esperada uma janela, encontradas {}. Use um título mais específico e --listar.", matches.len()).into());
    }
    let window = matches[0];
    let title = window.title()?;
    let output = PathBuf::from(&args[3]);
    std::fs::create_dir_all(&output)?;
    let stats = Arc::new(Mutex::new(Stats::default()));
    let settings = Settings::new(window, CursorCaptureSettings::Default,
        DrawBorderSettings::Default, SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default, DirtyRegionSettings::Default,
        ColorFormat::Rgba8, (stats.clone(), output.join("frame.png")));
    let start = Instant::now();
    let control = Capture::start_free_threaded(settings)?;
    // O prazo independe da chegada de quadros: janelas estáticas também encerram.
    std::thread::sleep(Duration::from_secs(10));
    control.stop()?;
    let elapsed = start.elapsed().as_secs_f64();
    let stats = stats.lock().unwrap();
    if stats.frames == 0 || stats.closed {
        return Err(format!("Captura inválida: {} quadros, janela fechada: {}", stats.frames, stats.closed).into());
    }
    let result = format!("Janela: {title}\nDuração (s): {elapsed:.3}\nQuadros: {}\nResolução: {}x{}\nFPS médio (quadros/duração): {:.3}\n", stats.frames, stats.width, stats.height, stats.frames as f64 / elapsed);
    std::fs::write(output.join("metricas.txt"), &result)?;
    print!("{result}");
    Ok(())
}
