//! Cópias de janela, tela e ícone pelo GDI do Windows (P2/P3).
//!
//! Diferente do WGC, não desenha a borda amarela e responde na hora. `PrintWindow` com
//! `PW_RENDERFULLCONTENT` pede ao DWM o conteúdo da janela, mesmo coberta por outras.
//! Não funciona com janela minimizada (o Windows não desenha) e alguns jogos devolvem preto.
use windows::Win32::{
    Foundation::{HWND, LPARAM, RECT, WPARAM},
    Graphics::Gdi::*,
    Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS},
    UI::WindowsAndMessaging::*,
};

/// Imagem RGBA (A = 255).
pub struct Imagem {
    pub largura: u32,
    pub altura: u32,
    pub rgba: Vec<u8>,
}

impl Imagem {
    /// Quase tudo preto: a cópia falhou (comum em jogos com DirectX).
    pub fn preta(&self) -> bool {
        let passo = (self.rgba.len() / 4 / 4096).max(1);
        !self.rgba.chunks_exact(4).step_by(passo).any(|p| p[0] > 8 || p[1] > 8 || p[2] > 8)
    }
}

const PW_RENDERFULLCONTENT: u32 = 2;

/// DC de memória com um bitmap 32 bits de cima para baixo; libera tudo no `Drop`.
struct Tela {
    dc: HDC,
    bitmap: HBITMAP,
    antigo: HGDIOBJ,
    bits: *mut u8,
    largura: u32,
    altura: u32,
}

impl Tela {
    unsafe fn nova(largura: u32, altura: u32) -> Option<Self> {
        if largura == 0 || altura == 0 || largura > 16384 || altura > 16384 { return None; }
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() { return None; }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: largura as i32,
                biHeight: -(altura as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(dc);
            return None;
        };
        let antigo = SelectObject(dc, bitmap.into());
        Some(Self { dc, bitmap, antigo, bits: bits as *mut u8, largura, altura })
    }

    /// Copia os pixels BGRA para RGBA opaco.
    unsafe fn imagem(&self) -> Imagem {
        let _ = GdiFlush();
        let n = (self.largura * self.altura * 4) as usize;
        let bgra = std::slice::from_raw_parts(self.bits, n);
        let mut rgba = Vec::with_capacity(n);
        for p in bgra.chunks_exact(4) {
            rgba.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
        Imagem { largura: self.largura, altura: self.altura, rgba }
    }
}

impl Drop for Tela {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.antigo);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Retângulo da janela na tela.
pub fn retangulo(hwnd: usize) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(HWND(hwnd as *mut _), &mut r).ok()? };
    Some(r)
}

/// Cursor do mouse já com alfa, guardado pelo handle (desenhar custa pouco, mas não a cada quadro).
pub struct Cursor { atual: isize, imagem: Option<(Imagem, i32, i32)> }

impl Cursor {
    pub fn novo() -> Self { Self { atual: 0, imagem: None } }

    /// Desenha o cursor (se visível) no quadro BGRA cuja origem na tela é (`x0`, `y0`).
    pub fn desenhar(&mut self, dados: &mut [u8], passo: usize, largura: u32, altura: u32, x0: i32, y0: i32) {
        unsafe {
            let mut info = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
            if GetCursorInfo(&mut info).is_err() || info.flags.0 & CURSOR_SHOWING.0 == 0 || info.hCursor.is_invalid() { return; }
            if info.hCursor.0 as isize != self.atual {
                self.atual = info.hCursor.0 as isize;
                let mut icone = ICONINFO::default();
                let ponto = if GetIconInfo(HICON(info.hCursor.0), &mut icone).is_ok() {
                    if !icone.hbmMask.is_invalid() { let _ = DeleteObject(icone.hbmMask.into()); }
                    if !icone.hbmColor.is_invalid() { let _ = DeleteObject(icone.hbmColor.into()); }
                    (icone.xHotspot as i32, icone.yHotspot as i32)
                } else { (0, 0) };
                let tamanho = GetSystemMetrics(SM_CXCURSOR).max(32) as u32;
                self.imagem = desenhar_icone(HICON(info.hCursor.0), tamanho).map(|i| (i, ponto.0, ponto.1));
            }
            let Some((img, hx, hy)) = &self.imagem else { return };
            let (cx, cy) = (info.ptScreenPos.x - hx - x0, info.ptScreenPos.y - hy - y0);
            for l in 0..img.altura as i32 {
                let y = cy + l;
                if y < 0 || y >= altura as i32 { continue; }
                for c in 0..img.largura as i32 {
                    let x = cx + c;
                    if x < 0 || x >= largura as i32 { continue; }
                    let o = ((l as u32 * img.largura + c as u32) * 4) as usize;
                    let alfa = img.rgba[o + 3] as u32;
                    if alfa == 0 { continue; }
                    let d = y as usize * passo + x as usize * 4;
                    for (k, canal) in [2usize, 1, 0].into_iter().enumerate() {
                        let fonte = img.rgba[o + canal] as u32;
                        dados[d + k] = ((fonte * alfa + dados[d + k] as u32 * (255 - alfa)) / 255) as u8;
                    }
                }
            }
        }
    }
}

/// Conteúdo da janela, sem borda amarela. `None` se minimizada, travada ou sem tamanho.
pub fn janela(hwnd: usize) -> Option<Imagem> {
    unsafe {
        let h = HWND(hwnd as *mut _);
        if IsIconic(h).as_bool() || IsHungAppWindow(h).as_bool() { return None; }
        let mut r = RECT::default();
        GetWindowRect(h, &mut r).ok()?;
        let tela = Tela::nova((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)?;
        if !PrintWindow(h, tela.dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool() { return None; }
        Some(tela.imagem())
    }
}

/// Retângulo da tela (coordenadas da área de trabalho virtual), pelo BitBlt: sem borda.
pub fn area(x: i32, y: i32, largura: u32, altura: u32) -> Option<Imagem> {
    unsafe {
        let tela = Tela::nova(largura, altura)?;
        let origem = GetDC(None);
        let ok = BitBlt(tela.dc, 0, 0, largura as i32, altura as i32, Some(origem), x, y, SRCCOPY | CAPTUREBLT).is_ok();
        ReleaseDC(None, origem);
        ok.then(|| tela.imagem())
    }
}

/// Ícone da janela no tamanho pedido (o da barra de tarefas). Pergunta com prazo curto
/// para não travar em janela que não responde.
pub fn icone(hwnd: usize, tamanho: u32) -> Option<Imagem> {
    unsafe {
        let h = HWND(hwnd as *mut _);
        let mut icone = HICON::default();
        for tipo in [ICON_BIG, ICON_SMALL2, ICON_SMALL] {
            let mut resultado = 0usize;
            let ok = SendMessageTimeoutW(h, WM_GETICON, WPARAM(tipo as usize), LPARAM(0), SMTO_ABORTIFHUNG, 100, Some(&mut resultado));
            if ok.0 != 0 && resultado != 0 { icone = HICON(resultado as *mut _); break; }
        }
        if icone.is_invalid() {
            for indice in [GCLP_HICON, GCLP_HICONSM] {
                let v = GetClassLongPtrW(h, indice);
                if v != 0 { icone = HICON(v as *mut _); break; }
            }
        }
        if icone.is_invalid() { return None; }
        desenhar_icone(icone, tamanho)
    }
}

/// Desenha um ícone/cursor com transparência: em fundo preto e branco, e calcula o alfa.
unsafe fn desenhar_icone(icone: HICON, tamanho: u32) -> Option<Imagem> {
    {
        let tela = Tela::nova(tamanho, tamanho)?;
        // Fundo transparente: desenha duas vezes (em preto e em branco) e calcula o alfa.
        let desenhar = |fundo: u8| -> Option<Vec<u8>> {
            std::ptr::write_bytes(tela.bits, fundo, (tamanho * tamanho * 4) as usize);
            DrawIconEx(tela.dc, 0, 0, icone, tamanho as i32, tamanho as i32, 0, None, DI_NORMAL).ok()?;
            let _ = GdiFlush();
            Some(std::slice::from_raw_parts(tela.bits, (tamanho * tamanho * 4) as usize).to_vec())
        };
        let (preto, branco) = (desenhar(0)?, desenhar(255)?);
        let mut rgba = Vec::with_capacity(preto.len());
        for (p, b) in preto.chunks_exact(4).zip(branco.chunks_exact(4)) {
            let alfa = 255 - (b[1] as i32 - p[1] as i32).clamp(0, 255);
            let cor = |c: u8| if alfa == 0 { 0 } else { ((c as i32 * 255) / alfa).min(255) as u8 };
            rgba.extend_from_slice(&[cor(p[2]), cor(p[1]), cor(p[0]), alfa as u8]);
        }
        Some(Imagem { largura: tamanho, altura: tamanho, rgba })
    }
}
