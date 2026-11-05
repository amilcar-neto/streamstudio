//! Toca um seno em laço por N segundos usando PlaySound (winmm).
//! Uso: tom <hz> <segundos> [amplitude 0..1]
use std::{env, thread, time::Duration};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_LOOP, SND_MEMORY, SND_NODEFAULT};
use windows::core::PCWSTR;

fn wav_seno(hz: f64, amp: f64) -> Vec<u8> {
    let taxa = 48_000u32;
    let quadros = taxa; // 1 s, repetido em laço (hz inteiro fecha o ciclo)
    let mut dados = Vec::with_capacity(quadros as usize * 4);
    for i in 0..quadros {
        let v = (amp * (2.0 * std::f64::consts::PI * hz * i as f64 / taxa as f64).sin() * 32767.0) as i16;
        dados.extend_from_slice(&v.to_le_bytes());
        dados.extend_from_slice(&v.to_le_bytes());
    }
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + dados.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&taxa.to_le_bytes());
    w.extend_from_slice(&(taxa * 4).to_le_bytes());
    w.extend_from_slice(&4u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(dados.len() as u32).to_le_bytes());
    w.extend_from_slice(&dados);
    w
}

fn main() {
    let a: Vec<String> = env::args().collect();
    if a.len() < 3 {
        eprintln!("uso: tom <hz> <segundos> [amplitude]");
        std::process::exit(2);
    }
    let hz: f64 = a[1].parse().expect("hz inválido");
    let seg: u64 = a[2].parse().expect("segundos inválidos");
    let amp: f64 = a.get(3).map(|s| s.parse().expect("amplitude inválida")).unwrap_or(0.05);
    let wav = wav_seno(hz, amp);
    unsafe {
        let ok = PlaySoundW(PCWSTR(wav.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_LOOP | SND_NODEFAULT);
        if !ok.as_bool() {
            eprintln!("PlaySound falhou");
            std::process::exit(1);
        }
        thread::sleep(Duration::from_secs(seg));
        let _ = PlaySoundW(PCWSTR::null(), None, Default::default());
    }
}
