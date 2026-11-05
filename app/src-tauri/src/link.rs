//! Links `streamstudio://c/<convite>` e `streamstudio://s/<código>` (F4.1).
//!
//! O Windows abre o app com o link como único argumento. Se o app já estava aberto,
//! o plugin de instância única passa o link para a janela existente e o novo processo sai.
//! O link fica guardado aqui até a interface buscá-lo com `pegar_link`.
use std::sync::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_deep_link::DeepLinkExt;

pub const ESQUEMA: &str = "streamstudio";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "tipo", content = "id", rename_all = "snake_case")]
pub enum LinkSala {
    /// Convite: entra sem senha.
    Convite(String),
    /// Código da sala: ainda pede a senha.
    Codigo(String),
}

/// Link com o que veio no fragmento `#` (F4.6): nome, chave da sala e segredo de dono.
/// O fragmento nunca vai ao servidor; serve para trocar entre site e app sem conta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkRecebido {
    #[serde(flatten)]
    pub sala: LinkSala,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chave: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dono: Option<String>,
}

#[derive(Default)]
pub struct Estado(Mutex<Option<LinkRecebido>>);

/// Tamanho máximo do fragmento inteiro: chave (~120) + dono (32) + nome sobram com folga.
const FRAGMENTO_MAXIMO: usize = 1024;

/// Decodifica `%XX` e `+`; devolve `None` se não for UTF-8 válido.
fn decodificar(v: &str) -> Option<String> {
    let b = v.as_bytes();
    let mut saida = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                saida.push(u8::from_str_radix(h, 16).ok()?);
                i += 3;
            }
            b'+' => { saida.push(b' '); i += 1; }
            c => { saida.push(c); i += 1; }
        }
    }
    String::from_utf8(saida).ok()
}

/// Chave e segredo de dono são base64url (com `.` na chave assinada).
fn segredo_valido(v: &str, max: usize) -> bool {
    (16..=max).contains(&v.len()) && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Lê o link e, se houver, o fragmento. Partes inválidas do fragmento são ignoradas.
pub fn ler_link_completo(texto: &str) -> Option<LinkRecebido> {
    let sala = ler_link(texto)?;
    let mut link = LinkRecebido { sala, nome: None, chave: None, dono: None };
    let Some((_, fragmento)) = texto.trim().split_once('#') else { return Some(link) };
    if fragmento.len() > FRAGMENTO_MAXIMO { return Some(link); }
    for par in fragmento.split('&') {
        let Some((k, v)) = par.split_once('=') else { continue };
        let Some(v) = decodificar(v) else { continue };
        match k {
            "n" => {
                let nome = v.trim();
                if (1..=24).contains(&nome.chars().count()) && !nome.chars().any(char::is_control) {
                    link.nome = Some(nome.to_string());
                }
            }
            "k" if segredo_valido(&v, 512) => link.chave = Some(v),
            "d" if segredo_valido(&v, 128) => link.dono = Some(v),
            _ => {}
        }
    }
    // Segredo de dono sem chave não serve para nada aqui: a chave é o que deixa entrar.
    if link.chave.is_none() { link.dono = None; }
    Some(link)
}

/// Aceita só os dois formatos combinados; qualquer outra coisa é ignorada.
pub fn ler_link(texto: &str) -> Option<LinkSala> {
    let t = texto.trim();
    let prefixo = format!("{ESQUEMA}://");
    if t.len() < prefixo.len() || !t[..prefixo.len()].eq_ignore_ascii_case(&prefixo) {
        return None;
    }
    let resto = &t[prefixo.len()..];
    let resto = resto.split(['?', '#']).next().unwrap_or("").trim_end_matches('/');
    let (tipo, id) = resto.split_once('/')?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let id = id.to_ascii_lowercase();
    match tipo.to_ascii_lowercase().as_str() {
        "c" if (6..=16).contains(&id.len()) => Some(LinkSala::Convite(id)),
        "s" if id.len() == 8 => Some(LinkSala::Codigo(id)),
        _ => None,
    }
}

fn receber(app: &AppHandle, urls: impl IntoIterator<Item = String>) {
    let Some(link) = urls.into_iter().find_map(|u| ler_link_completo(&u)) else { return };
    *app.state::<Estado>().0.lock().unwrap() = Some(link);
    if let Some(janela) = app.get_webview_window("main") {
        let _ = janela.unminimize();
        let _ = janela.show();
        let _ = janela.set_focus();
    }
}

/// Registra o protocolo no Windows (só para o usuário atual) e começa a ouvir links.
pub fn configurar(app: &AppHandle) {
    // Aponta o protocolo para este executável. O instalador (F5.1) também registra;
    // fazer aqui garante que funcione mesmo copiando o .exe para outra pasta.
    // Build de desenvolvimento não registra: senão os links do site abririam a versão de teste
    // em vez do app instalado. Os ensaios passam o link direto como argumento.
    if !cfg!(debug_assertions) {
        if let Err(e) = app.deep_link().register_all() {
            eprintln!("Não foi possível registrar o protocolo {ESQUEMA}://: {e}");
        }
    }
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        receber(app, urls.into_iter().map(String::from));
    }
    let a = app.clone();
    app.deep_link().on_open_url(move |e| receber(&a, e.urls().into_iter().map(String::from)));
}

/// A interface chama isto ao abrir e a cada segundo: devolve o link recebido (uma vez só).
#[tauri::command]
pub fn pegar_link(estado: tauri::State<'_, Estado>) -> Option<LinkRecebido> {
    estado.0.lock().unwrap().take()
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn le_os_formatos_combinados() {
        assert_eq!(ler_link("streamstudio://c/AbC123xyz"), Some(LinkSala::Convite("abc123xyz".into())));
        assert_eq!(ler_link("StreamStudio://C/abc123/"), Some(LinkSala::Convite("abc123".into())));
        assert_eq!(ler_link("streamstudio://s/k7mqx3ta"), Some(LinkSala::Codigo("k7mqx3ta".into())));
        assert_eq!(ler_link("streamstudio://s/k7mqx3ta?x=1"), Some(LinkSala::Codigo("k7mqx3ta".into())));
    }

    #[test]
    fn le_fragmento_da_troca_site_app() {
        let chave = "eyJzIjoiazdtcXgzdGEiLCJ2IjoiMDAifQ.AbC-_123456789xyz";
        let l = ler_link_completo(&format!("streamstudio://s/k7mqx3ta#n=Jo%C3%A3o+Neto&k={chave}&d=segredo_de_dono-1234567")).unwrap();
        assert_eq!(l.sala, LinkSala::Codigo("k7mqx3ta".into()));
        assert_eq!(l.nome.as_deref(), Some("João Neto"));
        assert_eq!(l.chave.as_deref(), Some(chave));
        assert_eq!(l.dono.as_deref(), Some("segredo_de_dono-1234567"));
        let json = serde_json::to_value(&l).unwrap();
        assert_eq!(json["tipo"], "codigo");
        assert_eq!(json["id"], "k7mqx3ta");
        assert_eq!(json["nome"], "João Neto");
        // Sem fragmento, o JSON continua igual ao da F4.1.
        let simples = serde_json::to_value(ler_link_completo("streamstudio://c/abc123").unwrap()).unwrap();
        assert_eq!(simples, serde_json::json!({"tipo": "convite", "id": "abc123"}));
    }

    #[test]
    fn fragmento_invalido_e_ignorado() {
        let vazio = |t: &str| { let l = ler_link_completo(t).unwrap(); (l.nome, l.chave, l.dono) };
        let nada = (None, None, None);
        assert_eq!(vazio("streamstudio://c/abc123#n=&k=curta&d=x"), nada);
        assert_eq!(vazio("streamstudio://c/abc123#n=%ZZ&k=aaaa<script>aaaaaaaaaa"), nada);
        assert_eq!(vazio(&format!("streamstudio://c/abc123#n={}", "a".repeat(25))), nada);
        assert_eq!(vazio("streamstudio://c/abc123#n=a%0Ab"), nada);
        // Fragmento grande demais é descartado inteiro.
        assert_eq!(vazio(&format!("streamstudio://c/abc123#n=Neto&k={}", "a".repeat(2000))), nada);
        // Dono sem chave não vale.
        assert_eq!(vazio("streamstudio://c/abc123#d=segredo_de_dono-1234567"), nada);
        // Link ruim continua ruim, com ou sem fragmento.
        assert_eq!(ler_link_completo("streamstudio://c/abc#n=Neto"), None);
    }

    #[test]
    fn recusa_o_resto() {
        for ruim in ["", "https://stream.exemplo.com/stream/c/abc123", "streamstudio://c/abc", "streamstudio://s/abc",
            "streamstudio://s/k7mq-x3ta", "streamstudio://c/abc123/extra", "streamstudio://x/abc123",
            "streamstudio://c/abc12%22", "streamstudio://", "--flag"] {
            assert_eq!(ler_link(ruim), None, "{ruim}");
        }
    }
}
