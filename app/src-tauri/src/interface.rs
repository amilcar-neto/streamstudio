//! Ponte entre as telas e o núcleo. O player recebe um token separado do envio.
use std::sync::Arc;
use livekit::prelude::*;
use serde::Serialize;
use crate::{api, conexao, video, audio};

#[derive(Default)]
pub struct Estado(pub tauri::async_runtime::Mutex<Option<Sessao>>);
#[derive(Clone)]
pub struct Sessao {
    api: api::ClienteApi,
    chave: String,
    apelido: String,
    sala: api::Sala,
    sala_lk: Arc<Room>,
    conexao: Arc<conexao::EstadoConexao>,
    cancelamento: Arc<conexao::Cancelamento>,
    ultima_consulta: Arc<std::sync::Mutex<std::time::Instant>>,
}
#[derive(Serialize)]
pub struct Pessoa { nome: String, voce: bool }
#[derive(Serialize)]
pub struct PainelSala {
    marca_envio: String,
    identidade_envio: String,
    sala: api::Sala,
    pessoas: Vec<Pessoa>,
    conexao: conexao::Conexao,
    #[serde(skip_serializing_if = "Option::is_none")]
    dono: Option<String>,
}
fn painel(s: &Sessao, sala: &Room, conexao: conexao::Conexao) -> PainelSala {
    let mut pessoas = vec![Pessoa { nome: s.apelido.clone(), voce: true }];
    let mut outras: Vec<_> = sala.remote_participants().values().map(|p| Pessoa {
        nome: p.name(), voce: false,
    }).collect();
    outras.sort_by(|a, b| a.nome.cmp(&b.nome));
    pessoas.extend(outras);
    PainelSala { marca_envio: s.conexao.marca_envio().into(), identidade_envio: sala.local_participant().identity().to_string(), sala: s.conexao.acesso().map(|a| a.sala).unwrap_or_else(|| s.sala.clone()), pessoas, conexao, dono: None }
}
#[tauri::command]
pub async fn preparar_sala(servidor: String, nome: String, senha: String, ocioso: u32) -> Result<api::SalaCriada, String> {
    api::ClienteApi::novo(&servidor).map_err(|e| e.to_string())?.criar_sala(&nome, &senha, ocioso).await.map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn conferir_servidor(servidor: String) -> Result<String, String> {
    let api = api::ClienteApi::novo(&servidor).map_err(|e| e.to_string())?;
    api.saude().await.map_err(|e| e.to_string())?;
    Ok(api.servidor().into())
}
#[tauri::command]
pub async fn entrar_sala(
    estado: tauri::State<'_, Estado>, servidor: String, apelido: String,
    alvo: String, senha: String, criar: bool, ocioso: u32, chave: Option<String>,
) -> Result<PainelSala, String> {
    let mut g = estado.0.lock().await;
    if g.is_some() { return Err("Saia da sala atual antes de entrar em outra.".into()); }
    if apelido.trim().is_empty() { return Err("Digite seu apelido.".into()); }
    let api = api::ClienteApi::novo(&servidor).map_err(|e| e.to_string())?;
    let (sala, chave, dono) = if criar {
        let e = api.criar_sala(alvo.trim(), &senha, ocioso).await.map_err(|e| e.to_string())?;
        (e.sala, e.chave, Some(e.dono))
    } else if let Some(chave) = chave {
        let sala = api.sala(&chave).await.map_err(|e| e.to_string())?;
        (sala, chave, None)
    } else {
        let alvo = api::ler_alvo(&alvo).ok_or("Cole um convite ou digite o código ou nome da sala.")?;
        let e = api.entrar(&alvo, &senha).await.map_err(|e| e.to_string())?;
        (e.sala, e.chave, None)
    };
    let a = api.token(&chave, apelido.trim()).await.map_err(|e| e.to_string())?;
    let conexao = Arc::new(conexao::EstadoConexao::default());
    conexao.atualizar_acesso(a.chave.clone(), a.sala.clone());
    let sala_lk = Arc::new(conexao::conectar(&api.url_livekit(), &a.token, conexao.clone()).await?);
    let s = Sessao { api, chave: a.chave, apelido: apelido.trim().into(), sala, sala_lk, conexao,
        cancelamento: Arc::default(), ultima_consulta: Arc::new(std::sync::Mutex::new(std::time::Instant::now())) };
    let mut resposta = painel(&s, &s.sala_lk, s.conexao.atual());
    resposta.dono = dono;
    *g = Some(s);
    Ok(resposta)
}
#[tauri::command]
pub async fn estado_sala(estado: tauri::State<'_, Estado>, video: tauri::State<'_, video::Estado>, atualizar: Option<bool>) -> Result<PainelSala, String> {
    let s = estado.0.lock().await.as_ref().cloned().ok_or("Você ainda não entrou numa sala.")?;
    let (sala, enviando) = {
        let ativa = video.0.lock().await;
        (ativa.as_ref().map(|t| t.sala.clone()).unwrap_or_else(|| s.sala_lk.clone()), ativa.is_some())
    };
    if s.conexao.atual() == conexao::Conexao::Encerrada { return Ok(painel(&s, &sala, conexao::Conexao::Encerrada)); }
    let chave = s.conexao.acesso().map(|a| a.chave).unwrap_or_else(|| s.chave.clone());
    let consultar = {
        let mut ultima = s.ultima_consulta.lock().unwrap();
        if atualizar.unwrap_or(false) || ultima.elapsed().as_secs() >= 10 { *ultima = std::time::Instant::now(); true } else { false }
    };
    if consultar {
        if let Some(resposta) = conexao::ate_parar(&s.cancelamento, s.api.sala(&chave)).await {
            match resposta {
                Ok(dados) => s.conexao.atualizar_sala(dados),
                Err(e) if matches!(e.status(), Some(401 | 403 | 404)) => s.conexao.definir(conexao::Conexao::Encerrada),
                Err(e) if atualizar.unwrap_or(false) => return Err(e.to_string()),
                Err(_) => {},
            }
        }
    }
    if enviando { return Ok(painel(&s, &sala, s.conexao.atual())); }
    // Sem envio, o SDK tenta retomar primeiro; depois reentramos pela API.
    if !s.conexao.em_recuperacao() && (s.conexao.atual() == conexao::Conexao::Caiu
        || (s.conexao.atual() == conexao::Conexao::Reconectando && s.conexao.ha_quanto() >= conexao::DESISTIR_DO_SDK)) {
        let Some(_reserva) = s.conexao.reservar_recuperacao() else { return Ok(painel(&s,&s.sala_lk,s.conexao.atual())) };
        // Reserva o estado antes de liberar os locks. Iniciar envio aguarda esta recuperação.
        s.conexao.definir(conexao::Conexao::Reconectando);
        let Some(resultado) = conexao::ate_parar(&s.cancelamento, s.api.token(&chave, &s.apelido)).await else { return Err("Você saiu da sala.".into()) };
        let a = match resultado {
            Ok(a) => a,
            Err(e) => {
                s.conexao.definir(if matches!(e.status(), Some(401 | 403 | 404)) { conexao::Conexao::Encerrada } else { conexao::Conexao::Caiu });
                return Ok(painel(&s, &s.sala_lk, s.conexao.atual()));
            }
        };
        s.conexao.atualizar_acesso(a.chave, a.sala);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), s.sala_lk.close()).await;
        let Some(resultado) = conexao::ate_parar(&s.cancelamento, conexao::conectar(&s.api.url_livekit(), &a.token, s.conexao.clone())).await else { return Err("Você saiu da sala.".into()) };
        let nova = match resultado { Ok(sala) => Arc::new(sala), Err(e) => { s.conexao.definir(conexao::Conexao::Caiu); return Err(e); } };
        let mut g = estado.0.lock().await;
        if let Some(atual) = g.as_mut().filter(|atual| Arc::ptr_eq(&atual.cancelamento, &s.cancelamento)) {
            atual.sala_lk = nova;
            return Ok(painel(atual, &atual.sala_lk, atual.conexao.atual()));
        }
        drop(g);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), nova.close()).await;
        return Err("Você saiu da sala.".into());
    }
    Ok(painel(&s, &s.sala_lk, s.conexao.atual()))
}
#[tauri::command]
pub async fn iniciar_envio(
    estado: tauri::State<'_, Estado>, video: tauri::State<'_, video::Estado>,
    fonte: String, config: video::ConfigVideo, audio: audio::ModoAudio,
) -> Result<video::InfoTransmissao, String> {
    let mut g = estado.0.lock().await;
    let s = g.as_mut().ok_or("Entre numa sala primeiro.")?;
    if s.conexao.em_recuperacao() || s.conexao.atual() != conexao::Conexao::Conectado { return Err("Aguarde a conexão com a sala.".into()); }
    let mut ativa = video.0.lock().await;
    if ativa.is_some() { return Err("Pare o envio antes de mudar os ajustes de vídeo.".into()); }
    let t = match video::Transmissao::iniciar_com_sala("", "", &fonte, &config, audio,
        Some((s.sala_lk.clone(), s.conexao.clone()))).await {
        Ok(t) => t,
        Err(e) => {
            if s.sala_lk.connection_state() != ConnectionState::Connected {
                s.conexao.definir(conexao::Conexao::Encerrada);
            }
            return Err(e);
        }
    };
    let info = t.info.clone();
    tauri::async_runtime::spawn(conexao::supervisionar(video.0.clone(), t.id, t.conexao.clone(), conexao::Acesso {
        api: s.api.clone(), url: s.api.url_livekit(), chave: s.chave.clone(), apelido: s.apelido.clone(),
    }));
    *ativa = Some(t);
    Ok(info)
}
#[tauri::command]
pub async fn parar_envio(estado: tauri::State<'_, Estado>, video: tauri::State<'_, video::Estado>) -> Result<(), String> {
    let mut g = estado.0.lock().await;
    let mut ativa = video.0.lock().await;
    if let Some(t) = ativa.take() {
        let (sala, conexao) = t.parar_mantendo_sala().await;
        conexao.definir(if sala.connection_state() == ConnectionState::Connected {
            conexao::Conexao::Conectado
        } else { conexao::Conexao::Caiu });
        if let Some(s) = g.as_mut() { s.sala_lk = sala; s.conexao = conexao; }
        else { let _ = sala.close().await; }
    }
    Ok(())
}
#[tauri::command]
pub async fn sair_sala(estado: tauri::State<'_, Estado>, video: tauri::State<'_, video::Estado>) -> Result<(), String> {
    let mut g = estado.0.lock().await;
    if let Some(s) = g.as_ref() { s.cancelamento.cancelar(); }
    if let Some(t) = video.0.lock().await.take() { t.parar().await; }
    if let Some(s) = g.take() { s.fechar().await; }
    Ok(())
}

/// Codifica um valor para o fragmento do link (`%XX` em tudo que não for seguro).
fn codificar(v: &str) -> String {
    v.bytes().map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Endereço do site que abre a mesma sala com o mesmo nome (F4.6). Nome, chave e segredo de
/// dono vão no fragmento `#`, que o navegador nunca manda ao servidor; o site lê e apaga.
fn link_navegador(servidor: &str, sala: &api::Sala, apelido: &str, chave: &str, dono: Option<&str>) -> Result<String, String> {
    if sala.id.is_empty() || !sala.id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("Sala inválida.".into());
    }
    let mut url = format!("{servidor}/s/{}#n={}&k={}", sala.id, codificar(apelido), codificar(chave));
    if let Some(d) = dono.filter(|d| !d.is_empty()) { url.push_str(&format!("&d={}", codificar(d))); }
    Ok(url)
}

#[tauri::command]
pub async fn abrir_navegador(estado: tauri::State<'_, Estado>, dono: Option<String>) -> Result<(), String> {
    let s = estado.0.lock().await.as_ref().cloned().ok_or("Entre numa sala primeiro.")?;
    let acesso = s.conexao.acesso();
    let chave = acesso.as_ref().map(|a| a.chave.clone()).unwrap_or_else(|| s.chave.clone());
    let sala = acesso.map(|a| a.sala).unwrap_or_else(|| s.sala.clone());
    let url = link_navegador(s.api.servidor(), &sala, &s.apelido, &chave, dono.as_deref())?;
    // Ensaio automático (app/testar-troca.mjs): grava o endereço em vez de abrir o navegador.
    if let Ok(arquivo) = std::env::var("STREAMSTUDIO_ENSAIO_NAVEGADOR") {
        return std::fs::write(arquivo, url).map_err(|e| e.to_string());
    }
    std::process::Command::new("rundll32.exe").arg("url.dll,FileProtocolHandler").arg(url)
        .spawn().map_err(|e| format!("Não foi possível abrir o navegador: {e}"))?;
    Ok(())
}

impl Sessao { pub async fn fechar(self) { self.cancelamento.cancelar(); let _ = tokio::time::timeout(std::time::Duration::from_secs(2), self.sala_lk.close()).await; } }

/// A recepção WebRTC do WebView usa o player do site, com acesso separado do envio nativo.
#[tauri::command]
pub async fn acesso_player(estado: tauri::State<'_, Estado>) -> Result<serde_json::Value, String> {
    let s = estado.0.lock().await.as_ref().cloned().ok_or("Entre numa sala primeiro.")?;
    if s.conexao.atual() == conexao::Conexao::Encerrada { return Err("Essa sala foi encerrada.".into()); }
    let chave = s.conexao.acesso().map(|a| a.chave).unwrap_or_else(|| s.chave.clone());
    let nome = format!("{} (player)", s.apelido);
    let a = conexao::ate_parar(&s.cancelamento, s.api.token(&chave, &nome)).await.ok_or("Você saiu da sala.")?.map_err(|e| e.to_string())?;
    s.conexao.atualizar_acesso(a.chave.clone(), a.sala.clone());
    Ok(serde_json::json!({"token": a.token, "chave": a.chave, "sala": a.sala,
        "url": s.api.url_livekit(), "nome": nome}))
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn link_do_navegador_leva_nome_chave_e_dono_no_fragmento() {
        let sala = api::Sala { id: "k7mqx3ta".into(), codigo: "k7mq-x3ta".into(), nome: "Teste".into(), convite: "abc123".into(), ocioso: 5 };
        let url = link_navegador("https://stream.exemplo.com/stream", &sala, "João & Cia", "abc.DEF_-1", Some("dono_123")).unwrap();
        assert_eq!(url, "https://stream.exemplo.com/stream/s/k7mqx3ta#n=Jo%C3%A3o%20%26%20Cia&k=abc.DEF_-1&d=dono_123");
        let sem_dono = link_navegador("http://localhost:8080", &sala, "Neto", "chave", None).unwrap();
        assert!(!sem_dono.contains("&d="));
        // O caminho (que vai ao servidor) nunca leva segredo.
        assert!(!url.split('#').next().unwrap().contains("dono_123"));
    }
}
