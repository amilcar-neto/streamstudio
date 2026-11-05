//! F5.2: atualização automática.
//!
//! O app confere `baixar/atualizacao.json` no site (endereço em `tauri.conf.json`). Se houver
//! versão nova, a interface mostra o botão "Atualizar"; o download só é aceito com a assinatura
//! certa (chave pública no `tauri.conf.json`, privada em `segredos/`, fora do git).
//! Nunca instala no meio de uma transmissão.
use serde::Serialize;
use tauri_plugin_updater::UpdaterExt;

#[derive(Debug, Serialize)]
pub struct Novidade {
    pub versao: String,
    pub notas: Option<String>,
}

#[tauri::command]
pub async fn verificar_atualizacao(app: tauri::AppHandle) -> Result<Option<Novidade>, String> {
    let atualizacao = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    Ok(atualizacao.map(|a| Novidade { versao: a.version.clone(), notas: a.body.clone() }))
}

/// Baixa, confere a assinatura e roda o instalador. No Windows o instalador fecha o app e
/// abre a versão nova sozinho.
#[tauri::command]
pub async fn instalar_atualizacao(
    app: tauri::AppHandle,
    video: tauri::State<'_, crate::video::Estado>,
    sessao: tauri::State<'_, crate::interface::Estado>,
) -> Result<(), String> {
    if video.0.lock().await.is_some() {
        return Err("Pare a transmissão antes de atualizar.".into());
    }
    let atualizacao = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?
        .ok_or("Você já está na versão mais nova.")?;
    let bytes = atualizacao.download(|_, _| {}, || {}).await
        .map_err(|e| format!("Não foi possível baixar a atualização: {e}"))?;
    // Sai da sala antes: o instalador fecha o app sem passar pelo encerramento normal.
    if let Some(s) = sessao.0.lock().await.take() { s.fechar().await; }
    atualizacao.install(bytes).map_err(|e| format!("Não foi possível instalar a atualização: {e}"))?;
    app.restart();
}

#[cfg(test)]
mod testes {
    use super::*;
    use std::io::{Read, Write};

    /// Ensaio com o instalador de verdade: `ENSAIO_SETUP` e `ENSAIO_SETUP.sig` saem do
    /// `compilar.ps1 -Instalador`; a chave pública vem do `tauri.conf.json`.
    /// Confere que a versão nova é achada, que o download com a assinatura certa passa
    /// e que um instalador adulterado é recusado.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "Precisa do instalador assinado (ENSAIO_SETUP)"]
    async fn baixa_so_com_assinatura_valida() {
        let setup = std::path::PathBuf::from(std::env::var("ENSAIO_SETUP").expect("ENSAIO_SETUP"));
        let original = std::fs::read(&setup).unwrap();
        let assinatura = std::fs::read_to_string(setup.with_extension("exe.sig")).unwrap();
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let chave = conf["plugins"]["updater"]["pubkey"].as_str().unwrap().to_string();

        // Servidor mínimo: /atualizacao.json, /setup.exe (original) e /adulterado.exe.
        let servidor = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", servidor.local_addr().unwrap());
        let mut adulterado = original.clone();
        let meio = adulterado.len() / 2;
        adulterado[meio] ^= 0xff;
        // A assinatura também carrega a versão: o manifesto tem que anunciar a mesma.
        let versao = conf["version"].as_str().unwrap().to_string();
        let manifesto = |arquivo: &str| serde_json::json!({
            "version": versao, "notes": "Ensaio", "pub_date": "2026-11-04T00:00:00Z",
            "platforms": { "windows-x86_64": { "signature": assinatura.trim(), "url": format!("{base}/{arquivo}") } }
        }).to_string().into_bytes();
        let (m_ok, m_ruim) = (manifesto("setup.exe"), manifesto("adulterado.exe"));
        std::thread::spawn(move || {
            for conexao in servidor.incoming() {
                let Ok(mut c) = conexao else { continue };
                let mut pedido = [0u8; 2048];
                let n = c.read(&mut pedido).unwrap_or(0);
                let linha = String::from_utf8_lossy(&pedido[..n]).lines().next().unwrap_or("").to_string();
                let corpo: &[u8] = if linha.contains("/ok.json") { &m_ok } else if linha.contains("/ruim.json") { &m_ruim }
                    else if linha.contains("/setup.exe") { &original } else if linha.contains("/adulterado.exe") { &adulterado } else { b"" };
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", corpo.len());
                let _ = c.write_all(corpo);
            }
        });

        let mut contexto = tauri::test::mock_context(tauri::test::noop_assets());
        contexto.config_mut().plugins.0.insert("updater".into(), conf["plugins"]["updater"].clone());
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().pubkey(chave).build())
            .build(contexto)
            .unwrap();
        let checar = |rota: &str| app.updater_builder().endpoints(vec![format!("{base}/{rota}").parse().unwrap()]).unwrap().build().unwrap();

        // O app simulado do Tauri é 0.1.0, então a versão do instalador é "nova".
        let boa = checar("ok.json").check().await.unwrap().expect("devia achar a versão nova");
        assert_eq!(boa.version, versao);
        let bytes = boa.download(|_, _| {}, || {}).await.expect("assinatura válida devia passar");
        assert_eq!(bytes.len(), std::fs::metadata(&setup).unwrap().len() as usize);

        let ruim = checar("ruim.json").check().await.unwrap().unwrap();
        assert!(ruim.download(|_, _| {}, || {}).await.is_err(), "instalador adulterado não pode passar");
    }
}
