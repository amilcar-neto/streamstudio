//! Memória compartilhada com o frame principal, somente leitura no JavaScript.
//! A vista nativa tem handle duplicado próprio: não depende da vida do objeto COM
//! nem da vista do renderer. Todos os métodos COM rodam na thread do WebView.
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, AtomicU32, Ordering}};
use windows::{core::{w, Interface}, Win32::{Foundation::{CloseHandle, DuplicateHandle, HANDLE, DUPLICATE_SAME_ACCESS},
    System::{Memory::{MapViewOfFile, UnmapViewOfFile, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS}, Threading::GetCurrentProcess}}};
use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2Environment12, ICoreWebView2_17, COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY};

struct Vista { endereco: usize, handle: usize, proximo: usize }
impl Drop for Vista {
    fn drop(&mut self) { unsafe {
        let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.endereco as *mut _ });
        let _ = CloseHandle(HANDLE(self.handle as *mut _));
    } }
}

pub struct Memoria {
    vista: Mutex<Vista>, capacidade: usize, passo: usize,
    tamanho: AtomicU64, parada: AtomicBool,
    tarefa: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl Memoria {
    /// Duplica o handle emprestado e cria uma vista própria. Nenhum ponteiro do COM
    /// sai da thread da interface. MapViewOfFile/CloseHandle são independentes de COM.
    unsafe fn nova(emprestado: HANDLE, capacidade: usize) -> Result<Self, String> {
        let capacidade = (capacidade + 7) & !7;
        let passo = capacidade + 32;
        let total = 16 + 3 * passo;
        let mut handle = HANDLE::default();
        unsafe { DuplicateHandle(GetCurrentProcess(), emprestado, GetCurrentProcess(), &mut handle, 0, false, DUPLICATE_SAME_ACCESS) }.map_err(|e| e.to_string())?;
        let endereco = unsafe { MapViewOfFile(handle, FILE_MAP_WRITE, 0, 0, total) };
        if endereco.Value.is_null() {
            let erro = windows::core::Error::from_thread().to_string();
            let _ = unsafe { CloseHandle(handle) }; return Err(erro);
        }
        Ok(Self { vista: Mutex::new(Vista { endereco: endereco.Value as usize, handle: handle.0 as usize, proximo: 0 }), capacidade, passo,
            tamanho: AtomicU64::new(0), parada: AtomicBool::new(false), tarefa: Mutex::new(None) })
    }

    pub fn configurar(&self, largura: u32, altura: u32) {
        self.tamanho.store(((largura as u64)<<32)|altura as u64, Ordering::Relaxed);
        if let Some(t) = self.tarefa.lock().unwrap().as_ref() { t.thread().unpark(); }
    }
    pub fn tamanho(&self) -> (u32,u32) { let n=self.tamanho.load(Ordering::Relaxed); ((n>>32) as u32,n as u32) }
    pub fn parada(&self) -> bool { self.parada.load(Ordering::Relaxed) }
    pub fn registrar(&self, tarefa: std::thread::JoinHandle<()>) { *self.tarefa.lock().unwrap()=Some(tarefa); }
    pub fn encerrar(&self) {
        self.parada.store(true, Ordering::Relaxed);
        if let Some(t)=self.tarefa.lock().unwrap().take() {
            t.thread().unpark();
            if t.thread().id()!=std::thread::current().id() { let _=t.join(); }
        }
    }

    pub fn gravar(&self, largura: u32, altura: u32, instante: u64, planos: &[&[u8]]) -> Result<(), String> {
        let tamanho: usize = planos.iter().map(|p| p.len()).sum();
        if tamanho > self.capacidade { return Err("A prévia mudou de tamanho.".into()); }
        let mut vista = self.vista.lock().map_err(|_| "Memória da prévia indisponível.")?;
        let indice=vista.proximo; vista.proximo=(indice+1)%3;
        let base=16+indice*self.passo;
        // MapViewOfFile é alinhado em página; o cabeçalho e os passos são múltiplos
        // de oito. Zero marca escrita em andamento. O leitor valida a sequência
        // novamente depois do upload, antes de apresentar a imagem.
        unsafe {
            let p=vista.endereco as *mut u8;
            let seq=AtomicU64::from_ptr(p.add(base) as *mut u64);
            seq.store(0,Ordering::SeqCst);
            // Garante que o zero fique visível antes de qualquer pixel novo.
            std::sync::atomic::fence(Ordering::Release);
            std::ptr::write(p.add(base+8) as *mut u32,largura);
            std::ptr::write(p.add(base+12) as *mut u32,altura);
            let destino=std::slice::from_raw_parts_mut(p.add(base+32),tamanho);
            let mut i=0;
            for plano in planos { destino[i..i+plano.len()].copy_from_slice(plano); i+=plano.len(); }
            seq.store(instante,Ordering::Release);
            AtomicU32::from_ptr(p as *mut u32).store(indice as u32,Ordering::Release);
        }
        Ok(())
    }
}

pub async fn preparar(janela: tauri::WebviewWindow, capacidade: usize) -> Result<Arc<Memoria>, String> {
    if janela.label() != "main" { return Err("A prévia só pode ser aberta na janela principal.".into()); }
    let (envia, recebe) = tokio::sync::oneshot::channel();
    janela.with_webview(move |webview| {
        let resultado = (|| -> Result<Arc<Memoria>, String> { unsafe {
            let ambiente: ICoreWebView2Environment12 = webview.environment().cast().map_err(|e| e.to_string())?;
            let buffer = ambiente.CreateSharedBuffer((16+3*(32+((capacidade+7)&!7))) as u64).map_err(|e| e.to_string())?;
            let mut emprestado = HANDLE::default(); buffer.FileMappingHandle(&mut emprestado).map_err(|e| e.to_string())?;
            let memoria = Arc::new(Memoria::nova(emprestado, capacidade)?);
            let principal: ICoreWebView2_17 = webview.controller().CoreWebView2().map_err(|e| e.to_string())?.cast().map_err(|e| e.to_string())?;
            principal.PostSharedBufferToScript(&buffer, COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY, w!("{\"tipo\":\"previa-envio\",\"anel\":true}")).map_err(|e| e.to_string())?;
            // O renderer e a vista duplicada conservam referências próprias à memória.
            buffer.Close().map_err(|e| e.to_string())?;
            Ok(memoria)
        } })();
        let _ = envia.send(resultado);
    }).map_err(|e| e.to_string())?;
    recebe.await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod testes {
    use super::*;
    #[test]
    fn vista_independe_do_handle_original_e_respeita_limite() {
        use windows::{core::PCWSTR, Win32::{Foundation::INVALID_HANDLE_VALUE,
            System::Memory::{CreateFileMappingW, PAGE_READWRITE}}};
        let original = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, 16+3*(32+64), PCWSTR::null()) }.unwrap();
        let memoria = Arc::new(unsafe { Memoria::nova(original, 64) }.unwrap());
        unsafe { CloseHandle(original) }.unwrap();
        let escritor = memoria.clone();
        std::thread::spawn(move || escritor.gravar(2,2,42,&[&[1,2,3], &[4,5], &[6]])).join().unwrap().unwrap();
        {
            let vista = memoria.vista.lock().unwrap();
            assert_eq!(unsafe { std::slice::from_raw_parts((vista.endereco+16+32) as *const u8, 6) }, &[1,2,3,4,5,6]);
            assert_eq!(unsafe { AtomicU64::from_ptr((vista.endereco+16) as *mut u64).load(Ordering::Acquire) },42);
        }
        assert!(memoria.gravar(2,2,43,&[&[0;65]]).is_err());
        memoria.gravar(2,2,43,&[&[7;6]]).unwrap();
        memoria.gravar(2,2,44,&[&[8;6]]).unwrap();
        memoria.gravar(2,2,45,&[&[9;6]]).unwrap();
        {
            let vista=memoria.vista.lock().unwrap();
            assert_eq!(unsafe { AtomicU32::from_ptr(vista.endereco as *mut u32).load(Ordering::Acquire) },0);
            assert_eq!(unsafe { AtomicU64::from_ptr((vista.endereco+16) as *mut u64).load(Ordering::Acquire) },45);
        }
        let fim=Arc::new(AtomicBool::new(false)); let terminou=fim.clone(); let destino=memoria.clone();
        memoria.registrar(std::thread::spawn(move || {
            while !destino.parada() { std::thread::park_timeout(std::time::Duration::from_secs(1)); }
            terminou.store(true,Ordering::SeqCst);
        }));
        memoria.encerrar();
        assert!(fim.load(Ordering::SeqCst),"Encerrar deve aguardar a thread");
    }
}
