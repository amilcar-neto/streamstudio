//! F2.7: o que está saindo de verdade e quem está limitando: o PC ou a internet.
//! Mesma lógica do painel e do aviso do site (`public/app.js`, `lerStats` e `monitorarTransmissao`).
use serde::Serialize;

/// O que limita a transmissão agora.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gargalo {
    Nenhum,
    /// O codificador não dá conta (CPU).
    Pc,
    /// A estimativa de banda baixou a qualidade.
    Rede,
    /// Nenhuma imagem nova há alguns segundos, ou a janela está minimizada.
    SemImagem,
    /// A janela transmitida foi fechada.
    FonteFechada,
    /// A conexão com a sala caiu; o app está tentando voltar (F2.8).
    Conexao,
}

/// Segundos sem quadro novo até avisar, como no site.
pub const SEGUNDOS_PARADO: f64 = 6.0;
/// No começo o WebRTC sempre marca `bandwidth` enquanto mede a banda (até numa rede local).
/// Aviso de rede só depois disso.
pub const SEGUNDOS_AQUECENDO: f64 = 15.0;
/// O codificador entregando menos de 80% dos quadros por este tempo, sem limite de banda,
/// também é gargalo do PC: o OpenH264 perde quadros sem o WebRTC marcar `cpu` (ensaio F2.7).
pub const SEGUNDOS_ENCODER_ATRASADO: f64 = 5.0;
pub const FRACAO_ENCODER_ATRASADO: f64 = 0.8;

/// Situação da fonte e do codificador num instante.
pub struct Sinais<'a> {
    /// `false` enquanto a conexão com a sala está caída ou voltando.
    pub conectado: bool,
    /// `none`, `cpu`, `bandwidth` ou `other`, vindo do WebRTC.
    pub limitacao: &'a str,
    pub segundos_sem_quadro: f64,
    pub minimizada: bool,
    /// Janela maximizada/tela cheia com outra por cima: a transmissão congela (P3).
    pub coberta: bool,
    pub fechada: bool,
    /// Desde o início da transmissão ou da última troca de fonte.
    pub segundos_no_ar: f64,
    /// Tempo seguido com o envio abaixo de `FRACAO_ENCODER_ATRASADO` da captura.
    pub segundos_encoder_atrasado: f64,
    /// Codec em uso (`h264`, `vp9`...), para a dica do aviso de CPU.
    pub codec: &'a str,
}

/// Decide o gargalo e o aviso para a pessoa. A ordem importa: sem imagem não há o que limitar.
pub fn avaliar(s: &Sinais) -> (Gargalo, Option<String>) {
    if !s.conectado {
        return (Gargalo::Conexao, Some("A conexão com a sala caiu. Tentando voltar sozinho…".into()));
    }
    if s.fechada {
        return (Gargalo::FonteFechada, Some("A janela transmitida foi fechada. Escolha outra fonte.".into()));
    }
    if s.coberta {
        return (
            Gargalo::SemImagem,
            Some("Outra janela está por cima da que você transmite e não foi possível continuar capturando: quem assiste vê a imagem parada até ela ficar visível de novo.".into()),
        );
    }
    if s.minimizada || s.segundos_sem_quadro >= SEGUNDOS_PARADO {
        return (
            Gargalo::SemImagem,
            Some(
                "Nenhuma imagem nova há alguns segundos. Se a janela transmitida está minimizada, restaure ela: \
                 o Windows não desenha janela minimizada. Transmitir a tela inteira evita isso."
                    .into(),
            ),
        );
    }
    let pc = || {
        let dica = if s.codec == "h264" { "Troque o codec pra VP9 ou baixe a resolução." } else { "Baixe a resolução ou o fps." };
        (Gargalo::Pc, Some(format!("O PC está no limite. {dica}")))
    };
    match s.limitacao {
        "cpu" => pc(),
        "bandwidth" if s.segundos_no_ar >= SEGUNDOS_AQUECENDO => {
            (Gargalo::Rede, Some("Sua internet está no limite. Baixe o bitrate.".into()))
        }
        "bandwidth" => (Gargalo::Nenhum, None),
        _ if s.segundos_encoder_atrasado >= SEGUNDOS_ENCODER_ATRASADO => pc(),
        _ => (Gargalo::Nenhum, None),
    }
}

/// Painel de estatísticas, lido pela interface a cada segundo.
#[derive(Debug, Clone, Serialize)]
pub struct Estatisticas {
    pub no_ar_s: u64,
    pub largura: u32,
    pub altura: u32,
    pub codec: String,
    /// Nome do codificador (ex.: `libvpx`, `OpenH264`).
    pub encoder: String,
    pub hardware: bool,
    pub fps_pedido: u32,
    /// Quadros entregues ao codificador por segundo (depois do limite de fps).
    pub fps_captura: f64,
    /// Quadros que saíram codificados por segundo.
    pub fps_envio: f64,
    pub mbps: f64,
    /// Perda de pacotes de vídeo informada por quem recebe (o servidor), em %.
    pub perda_pct: f64,
    pub ping_ms: Option<f64>,
    pub limitacao: String,
    pub som_kbps: Option<f64>,
    /// Maior amostra de som do último segundo, de 0 a 1.
    pub som_pico: Option<f64>,
    pub gargalo: Gargalo,
    pub aviso: Option<String>,
    /// Estado da conexão com a sala (F2.8).
    pub conexao: crate::conexao::Conexao,
    /// Quantas vezes o app entrou de novo na sala sozinho.
    pub reentradas: u32,
}

#[cfg(test)]
mod testes {
    use super::*;

    fn sinais(limitacao: &str) -> Sinais<'_> {
        Sinais {
            conectado: true,
            limitacao,
            segundos_sem_quadro: 0.0,
            minimizada: false,
            coberta: false,
            fechada: false,
            segundos_no_ar: 60.0,
            segundos_encoder_atrasado: 0.0,
            codec: "vp9",
        }
    }

    #[test]
    fn gargalos_como_no_site() {
        assert_eq!(avaliar(&sinais("none")), (Gargalo::Nenhum, None));
        assert_eq!(avaliar(&sinais("other")).0, Gargalo::Nenhum);
        assert_eq!(avaliar(&sinais("cpu")).0, Gargalo::Pc);
        assert!(avaliar(&Sinais { codec: "h264", ..sinais("cpu") }).1.unwrap().contains("VP9"));
        assert_eq!(avaliar(&sinais("bandwidth")).0, Gargalo::Rede);
    }

    #[test]
    fn medicao_inicial_de_banda_nao_e_aviso() {
        assert_eq!(avaliar(&Sinais { segundos_no_ar: 3.0, ..sinais("bandwidth") }).0, Gargalo::Nenhum);
        assert_eq!(avaliar(&Sinais { segundos_no_ar: 15.0, ..sinais("bandwidth") }).0, Gargalo::Rede);
        // CPU não espera
        assert_eq!(avaliar(&Sinais { segundos_no_ar: 1.0, ..sinais("cpu") }).0, Gargalo::Pc);
    }

    #[test]
    fn encoder_perdendo_quadros_e_pc() {
        let atrasado = Sinais { segundos_encoder_atrasado: 5.0, codec: "h264", ..sinais("none") };
        assert_eq!(avaliar(&atrasado).0, Gargalo::Pc);
        assert!(avaliar(&atrasado).1.unwrap().contains("VP9"));
        assert_eq!(avaliar(&Sinais { segundos_encoder_atrasado: 4.0, ..sinais("none") }).0, Gargalo::Nenhum);
        // com a banda limitada, a perda de quadros é da rede
        assert_eq!(avaliar(&Sinais { segundos_encoder_atrasado: 9.0, ..sinais("bandwidth") }).0, Gargalo::Rede);
    }

    #[test]
    fn sem_imagem_vem_antes_da_limitacao() {
        assert_eq!(avaliar(&Sinais { segundos_sem_quadro: 6.0, ..sinais("cpu") }).0, Gargalo::SemImagem);
        assert_eq!(avaliar(&Sinais { segundos_sem_quadro: 5.9, ..sinais("none") }).0, Gargalo::Nenhum);
        assert_eq!(avaliar(&Sinais { minimizada: true, ..sinais("bandwidth") }).0, Gargalo::SemImagem);
        assert_eq!(avaliar(&Sinais { fechada: true, minimizada: true, ..sinais("cpu") }).0, Gargalo::FonteFechada);
    }

    #[test]
    fn sem_conexao_vem_antes_de_tudo() {
        let caiu = Sinais { conectado: false, segundos_sem_quadro: 30.0, fechada: true, ..sinais("bandwidth") };
        assert_eq!(avaliar(&caiu).0, Gargalo::Conexao);
    }
}
