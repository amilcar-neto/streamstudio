#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Cliente da API e ponte das telas aprovadas.
#[allow(dead_code)]
mod api;

mod atualizacao;
mod interface;
mod link;
mod audio;
mod conexao;
mod estatisticas;
mod fontes;
mod gdi;
mod video;
mod previa_memoria;

fn main() {
    tauri::Builder::default()
        // Precisa ser o primeiro plugin: um segundo app aberto por link passa o link para cá e sai.
        .plugin(tauri_plugin_single_instance::init(|_, _, _| {}))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(link::Estado::default())
        .setup(|app| { link::configurar(app.handle()); Ok(()) })
        .manage(video::Estado::default())
        .manage(interface::Estado::default())
        .invoke_handler(tauri::generate_handler![interface::conferir_servidor, interface::preparar_sala, interface::entrar_sala,
            interface::acesso_player, interface::estado_sala, interface::iniciar_envio, interface::parar_envio,
            interface::sair_sala, interface::conferir_sala, interface::abrir_navegador, link::pegar_link, fontes::miniatura_fonte, fontes::listar_fontes, fontes::listar_processos_audio,
            video::padroes_video, video::iniciar_transmissao, video::trocar_fonte, video::trocar_audio, video::estatisticas_transmissao, video::parar_transmissao, video::previa_envio, video::preparar_previa, video::manter_previa, video::ajustar_transmissao,
            atualizacao::verificar_atualizacao, atualizacao::instalar_atualizacao])
        .build(tauri::generate_context!())
        .expect("Não foi possível iniciar o StreamStudio")
        .run(|app, evento| {
            // Fechar o app despublica as faixas e sai da sala na hora, em vez de os outros
            // esperarem o servidor perceber a queda (F2.8).
            if let tauri::RunEvent::Exit = evento {
                use tauri::Manager;
                let ativa = app.state::<video::Estado>().0.clone();
                tauri::async_runtime::block_on(async move {
                    if let Some(t) = ativa.lock().await.take() {
                        t.parar().await;
                    }
                });
                tauri::async_runtime::block_on(async {
                    if let Some(s) = app.state::<interface::Estado>().0.lock().await.take() {
                        s.fechar().await;
                    }
                });
            }
        });
}
