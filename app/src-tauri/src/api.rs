//! Cliente da API de salas do StreamStudio (a mesma que o site usa).
//!
//! Fluxo: `entrar_por_codigo`/`entrar_por_nome`/`entrar_por_convite` devolvem uma
//! `Entrada` com a chave da sala. Com a chave, `token` pede o token do LiveKit.
//! Cada resposta de `token` traz uma chave nova: guarde-a e use-a para renovar.
//!
//! Rotas em `api/token_server.py` (cabeçalho do arquivo).

use std::fmt;
use std::time::Duration;

use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Servidor usado quando a pessoa não escolhe outro.
pub const SERVIDOR_PADRAO: &str = "https://stream.exemplo.com/stream";

/// Dados públicos da sala (o que qualquer pessoa dentro dela pode saber).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sala {
    pub id: String,
    /// Código formatado, ex.: `abcd-efgh`.
    pub codigo: String,
    pub nome: String,
    pub convite: String,
    /// Minutos sem ninguém transmitindo até a sala fechar.
    pub ocioso: u32,
}

/// Resultado de entrar numa sala: os dados dela e a chave de acesso.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entrada {
    #[serde(flatten)]
    pub sala: Sala,
    pub chave: String,
}

/// Resultado de criar uma sala. `dono` permite encerrar e mudar ajustes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SalaCriada {
    #[serde(flatten)]
    pub sala: Sala,
    pub chave: String,
    pub dono: String,
}

/// Token do LiveKit pronto para conectar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acesso {
    pub token: String,
    pub sala: Sala,
    /// Chave renovada: substitui a anterior.
    pub chave: String,
}

/// O que a pessoa digitou ou colou para achar a sala.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alvo {
    Convite(String),
    Codigo(String),
    Nome(String),
}

/// Igual ao `lerAlvo` do site (`public/app.js`): entende link de convite,
/// endereço de sala (`/s/<código>`), código (`abcd-efgh`) ou nome.
/// Também aceita `streamstudio://c/<convite>` e `streamstudio://s/<código>`.
pub fn ler_alvo(texto: &str) -> Option<Alvo> {
    let t = texto.trim();
    if t.is_empty() {
        return None;
    }
    let depois = |marca: &str| -> Option<String> {
        let resto = &t[t.to_ascii_lowercase().find(marca)? + marca.len()..];
        let id: String = resto.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        Some(id.to_ascii_lowercase())
    };
    // "/c/" também casa com "streamstudio://c/"
    if let Some(id) = depois("/c/").filter(|id| (6..=16).contains(&id.len())) {
        return Some(Alvo::Convite(id));
    }
    if let Some(id) = depois("/s/").filter(|id| id.len() == 8) {
        return Some(Alvo::Codigo(id));
    }
    let sem_sep: String = t.chars().filter(|c| *c != '-' && !c.is_whitespace()).collect();
    let separadores = t.len() - sem_sep.len();
    if sem_sep.len() == 8
        && separadores <= 1
        && (separadores == 0 || t.chars().nth(4).is_some_and(|c| c == '-' || c.is_whitespace()))
        && sem_sep.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Some(Alvo::Codigo(sem_sep.to_ascii_lowercase()));
    }
    Some(Alvo::Nome(t.to_string()))
}

#[derive(Debug)]
pub enum ErroApi {
    /// O endereço do servidor está errado.
    ServidorInvalido(String),
    /// Não chegou resposta (sem internet, servidor fora, tempo esgotado).
    SemConexao,
    /// O servidor respondeu com erro; `mensagem` vem dele, em português.
    Servidor { status: u16, mensagem: String },
    /// Resposta que não entendemos.
    RespostaInvalida,
}

impl ErroApi {
    /// Código HTTP, se o servidor respondeu. 404 = sala não existe mais (apague a chave).
    pub fn status(&self) -> Option<u16> {
        match self {
            ErroApi::Servidor { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl fmt::Display for ErroApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErroApi::ServidorInvalido(s) => write!(f, "Endereço do servidor inválido: {s}"),
            ErroApi::SemConexao => write!(f, "Sem conexão com o servidor. Confira sua internet."),
            ErroApi::Servidor { mensagem, .. } => write!(f, "{mensagem}"),
            ErroApi::RespostaInvalida => {
                write!(f, "O servidor não respondeu. Tente de novo em instantes.")
            }
        }
    }
}

impl std::error::Error for ErroApi {}

// Para os comandos do Tauri: a interface recebe só o texto.
impl Serialize for ErroApi {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct ClienteApi {
    /// Sempre termina com `/`, para `join` não comer o último trecho do caminho.
    base: Url,
    http: reqwest::Client,
}

impl ClienteApi {
    /// `servidor` é o endereço do site, ex.: `https://stream.exemplo.com/stream`.
    pub fn novo(servidor: &str) -> Result<Self, ErroApi> {
        let invalido = || ErroApi::ServidorInvalido(servidor.to_string());
        let mut texto = servidor.trim().to_string();
        if !texto.ends_with('/') {
            texto.push('/');
        }
        let base = Url::parse(&texto).map_err(|_| invalido())?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(invalido());
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(8))
            .user_agent(concat!("StreamStudio/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| invalido())?;
        Ok(Self { base, http })
    }

    pub fn padrao() -> Self {
        Self::novo(SERVIDOR_PADRAO).expect("SERVIDOR_PADRAO é válido")
    }

    pub fn servidor(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    /// Endereço do LiveKit para o SDK, igual ao `URL_LIVEKIT` do site:
    /// `https://h/stream` → `wss://h/stream`, `http://…` → `ws://…`.
    pub fn url_livekit(&self) -> String {
        let mut u = self.base.clone();
        let esquema = if u.scheme() == "https" { "wss" } else { "ws" };
        u.set_scheme(esquema).expect("http(s) → ws(s) sempre vale");
        u.as_str().trim_end_matches('/').to_string()
    }

    async fn pedir<T: for<'de> Deserialize<'de>>(
        &self,
        caminho: &str,
        corpo: Option<Value>,
    ) -> Result<T, ErroApi> {
        let url = self
            .base
            .join(&format!("api/{caminho}"))
            .map_err(|_| ErroApi::ServidorInvalido(self.servidor().to_string()))?;
        let pedido = match corpo {
            Some(c) => self.http.post(url).json(&c),
            None => self.http.get(url),
        };
        let resposta = pedido.send().await.map_err(|_| ErroApi::SemConexao)?;
        let status = resposta.status();
        let bytes = resposta.bytes().await.map_err(|_| ErroApi::SemConexao)?;
        if status != StatusCode::OK {
            let mensagem = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| v.get("erro")?.as_str().map(str::to_string))
                .unwrap_or_else(|| ErroApi::RespostaInvalida.to_string());
            return Err(ErroApi::Servidor { status: status.as_u16(), mensagem });
        }
        serde_json::from_slice(&bytes).map_err(|_| ErroApi::RespostaInvalida)
    }

    /// Confere se o servidor está no ar.
    pub async fn saude(&self) -> Result<(), ErroApi> {
        let _: Value = self.pedir("health", None).await?;
        Ok(())
    }

    /// Código da sala (com ou sem o hífen) + senha.
    pub async fn entrar_por_codigo(&self, codigo: &str, senha: &str) -> Result<Entrada, ErroApi> {
        self.pedir("entrar", Some(json!({ "codigo": codigo, "senha": senha }))).await
    }

    /// Nome da sala + senha (nomes são únicos entre as salas abertas).
    pub async fn entrar_por_nome(&self, nome: &str, senha: &str) -> Result<Entrada, ErroApi> {
        self.pedir("entrar", Some(json!({ "nome": nome, "senha": senha }))).await
    }

    /// Convite não pede senha. Aceita o código do convite ou o link inteiro.
    pub async fn entrar_por_convite(&self, convite: &str) -> Result<Entrada, ErroApi> {
        let id = match ler_alvo(convite) {
            Some(Alvo::Convite(id)) => id,
            _ => convite.trim().to_ascii_lowercase(),
        };
        let id: String = id.chars().filter(char::is_ascii_alphanumeric).collect();
        if id.is_empty() {
            return Err(ErroApi::Servidor {
                status: 404,
                mensagem: "Esse convite não vale mais. A sala fechou ou o link foi trocado.".into(),
            });
        }
        self.pedir(&format!("convite/{id}"), Some(json!({}))).await
    }

    /// Entra pelo que a pessoa digitou. `senha` é ignorada para convites.
    pub async fn entrar(&self, alvo: &Alvo, senha: &str) -> Result<Entrada, ErroApi> {
        match alvo {
            Alvo::Convite(c) => self.entrar_por_convite(c).await,
            Alvo::Codigo(c) => self.entrar_por_codigo(c, senha).await,
            Alvo::Nome(n) => self.entrar_por_nome(n, senha).await,
        }
    }

    /// Token do LiveKit (vale 12 h). Também serve para renovar: chame de novo com a
    /// chave mais recente (`Acesso::chave`), que vale 24 h a partir de cada pedido.
    pub async fn token(&self, chave: &str, apelido: &str) -> Result<Acesso, ErroApi> {
        self.pedir("token", Some(json!({ "chave": chave, "nome": apelido }))).await
    }

    /// Dados atuais da sala (por exemplo, depois que o dono trocou o convite).
    pub async fn sala(&self, chave: &str) -> Result<Sala, ErroApi> {
        self.pedir("sala", Some(json!({ "chave": chave }))).await
    }

    /// Cria uma sala pelo mesmo formulário oferecido no site.
    pub async fn criar_sala(&self, nome: &str, senha: &str, ocioso: u32) -> Result<SalaCriada, ErroApi> {
        self.pedir("salas", Some(json!({ "nome": nome, "senha": senha, "ocioso": ocioso }))).await
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn le_alvo_como_o_site() {
        let c = |s: &str| Some(Alvo::Convite(s.into()));
        let s = |s: &str| Some(Alvo::Codigo(s.into()));
        assert_eq!(ler_alvo("https://stream.exemplo.com/stream/c/AbCdEf2345"), c("abcdef2345"));
        assert_eq!(ler_alvo("  http://localhost:8080/c/abcdef2345?x=1 "), c("abcdef2345"));
        assert_eq!(ler_alvo("streamstudio://c/abcdef2345"), c("abcdef2345"));
        assert_eq!(ler_alvo("https://h/stream/s/abcdefgh"), s("abcdefgh"));
        assert_eq!(ler_alvo("streamstudio://s/abcdefgh/"), s("abcdefgh"));
        assert_eq!(ler_alvo("ABCD-efgh"), s("abcdefgh"));
        assert_eq!(ler_alvo("abcd efgh"), s("abcdefgh"));
        assert_eq!(ler_alvo("abcdefgh"), s("abcdefgh"));
        assert_eq!(ler_alvo("ab-cdefgh"), Some(Alvo::Nome("ab-cdefgh".into())));
        assert_eq!(ler_alvo("Sala do Zé"), Some(Alvo::Nome("Sala do Zé".into())));
        assert_eq!(ler_alvo("   "), None);
    }

    #[test]
    fn servidor_configuravel() {
        let p = ClienteApi::padrao();
        assert_eq!(p.servidor(), "https://stream.exemplo.com/stream");
        assert_eq!(p.url_livekit(), "wss://stream.exemplo.com/stream");
        let l = ClienteApi::novo("http://localhost:8080/").unwrap();
        assert_eq!(l.servidor(), "http://localhost:8080");
        assert_eq!(l.url_livekit(), "ws://localhost:8080");
        for ruim in ["", "stream.exemplo.com", "ftp://x/y", "https://x/?a=1", "nada aqui"] {
            assert!(ClienteApi::novo(ruim).is_err(), "deveria recusar {ruim:?}");
        }
    }

    /// Sobe `api/token_server.py` numa porta livre, com estado temporário.
    /// O LiveKit não precisa estar rodando: a API ignora quando ele não responde.
    struct ApiLocal {
        processo: std::process::Child,
        pasta: std::path::PathBuf,
        porta: u16,
    }

    impl ApiLocal {
        async fn subir() -> Self {
            let raiz = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let porta = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let pasta = raiz.join(format!("dev/estado/f22-teste-{porta}"));
            std::fs::create_dir_all(&pasta).unwrap();
            let processo = std::process::Command::new("python")
                .arg(raiz.join("api/token_server.py"))
                .env("LK_API_KEY", "devkey")
                .env("LK_API_SECRET", "secret")
                .env("API_PORT", porta.to_string())
                // Caminho relativo evita interpretar a letra da unidade como separador.
                .current_dir(&pasta)
                .env("STATE_DIRECTORY", ".")
                .env("LK_URL", "http://127.0.0.1:9")
                .env("MAX_SALAS", "20")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("precisa do python no PATH");
            let api = Self { processo, pasta, porta };
            let cliente = api.cliente();
            for _ in 0..100 {
                if cliente.saude().await.is_ok() {
                    return api;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("a API local não subiu na porta {porta}");
        }

        fn cliente(&self) -> ClienteApi {
            ClienteApi::novo(&format!("http://127.0.0.1:{}", self.porta)).unwrap()
        }
    }

    impl Drop for ApiLocal {
        fn drop(&mut self) {
            let _ = self.processo.kill();
            let _ = self.processo.wait();
            let _ = std::fs::remove_dir_all(&self.pasta);
        }
    }

    /// Lê o `video.room`, o `sub` e as fontes permitidas de dentro do JWT.
    fn conteudo_token(token: &str) -> Value {
        let parte = token.split('.').nth(1).expect("JWT com 3 partes");
        let tabela = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let (mut bits, mut n, mut bytes) = (0u32, 0, Vec::new());
        for c in parte.bytes() {
            let v = tabela.iter().position(|&t| t == c).expect("base64url") as u32;
            bits = (bits << 6) | v;
            n += 6;
            if n >= 8 {
                n -= 8;
                bytes.push((bits >> n) as u8);
            }
        }
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn fluxo_completo_contra_api_local() {
        let api = ApiLocal::subir().await;
        let c = api.cliente();
        let criada = c.criar_sala("Teste F2.2", "senha123", 5).await.unwrap();
        let sala = &criada.sala;
        assert_eq!(sala.nome, "Teste F2.2");
        assert_eq!(sala.codigo.replace('-', ""), sala.id);

        // código + senha, com e sem hífen
        let e1 = c.entrar_por_codigo(&sala.codigo, "senha123").await.unwrap();
        assert_eq!(&e1.sala, sala);
        let e2 = c.entrar(&ler_alvo(&sala.id.to_uppercase()).unwrap(), "senha123").await.unwrap();
        assert_eq!(e2.sala.id, sala.id);
        // nome + senha
        let e3 = c.entrar(&ler_alvo("teste f2.2").unwrap(), "senha123").await.unwrap();
        assert_eq!(e3.sala.id, sala.id);
        // senha errada
        let erro = c.entrar_por_codigo(&sala.id, "errada").await.unwrap_err();
        assert_eq!(erro.status(), Some(401));
        assert_eq!(erro.to_string(), "Senha errada. Confira com quem te chamou.");
        // sala que não existe
        let erro = c.entrar_por_codigo("zzzz-zzzz", "senha123").await.unwrap_err();
        assert_eq!(erro.status(), Some(404));

        // convite, pelo código e pelo link inteiro
        let e4 = c.entrar_por_convite(&sala.convite).await.unwrap();
        assert_eq!(e4.sala.id, sala.id);
        let link = format!("{}/c/{}", c.servidor(), sala.convite);
        let e5 = c.entrar(&ler_alvo(&link).unwrap(), "").await.unwrap();
        assert_eq!(e5.sala.id, sala.id);
        let erro = c.entrar_por_convite("naoexiste99").await.unwrap_err();
        assert_eq!(erro.status(), Some(404));

        // token e renovação
        let a1 = c.token(&e4.chave, "Neto").await.unwrap();
        assert_eq!(a1.sala.id, sala.id);
        let jwt = conteudo_token(&a1.token);
        assert_eq!(jwt["video"]["room"], format!("s-{}", sala.id));
        assert_eq!(jwt["name"], "Neto");
        assert!(jwt["sub"].as_str().unwrap().starts_with("Neto#"));
        let fontes = jwt["video"]["canPublishSources"].to_string();
        assert!(fontes.contains("screen_share") && fontes.contains("screen_share_audio"), "{fontes}");
        tokio::time::sleep(Duration::from_millis(1100)).await; // a validade é em segundos
        let a2 = c.token(&a1.chave, "Neto").await.unwrap();
        assert_ne!(a2.token, a1.token);
        assert_ne!(a2.chave, a1.chave);
        assert!(conteudo_token(&a2.token)["exp"].as_i64() > conteudo_token(&a1.token)["exp"].as_i64());
        assert_eq!(c.sala(&a2.chave).await.unwrap(), *sala);
        // sem apelido, chave falsa
        assert_eq!(c.token(&a2.chave, "  ").await.unwrap_err().status(), Some(400));
        assert_eq!(c.token("lixo", "Neto").await.unwrap_err().status(), Some(404));
    }

    #[tokio::test]
    async fn sem_servidor_vira_sem_conexao() {
        let porta = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let c = ClienteApi::novo(&format!("http://127.0.0.1:{porta}/stream")).unwrap();
        let erro = c.entrar_por_codigo("abcdefgh", "x").await.unwrap_err();
        assert!(matches!(erro, ErroApi::SemConexao));
        assert_eq!(erro.status(), None);
    }

    #[tokio::test]
    async fn caminho_base_e_mantido() {
        // servidor com caminho (como /stream no VPS): o pedido vai para /stream/api/...
        let ouvinte = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let porta = ouvinte.local_addr().unwrap().port();
        let servidor = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut s, _) = ouvinte.accept().await.unwrap();
            let mut buf = vec![0; 4096];
            let n = s.read(&mut buf).await.unwrap();
            let corpo = br#"{"ok":true}"#;
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", corpo.len());
            s.write_all(resp.as_bytes()).await.unwrap();
            s.write_all(corpo).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).lines().next().unwrap().to_string()
        });
        let c = ClienteApi::novo(&format!("http://127.0.0.1:{porta}/stream")).unwrap();
        c.saude().await.unwrap();
        assert_eq!(servidor.await.unwrap(), "GET /stream/api/health HTTP/1.1");
    }
}
