#!/usr/bin/env bash
# Instalador do StreamAmigos
# Uso:
#   sudo bash install.sh stream.exemplo.com /stream     -> site em https://stream.exemplo.com/stream/
#   sudo bash install.sh stream.exemplo.com             -> site num subdomínio próprio
#
# - Instala o LiveKit (servidor de mídia) sem Docker
# - Instala o servidor de senha/salas/token (Python, já vem no Ubuntu)
# - Copia o site pra /opt/streamamigos/public
# - Adiciona o StreamAmigos no Caddy SEM mexer no resto (faz backup antes e desfaz se der erro)
# Pode rodar de novo pra atualizar: mantém senha, chaves e a lista de salas.
set -euo pipefail

DOMINIO="${1:-}"
CAMINHO="${2:-}"
DIR_APP=/opt/streamamigos
DIR_CONF=/etc/streamamigos
CADDYFILE=/etc/caddy/Caddyfile
ORIGEM="$(cd "$(dirname "$0")" && pwd)"

verde()  { printf '\033[1;32m%s\033[0m\n' "$*"; }
amarelo(){ printf '\033[1;33m%s\033[0m\n' "$*"; }
vermelho(){ printf '\033[1;31m%s\033[0m\n' "$*"; }

[ "$(id -u)" -eq 0 ] || { vermelho "Rode como root (sudo bash install.sh ...)"; exit 1; }
[ -n "$DOMINIO" ] || { vermelho "Faltou o domínio. Ex: sudo bash install.sh stream.exemplo.com /stream"; exit 1; }
if [ -n "$CAMINHO" ]; then
  CAMINHO="/${CAMINHO#/}"; CAMINHO="${CAMINHO%/}"
  [[ "$CAMINHO" =~ ^/[a-z0-9_-]+$ ]] || { vermelho "Caminho inválido: use algo como /stream"; exit 1; }
fi

# ---------------------------------------------------------------- 1. LiveKit
verde "[1/6] Instalando o LiveKit..."
if ! command -v livekit-server >/dev/null 2>&1; then
  curl -sSL https://get.livekit.io | bash
fi
livekit-server --version

# ---------------------------------------------------------------- 2. Chaves, senha e salas
verde "[2/6] Gerando configuração..."
mkdir -p "$DIR_CONF"
if [ -f "$DIR_CONF/api.env" ]; then
  # shellcheck disable=SC1091
  . "$DIR_CONF/api.env"
  amarelo "Configuração existente mantida (chaves iguais)."
else
  LK_API_KEY="SA$(openssl rand -hex 6)"
  LK_API_SECRET="$(openssl rand -hex 32)"
fi
# reescreve sempre: versões antigas tinham uma senha única do site, que não existe mais
cat > "$DIR_CONF/api.env" <<EOF
LK_API_KEY=$LK_API_KEY
LK_API_SECRET=$LK_API_SECRET
API_PORT=3100
MAX_SALAS=5
EOF
rm -f "$DIR_CONF/salas.json"   # salas agora são criadas pelo site e ficam em /var/lib/streamamigos

cat > "$DIR_CONF/livekit.yaml" <<EOF
# Gerado pelo install.sh do StreamAmigos
port: 7880
log_level: info
rtc:
  tcp_port: 7881          # alternativa por TCP pra quem tem UDP bloqueado
  udp_port: 7882          # toda a mídia passa por essa porta UDP
  use_external_ip: true
keys:
  $LK_API_KEY: $LK_API_SECRET
room:
  auto_create: false      # só a API cria salas
  max_participants: 0     # sem limite de pessoas por sala
# avisa a API quando alguém entra, sai ou começa/para de transmitir (pra fechar salas na hora)
webhook:
  api_key: $LK_API_KEY
  urls:
    - http://127.0.0.1:3100/api/lk
EOF

# Buffers UDP maiores: evita perda de pacote com bitrate alto
cat > /etc/sysctl.d/90-streamamigos.conf <<EOF
net.core.rmem_max=16777216
net.core.wmem_max=16777216
net.core.rmem_default=4194304
net.core.wmem_default=4194304
EOF
sysctl --system >/dev/null

# ---------------------------------------------------------------- 3. Arquivos
verde "[3/6] Copiando o site..."
mkdir -p "$DIR_APP"
rm -rf "$DIR_APP/public" "$DIR_APP/api"
cp -r "$ORIGEM/public" "$ORIGEM/api" "$DIR_APP/"
# carimba a versão nos links do index.html pra ninguém ficar com CSS/JS velho em cache
sed -i -e "s/__VERSAO__/$(date +%s)/g" -e "s#__BASE__#${CAMINHO}#g" "$DIR_APP/public/index.html"
chmod -R a+rX "$DIR_APP"

# ---------------------------------------------------------------- 4. Serviços
verde "[4/6] Criando serviços (ligam sozinhos quando o VPS reinicia)..."
# trava o que os serviços podem fazer: se algum for invadido, não mexe no resto do VPS
endurecer() {
  cat <<'EOF'
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
LockPersonality=yes
CapabilityBoundingSet=
AmbientCapabilities=
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK
SystemCallArchitectures=native
UMask=0077
EOF
}
cat > /etc/systemd/system/streamamigos-livekit.service <<EOF
[Unit]
Description=StreamAmigos - LiveKit
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=$(command -v livekit-server) --config $DIR_CONF/livekit.yaml
Restart=always
RestartSec=3
LimitNOFILE=65536
User=streamamigos
$(endurecer)

[Install]
WantedBy=multi-user.target
EOF

cat > /etc/systemd/system/streamamigos-api.service <<EOF
[Unit]
Description=StreamAmigos - senha, salas e tokens
After=network-online.target streamamigos-livekit.service

[Service]
EnvironmentFile=$DIR_CONF/api.env
ExecStart=/usr/bin/python3 $DIR_APP/api/token_server.py
Restart=always
RestartSec=3
User=streamamigos
# salas criadas pelo site ficam em /var/lib/streamamigos (único lugar onde o serviço escreve)
StateDirectory=streamamigos
StateDirectoryMode=0700
$(endurecer)

[Install]
WantedBy=multi-user.target
EOF

id streamamigos >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin streamamigos
chown root:streamamigos "$DIR_CONF" "$DIR_CONF"/*
chmod 750 "$DIR_CONF"
chmod 640 "$DIR_CONF"/*

systemctl daemon-reload
systemctl enable streamamigos-livekit streamamigos-api >/dev/null
systemctl restart streamamigos-livekit streamamigos-api

# ---------------------------------------------------------------- 5. Firewall
verde "[5/6] Liberando portas..."
if command -v ufw >/dev/null && ufw status | grep -q "Status: active"; then
  ufw allow 7881/tcp >/dev/null
  ufw allow 7882/udp >/dev/null
  echo "ufw: 7881/tcp e 7882/udp liberadas."
else
  echo "ufw não está ativo, nada a fazer aqui."
fi
amarelo "Se o painel da Hostinger tiver firewall ligado, libere lá também: 7881 TCP e 7882 UDP."

# ---------------------------------------------------------------- 6. Caddy
verde "[6/6] Configurando o Caddy..."
# cabeçalhos de segurança: só carrega coisas do próprio site, não pode ser posto em iframe,
# não vaza o endereço (com a sala) pra outros sites, e só libera captura de tela
CABECALHOS="header {
            Content-Security-Policy \"default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self'; img-src 'self' data:; media-src 'self' blob:; connect-src 'self' wss://{host}; worker-src 'self' blob:; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'\"
            X-Content-Type-Options \"nosniff\"
            X-Frame-Options \"DENY\"
            Referrer-Policy \"no-referrer\"
            X-Robots-Tag \"noindex, nofollow\"
            Cross-Origin-Opener-Policy \"same-origin\"
            Permissions-Policy \"display-capture=(self), fullscreen=(self), picture-in-picture=(self), autoplay=(self), geolocation=(), payment=(), usb=()\"
            -Server
        }"
if [ -n "$CAMINHO" ]; then
  # entra dentro do bloco do domínio que já existe, entre marcadores
  TRECHO="    # --- StreamAmigos (inicio) ---
    redir $CAMINHO $CAMINHO/
    handle_path $CAMINHO/* {
        $CABECALHOS
        # sinalização do LiveKit (WebSocket)
        handle /rtc* {
            reverse_proxy 127.0.0.1:7880
        }
        # senha, salas e token
        handle /api/* {
            reverse_proxy 127.0.0.1:3100
        }
        # o site (endereços como /s/<código> e /c/<convite> abrem o index.html)
        handle {
            root * $DIR_APP/public
            encode zstd gzip
            try_files {path} /index.html
            header Cache-Control \"no-cache\"
            file_server
        }
    }
    # --- StreamAmigos (fim) ---"
else
  TRECHO="# --- StreamAmigos (inicio) ---
$DOMINIO {
    encode zstd gzip
    $CABECALHOS
    handle /rtc* {
        reverse_proxy 127.0.0.1:7880
    }
    handle /api/* {
        reverse_proxy 127.0.0.1:3100
    }
    handle {
        root * $DIR_APP/public
        try_files {path} /index.html
        header Cache-Control \"no-cache\"
        file_server
    }
}
# --- StreamAmigos (fim) ---"
fi

if [ ! -f "$CADDYFILE" ]; then
  amarelo "Não achei $CADDYFILE. Adicione isto no seu Caddyfile manualmente:"
  echo "$TRECHO"
else
  BACKUP="$CADDYFILE.bak-$(date +%Y%m%d-%H%M%S)"
  cp "$CADDYFILE" "$BACKUP"
  TRECHO="$TRECHO" DOMINIO="$DOMINIO" CAMINHO="$CAMINHO" python3 - "$CADDYFILE" <<'PY'
import os, re, sys
arq = sys.argv[1]
texto = open(arq, encoding="utf-8").read()
trecho = os.environ["TRECHO"]
dominio = os.environ["DOMINIO"]
# tira uma instalação anterior (entre os marcadores)
texto = re.sub(r"\n?[ \t]*# --- StreamAmigos \(inicio\) ---.*?# --- StreamAmigos \(fim\) ---[ \t]*\n?", "\n", texto, flags=re.S)
if os.environ["CAMINHO"]:
    m = re.search(r"^" + re.escape(dominio) + r"\s*\{[ \t]*\n", texto, flags=re.M)
    if not m:
        sys.exit(f"O Caddyfile não tem um bloco '{dominio} {{'. Rode sem o caminho pra criar um site novo.")
    texto = texto[:m.end()] + trecho + "\n\n" + texto[m.end():]
else:
    texto = texto.rstrip("\n") + "\n\n" + trecho + "\n"
open(arq, "w", encoding="utf-8").write(texto)
PY
  if caddy validate --config "$CADDYFILE" --adapter caddyfile >/dev/null 2>&1; then
    systemctl reload caddy
    echo "Caddy atualizado e recarregado (backup: $BACKUP)."
  else
    vermelho "O Caddy recusou a configuração. Desfazendo..."
    cp "$BACKUP" "$CADDYFILE"
    caddy validate --config "$CADDYFILE" --adapter caddyfile || true
    exit 1
  fi
fi

# ---------------------------------------------------------------- Checagens
sleep 2
echo
systemctl is-active --quiet streamamigos-livekit && echo "LiveKit: rodando" || vermelho "LiveKit: PARADO (veja: journalctl -u streamamigos-livekit -n 50)"
systemctl is-active --quiet streamamigos-api && echo "API:     rodando" || vermelho "API: PARADA (veja: journalctl -u streamamigos-api -n 50)"
echo
verde "Pronto! Acesse: https://$DOMINIO${CAMINHO}/"
echo "Limite de salas e tempo de expiração: nano $DIR_CONF/api.env && systemctl restart streamamigos-api"
