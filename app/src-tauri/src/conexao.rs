//! F2.8: conexão com a sala, reconexão automática e estado para a interface.
//!
//! Quedas curtas o próprio SDK resolve (eventos `Reconnecting` → `Reconnected`). Se ele desiste
//! (`Disconnected`) ou passa de `DESISTIR_DO_SDK` tentando, o supervisor pede um token novo à API,
//! entra de novo e republica as faixas, usando as mesmas fontes de vídeo e som (a captura não para).
//! No ensaio, com o LiveKit fora do ar por 3 s, o SDK sozinho levou ~29 s para voltar.
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};

use livekit::prelude::*;
use serde::Serialize;
use tokio::sync::{Notify, watch};

pub struct Cancelamento(watch::Sender<bool>);
impl Default for Cancelamento {
    fn default() -> Self { Self(watch::channel(false).0) }
}
impl Cancelamento {
    pub fn cancelar(&self) { self.0.send_replace(true); }
    pub async fn cancelado(&self) {
        let mut rx = self.0.subscribe();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() { return; }
        }
    }
}
pub async fn ate_parar<T>(cancelamento: &Cancelamento, futuro: impl std::future::Future<Output = T>) -> Option<T> {
    tokio::select! { biased;
        _ = cancelamento.cancelado() => None,
        resultado = futuro => Some(resultado),
    }
}

/// Tempo que o SDK tem para se reconectar sozinho antes de o supervisor assumir.
pub const DESISTIR_DO_SDK: Duration = Duration::from_secs(10);
/// Espera máxima entre tentativas de entrar de novo.
const ESPERA_MAXIMA: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Conexao {
    Conectado,
    /// O SDK está tentando retomar sozinho.
    Reconectando,
    /// O supervisor está entrando de novo na sala.
    Caiu,
    /// Não dá para voltar (sala encerrada, chave inválida) ou a transmissão foi parada.
    Encerrada,
}

fn agora_ms() -> u64 {
    static INICIO: OnceLock<Instant> = OnceLock::new();
    INICIO.get_or_init(Instant::now).elapsed().as_millis() as u64
}

#[derive(Default)]
pub struct EstadoConexao {
    estado: AtomicU8,
    mudou_em_ms: AtomicU64,
    /// Sala atual. Eventos de uma sala antiga (já trocada) são ignorados.
    sala: AtomicU64,
    /// Quantas vezes o supervisor entrou de novo na sala.
    pub reentradas: AtomicU32,
    caiu: Notify,
    dados: std::sync::Mutex<Option<crate::api::Entrada>>,
    marca_envio: OnceLock<String>,
    recuperando: AtomicBool,
}

pub struct Recuperando(Arc<EstadoConexao>);
impl Drop for Recuperando { fn drop(&mut self) { self.0.recuperando.store(false, Ordering::SeqCst); } }

impl EstadoConexao {
    pub fn em_recuperacao(&self) -> bool { self.recuperando.load(Ordering::SeqCst) }
    pub fn reservar_recuperacao(self: &Arc<Self>) -> Option<Recuperando> {
        self.recuperando.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).ok().map(|_| Recuperando(self.clone()))
    }
    pub fn marca_envio(&self) -> &str {
        self.marca_envio.get_or_init(|| {
            static NUMERO: AtomicU64 = AtomicU64::new(1);
            format!("streamstudio-{}-{}-{}", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos(),
                NUMERO.fetch_add(1, Ordering::Relaxed))
        })
    }
    pub fn acesso(&self) -> Option<crate::api::Entrada> { self.dados.lock().unwrap().clone() }
    pub fn atualizar_acesso(&self, chave: String, sala: crate::api::Sala) {
        *self.dados.lock().unwrap() = Some(crate::api::Entrada { chave, sala });
    }
    pub fn atualizar_sala(&self, sala: crate::api::Sala) {
        if let Some(dados) = self.dados.lock().unwrap().as_mut() { dados.sala = sala; }
    }
    fn precisa_recuperar(&self) -> bool {
        self.atual() == Conexao::Caiu || (self.atual() == Conexao::Reconectando && self.ha_quanto() >= DESISTIR_DO_SDK)
    }
    pub fn atual(&self) -> Conexao {
        match self.estado.load(Ordering::Relaxed) {
            1 => Conexao::Reconectando,
            2 => Conexao::Caiu,
            3 => Conexao::Encerrada,
            _ => Conexao::Conectado,
        }
    }

    pub fn definir(&self, c: Conexao) {
        let v = match c {
            Conexao::Conectado => 0,
            Conexao::Reconectando => 1,
            Conexao::Caiu => 2,
            Conexao::Encerrada => 3,
        };
        if self.estado.swap(v, Ordering::Relaxed) != v {
            self.mudou_em_ms.store(agora_ms(), Ordering::Relaxed);
        }
    }

    /// Há quanto tempo está no estado atual.
    pub fn ha_quanto(&self) -> Duration {
        Duration::from_millis(agora_ms().saturating_sub(self.mudou_em_ms.load(Ordering::Relaxed)))
    }

    /// Acorda o supervisor para ele ver que a transmissão acabou e terminar.
    pub fn liberar_supervisor(&self) {
        self.caiu.notify_waiters();
    }
}

/// Entra na sala sem assinar nada: quem transmite não precisa baixar o vídeo dos outros.
/// Os eventos da sala atualizam `estado`; a saída pedida por nós (`close`) não conta como queda.
pub async fn conectar(url: &str, token: &str, estado: Arc<EstadoConexao>) -> Result<Room, String> {
    let mut opcoes = RoomOptions::default();
    opcoes.auto_subscribe = false;
    let (sala, mut eventos) = tokio::time::timeout(Duration::from_secs(15), Room::connect(url, token, opcoes))
        .await.map_err(|_| "A conexão com a sala demorou demais.".to_string())?
        .map_err(|e| format!("Não foi possível conectar na sala: {e}"))?;
    let minha = estado.sala.fetch_add(1, Ordering::Relaxed) + 1;
    estado.definir(Conexao::Conectado);
    tokio::spawn(async move {
        while let Some(ev) = eventos.recv().await {
            if estado.sala.load(Ordering::Relaxed) != minha {
                return;
            }
            match ev {
                RoomEvent::Reconnecting => estado.definir(Conexao::Reconectando),
                RoomEvent::Reconnected => estado.definir(Conexao::Conectado),
                RoomEvent::Disconnected { reason } => {
                    if matches!(reason, livekit::DisconnectReason::RoomDeleted | livekit::DisconnectReason::ParticipantRemoved) {
                        estado.definir(Conexao::Encerrada);
                        estado.caiu.notify_waiters();
                        return;
                    }
                    if !matches!(reason, livekit::DisconnectReason::ClientInitiated) {
                        eprintln!("conexão: a sala caiu ({reason:?})");
                        estado.definir(Conexao::Caiu);
                        estado.caiu.notify_one();
                    }
                    return;
                }
                _ => {}
            }
        }
    });
    Ok(sala)
}

/// Mutex da transmissão ativa, compartilhado entre os comandos e o supervisor.
pub type Ativa = Arc<tauri::async_runtime::Mutex<Option<crate::video::Transmissao>>>;

/// Dados para pedir token de novo.
pub struct Acesso {
    pub api: crate::api::ClienteApi,
    pub url: String,
    /// Chave mais recente: cada token novo traz uma chave nova (F2.2).
    pub chave: String,
    pub apelido: String,
}

/// Fica de olho na conexão e entra de novo quando ela cai. Termina quando a transmissão `id`
/// é parada ou trocada por outra, ou quando a API diz que a sala acabou / a chave não vale.
pub async fn supervisionar(ativa: Ativa, id: u64, estado: Arc<EstadoConexao>, mut acesso: Acesso) {
    let ainda_ativa = || async { ativa.lock().await.as_ref().map(|t| t.id) == Some(id) };
    loop {
        tokio::select! {
            _ = estado.caiu.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                let sdk_demorando = estado.atual() == Conexao::Reconectando && estado.ha_quanto() >= DESISTIR_DO_SDK;
                if !sdk_demorando {
                    if !ainda_ativa().await { return; }
                    continue;
                }
                eprintln!("conexão: o SDK está há {:?} tentando; entrando de novo", estado.ha_quanto());
            }
        }
        if !ainda_ativa().await || estado.atual() == Conexao::Encerrada {
            return;
        }
        if !estado.precisa_recuperar() { continue; }
        let Some(_reserva) = estado.reservar_recuperacao() else { continue };
        estado.definir(Conexao::Caiu);
        let mut espera = Duration::from_secs(1);
        loop {
            {
                if estado.atual() == Conexao::Encerrada { return; }
                let (recuperacao, cancelamento) = {
                    let g = ativa.lock().await;
                    let Some(t) = g.as_ref().filter(|t| t.id == id) else { return };
                    (t.preparar_reconexao(), t.cancelamento.clone())
                };
                if let Some(a) = estado.acesso() { acesso.chave = a.chave; }
                let Some(token) = ate_parar(&cancelamento, acesso.api.token(&acesso.chave, &acesso.apelido)).await else { return };
                match token {
                    Ok(novo) => {
                        acesso.chave = novo.chave;
                        estado.atualizar_acesso(acesso.chave.clone(), novo.sala);
                        let Some(resultado) = ate_parar(&cancelamento, recuperacao.executar(&acesso.url, &novo.token, estado.clone())).await else { return };
                        match resultado {
                            Ok(mut publicacao) => {
                                let mut g = ativa.lock().await;
                                let Some(t) = g.as_mut().filter(|t| t.id == id) else { return };
                                t.aplicar_reconexao(&mut publicacao);
                                estado.definir(Conexao::Conectado);
                                estado.reentradas.fetch_add(1, Ordering::Relaxed);
                                eprintln!("conexão: entrou de novo na sala");
                                break;
                            }
                            Err(e) => { if estado.atual() != Conexao::Encerrada { estado.definir(Conexao::Caiu); } eprintln!("conexão: falhou ao entrar de novo: {e}"); }
                        }
                    }
                    // 401/403/404: a sala acabou ou a chave não vale mais; insistir não adianta.
                    Err(e) if matches!(e.status(), Some(401 | 403 | 404)) => {
                        eprintln!("conexão: desistindo: {e}");
                        estado.definir(Conexao::Encerrada);
                        return;
                    }
                    Err(e) => eprintln!("conexão: sem token ainda: {e}"),
                }
            }
            tokio::time::sleep(espera).await;
            espera = (espera * 2).min(ESPERA_MAXIMA);
        }
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[tokio::test]
    async fn parada_nao_deixa_aviso_para_proxima_transmissao() {
        let estado=EstadoConexao::default();
        estado.liberar_supervisor();
        assert!(tokio::time::timeout(Duration::from_millis(20),estado.caiu.notified()).await.is_err());
        estado.caiu.notify_one();
        estado.caiu.notified().await;
        assert!(!estado.precisa_recuperar());
        estado.definir(Conexao::Caiu); assert!(estado.precisa_recuperar());
    }

    #[tokio::test]
    async fn parar_cancela_pedido_http_sem_resposta() {
        use tokio::io::AsyncReadExt;
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url=format!("http://{}",listener.local_addr().unwrap());
        let (pronto,recebeu)=tokio::sync::oneshot::channel();
        let servidor=tokio::spawn(async move {
            let (mut cliente,_)=listener.accept().await.unwrap();
            let mut buffer=[0u8;4096]; let _=cliente.read(&mut buffer).await;
            let _=pronto.send(()); std::future::pending::<()>().await;
        });
        let cancelamento=Arc::new(Cancelamento::default());let c=cancelamento.clone();
        let cliente=crate::api::ClienteApi::novo(&url).unwrap();
        let pedido=tokio::spawn(async move { ate_parar(&c,cliente.token("teste","pessoa")).await });
        recebeu.await.unwrap();cancelamento.cancelar();
        assert!(tokio::time::timeout(Duration::from_millis(200),pedido).await.unwrap().unwrap().is_none());
        servidor.abort();
    }

    #[test]
    fn atualizacao_de_sala_preserva_chave_renovada() {
        let e=EstadoConexao::default();
        let mut sala=crate::api::Sala{id:"abcdefgh".into(),codigo:"abcd-efgh".into(),nome:"Teste".into(),convite:"antigo".into(),ocioso:5};
        e.atualizar_acesso("chave-renovada".into(),sala.clone());
        sala.convite="novo".into();e.atualizar_sala(sala);
        let dados=e.acesso().unwrap();assert_eq!(dados.chave,"chave-renovada");assert_eq!(dados.sala.convite,"novo");
    }

    /// O site no ar usa wss://. Sem o recurso `native-tls` do SDK isso falhava com
    /// "TLS support not compiled in", e os ensaios locais (ws://) não pegavam.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "Cria e encerra uma sala de teste no site no ar"]
    async fn conecta_no_site_no_ar_por_wss() {
        let api = crate::api::ClienteApi::padrao();
        let criada = api.criar_sala("Teste TLS do app", "teste-tls-123456", 5).await.unwrap();
        let a = api.token(&criada.chave, "Teste TLS").await.unwrap();
        assert!(api.url_livekit().starts_with("wss://"), "{}", api.url_livekit());
        let estado = Arc::new(EstadoConexao::default());
        let resultado = conectar(&api.url_livekit(), &a.token, estado.clone()).await;
        let conectou = resultado.as_ref().map(|_| ()).map_err(|e| e.clone());
        if let Ok(sala) = resultado { let _ = sala.close().await; }
        let fim = reqwest::Client::new()
            .post(format!("{}/api/salas/{}/encerrar", api.servidor(), criada.sala.id))
            .json(&serde_json::json!({ "dono": criada.dono })).send().await.unwrap();
        assert!(fim.status().is_success(), "não encerrou a sala de teste: {}", fim.status());
        conectou.unwrap();
        assert_eq!(estado.atual(), Conexao::Conectado);
    }

    #[test]
    fn estados_ida_e_volta() {
        let e = EstadoConexao::default();
        assert_eq!(e.atual(), Conexao::Conectado);
        for c in [Conexao::Reconectando, Conexao::Caiu, Conexao::Encerrada, Conexao::Conectado] {
            e.definir(c);
            assert_eq!(e.atual(), c);
            assert!(e.ha_quanto() < Duration::from_secs(1));
        }
        assert_eq!(serde_json::to_string(&Conexao::Reconectando).unwrap(), r#""reconectando""#);
    }
}
