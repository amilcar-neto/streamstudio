#!/usr/bin/env python3
"""
Servidor de salas e tokens do StreamStudio.

Ninguém tem conta: a pessoa escolhe um apelido no navegador. Qualquer um cria uma sala com
nome + senha (até MAX_SALAS ao mesmo tempo). Dá pra entrar de três jeitos:
  - código da sala + senha
  - nome da sala + senha (nomes são únicos entre as salas abertas)
  - link de convite (/c/<convite>), que não pede senha
Quem entra recebe uma "chave" assinada daquela sala e com ela pede tokens do LiveKit.

A sala é APAGADA quando:
  - todo mundo sai (o LiveKit avisa por webhook depois de 60 s vazia, pra quem só recarregou)
  - passa o tempo de ociosidade sem ninguém transmitindo (5 min por padrão, o dono muda até 60)
  - quem criou encerra

  GET  /api/info
  POST /api/salas                {nome, senha, ocioso}  -> cria
  POST /api/entrar               {codigo | nome, senha} -> chave
  POST /api/convite/<convite>    {}                     -> chave (sem senha)
  POST /api/sala                 {chave}                -> dados da sala (convite, código, ociosidade)
  POST /api/salas/<id>/ajustes   {dono, ocioso?, novoConvite?}
  POST /api/salas/<id>/encerrar  {dono}
  POST /api/token                {chave, nome}          -> token do LiveKit
  POST /api/lk                   (webhook do LiveKit, assinado)
  GET  /api/health

Não precisa de nenhuma dependência: só o Python 3 que já vem no Ubuntu.

Variáveis de ambiente (ficam em /etc/streamamigos/api.env):
  LK_API_KEY, LK_API_SECRET  -> as mesmas do livekit.yaml
  API_PORT                   -> porta local (padrão 3100)
  STATE_DIRECTORY            -> onde as salas são guardadas (o systemd preenche)
  MAX_SALAS                  -> salas abertas ao mesmo tempo (padrão 5)
  LK_URL                     -> API interna do LiveKit (padrão http://127.0.0.1:7880)
"""
import base64
import collections
import hashlib
import hmac
import ipaddress
import json
import os
import re
import secrets
import threading
import time
import unicodedata
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

API_KEY = os.environ["LK_API_KEY"]
API_SECRET = os.environ["LK_API_SECRET"].encode()
PORT = int(os.environ.get("API_PORT", "3100"))
DIR_ESTADO = os.environ.get("STATE_DIRECTORY", "/var/lib/streamamigos").split(":")[0]
ARQ_SALAS = os.path.join(DIR_ESTADO, "salas.json")
MAX_SALAS = int(os.environ.get("MAX_SALAS", "5"))
LK_URL = os.environ.get("LK_URL", "http://127.0.0.1:7880").rstrip("/")

CHAVE_SEGREDO = hmac.new(API_SECRET, b"streamstudio-chave-sala", hashlib.sha256).digest()
CHAVE_TTL = 24 * 3600
TOKEN_TTL = 12 * 3600
PBKDF2_ITER = 200_000
SENHA_MIN = 4
OCIOSO_PADRAO, OCIOSO_MIN, OCIOSO_MAX = 5, 1, 60
VAZIA_SEGUNDOS = 60          # sala sem ninguém fecha depois disso
NUNCA_USADA_SEGUNDOS = 600   # sala criada em que ninguém entrou
ALFABETO = "abcdefghjkmnpqrstuvwxyz23456789"

# Faixas do Cloudflare (https://www.cloudflare.com/ips/). Só confiamos no CF-Connecting-IP
# quando o pedido chegou de uma delas; senão qualquer um forjaria o cabeçalho.
CLOUDFLARE = [ipaddress.ip_network(n) for n in (
    "173.245.48.0/20", "103.21.244.0/22", "103.22.200.0/22", "103.31.4.0/22",
    "141.101.64.0/18", "108.162.192.0/18", "190.93.240.0/20", "188.114.96.0/20",
    "197.234.240.0/22", "198.41.128.0/17", "162.158.0.0/15", "104.16.0.0/13",
    "104.24.0.0/14", "172.64.0.0/13", "131.0.72.0/22",
    "2400:cb00::/32", "2606:4700::/32", "2803:f800::/32", "2405:b500::/32",
    "2405:8100::/32", "2a06:98c0::/29", "2c0f:f248::/32",
)]


# ------------------------------------------------------------------ utilidades
def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def b64url_dec(s: str) -> bytes:
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


def limpar_nome(raw, limite=24) -> str:
    nome = re.sub(r"[\x00-\x1f\x7f<>]", "", str(raw or "")).strip()
    return re.sub(r"\s+", " ", nome)[:limite]


def chave_nome(nome: str) -> str:
    """Forma comparável de um nome de sala: sem acento, minúscula, espaços únicos."""
    sem_acento = unicodedata.normalize("NFKD", nome).encode("ascii", "ignore").decode()
    return re.sub(r"\s+", " ", sem_acento).strip().lower()


def aleatorio(n: int) -> str:
    return "".join(secrets.choice(ALFABETO) for _ in range(n))


def limpar_codigo(raw) -> str:
    return re.sub(r"[^a-z0-9]", "", str(raw or "").lower())[:16]


def formatar_codigo(sid: str) -> str:
    return f"{sid[:4]}-{sid[4:]}"


def hash_senha(senha: str, sal: bytes) -> str:
    return b64url(hashlib.pbkdf2_hmac("sha256", senha.encode(), sal, PBKDF2_ITER))


def assinar(dados: dict) -> str:
    corpo = b64url(json.dumps(dados, separators=(",", ":")).encode())
    return corpo + "." + b64url(hmac.new(CHAVE_SEGREDO, corpo.encode(), hashlib.sha256).digest())


def verificar(valor):
    try:
        corpo, sig = str(valor).split(".", 1)
        esperado = b64url(hmac.new(CHAVE_SEGREDO, corpo.encode(), hashlib.sha256).digest())
        if not hmac.compare_digest(sig, esperado):
            return None
        dados = json.loads(b64url_dec(corpo))
        return dados if dados.get("e", 0) > time.time() else None
    except (ValueError, TypeError, json.JSONDecodeError):
        return None


def jwt(claims: dict) -> str:
    header = {"alg": "HS256", "typ": "JWT"}
    entrada = b64url(json.dumps(header, separators=(",", ":")).encode()) + "." + \
        b64url(json.dumps(claims, separators=(",", ":")).encode())
    return entrada + "." + b64url(hmac.new(API_SECRET, entrada.encode(), hashlib.sha256).digest())


def jwt_valido(token: str):
    """Confere um JWT assinado pelo LiveKit (webhook). Devolve as claims ou None."""
    try:
        h, c, s = token.split(".")
        esperado = b64url(hmac.new(API_SECRET, f"{h}.{c}".encode(), hashlib.sha256).digest())
        if not hmac.compare_digest(s, esperado):
            return None
        claims = json.loads(b64url_dec(c))
        if claims.get("iss") != API_KEY or claims.get("exp", 0) < time.time() - 30:
            return None
        return claims
    except (ValueError, json.JSONDecodeError):
        return None


def sala_lk(sid: str) -> str:
    return f"s-{sid}"


def token_participante(sid: str, identity: str, nome: str) -> str:
    agora = int(time.time())
    return jwt({
        "iss": API_KEY, "sub": identity, "name": nome, "jti": identity,
        "nbf": agora - 10, "exp": agora + TOKEN_TTL,
        "video": {
            "room": sala_lk(sid),
            "roomJoin": True,
            "canSubscribe": True,
            "canPublish": True,
            "canPublishSources": ["screen_share", "screen_share_audio"],  # só tela, nada de câmera/mic
            "canPublishData": False,
            "canUpdateOwnMetadata": True,  # mostra pros outros quem você está assistindo
        },
    })


def lk_api(metodo: str, corpo: dict, grant: dict):
    agora = int(time.time())
    tok = jwt({"iss": API_KEY, "nbf": agora - 10, "exp": agora + 60, "video": grant})
    req = urllib.request.Request(
        f"{LK_URL}/twirp/livekit.RoomService/{metodo}",
        data=json.dumps(corpo).encode(),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {tok}"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=3) as r:
        return json.loads(r.read() or b"{}")


# ------------------------------------------------------------------ limite de tentativas
class Limite:
    """No máximo `maximo` eventos por `janela` segundos para cada chave."""

    def __init__(self, maximo: int, janela: int):
        self.maximo, self.janela = maximo, janela
        self.eventos = collections.defaultdict(collections.deque)
        self.lock = threading.Lock()

    def bloqueado(self, chave: str) -> bool:
        with self.lock:
            fila = self.eventos[chave]
            limite = time.time() - self.janela
            while fila and fila[0] < limite:
                fila.popleft()
            if not fila:
                del self.eventos[chave]
                return False
            return len(fila) >= self.maximo

    def registrar(self, chave: str):
        with self.lock:
            self.eventos[chave].append(time.time())


lim_criar = Limite(4, 3600)        # 4 salas por hora por IP
lim_senha = Limite(10, 900)        # 10 tentativas erradas a cada 15 min por IP
lim_senha_sala = Limite(60, 900)   # e 60 por sala, contra ataque de vários IPs
lim_geral = Limite(240, 60)        # 240 pedidos por minuto por IP


# ------------------------------------------------------------------ salas
_lock = threading.RLock()
_salas: dict = {}


def carregar():
    global _salas
    try:
        with open(ARQ_SALAS, encoding="utf-8") as f:
            _salas = {s["id"]: s for s in json.load(f) if "convite" in s}
    except (OSError, ValueError, KeyError, TypeError):
        _salas = {}


def salvar():
    tmp = ARQ_SALAS + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(list(_salas.values()), f)
    os.chmod(tmp, 0o600)
    os.replace(tmp, ARQ_SALAS)


def nova_chave(sala: dict) -> str:
    return assinar({"s": sala["id"], "v": sala["versao"], "e": int(time.time()) + CHAVE_TTL})


def sala_da_chave(chave):
    dados = verificar(chave)
    if not dados:
        return None
    with _lock:
        sala = _salas.get(dados.get("s"))
    return sala if sala and hmac.compare_digest(str(dados.get("v")), sala["versao"]) else None


def publico(sala: dict) -> dict:
    """O que qualquer pessoa dentro da sala pode saber."""
    return {"id": sala["id"], "codigo": formatar_codigo(sala["id"]), "nome": sala["nome"],
            "convite": sala["convite"], "ocioso": sala["ocioso"]}


def metadados(sala: dict) -> str:
    return json.dumps({"ocioso": sala["ocioso"], "semStreamDesde": sala.get("semStreamDesde")})


def garantir_sala_lk(sala: dict):
    """Cria a sala no LiveKit (se já existe, devolve a mesma)."""
    try:
        lk_api("CreateRoom", {"name": sala_lk(sala["id"]), "emptyTimeout": NUNCA_USADA_SEGUNDOS,
                              "departureTimeout": VAZIA_SEGUNDOS, "metadata": metadados(sala)},
               {"roomCreate": True})
    except Exception:
        pass


def publicar_metadados(sala: dict):
    try:
        lk_api("UpdateRoomMetadata", {"room": sala_lk(sala["id"]), "metadata": metadados(sala)},
               {"room": sala_lk(sala["id"]), "roomAdmin": True})
    except Exception:
        pass


def apagar(sid: str, derrubar: bool):
    """Some com a sala: do disco e, se pedido, do LiveKit (todos caem)."""
    with _lock:
        if _salas.pop(sid, None) is None:
            return
        salvar()
    if derrubar:
        try:
            lk_api("DeleteRoom", {"room": sala_lk(sid)}, {"roomCreate": True})
        except Exception:
            pass
    print(f"sala {sid} apagada", flush=True)


def participantes(sid: str):
    """Lista do LiveKit; None se a sala não existe lá."""
    try:
        return lk_api("ListParticipants", {"room": sala_lk(sid)},
                      {"room": sala_lk(sid), "roomAdmin": True}).get("participants", [])
    except Exception:
        return None


def alguem_transmitindo(gente) -> bool:
    return any(str(t.get("source", "")) in ("SCREEN_SHARE", "3")
               for p in gente for t in p.get("tracks", []))


def conferir(sid: str):
    """Atualiza o estado de uma sala a partir do LiveKit e decide se ela fecha."""
    with _lock:
        sala = _salas.get(sid)
    if not sala:
        return
    agora = time.time()
    gente = participantes(sid)
    if gente is None:
        # o LiveKit não conhece a sala: ou ninguém entrou ainda, ou ela já foi fechada lá
        if sala.get("teveGente") or agora - sala["criada"] > NUNCA_USADA_SEGUNDOS:
            apagar(sid, derrubar=False)
        return
    mudou = False
    with _lock:
        if gente and not sala.get("teveGente"):
            sala["teveGente"] = True
            mudou = True
        if alguem_transmitindo(gente):
            if sala.get("semStreamDesde") is not None:
                sala["semStreamDesde"] = None
                mudou = True
        elif sala.get("semStreamDesde") is None:
            sala["semStreamDesde"] = agora
            mudou = True
        desde = sala.get("semStreamDesde")
        if mudou:
            salvar()
    if desde is not None and agora - desde > sala["ocioso"] * 60:
        apagar(sid, derrubar=True)
    elif mudou:
        publicar_metadados(sala)


def vigia():
    """Confere todas as salas a cada 10 s (os webhooks deixam isso quase instantâneo)."""
    while True:
        with _lock:
            ids = list(_salas)
        for sid in ids:
            try:
                conferir(sid)
            except Exception as e:  # nunca deixar a vigia morrer
                print(f"vigia: {e}", flush=True)
        time.sleep(10)


# ------------------------------------------------------------------ HTTP
class Handler(BaseHTTPRequestHandler):
    server_version = "StreamStudio"
    sys_version = ""

    def _json(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _ip(self) -> str:
        # o Caddy põe em X-Forwarded-For o IP de quem conectou nele de verdade
        par = (self.headers.get("X-Forwarded-For") or self.client_address[0]).split(",")[-1].strip()
        try:
            if any(ipaddress.ip_address(par) in rede for rede in CLOUDFLARE):
                return self.headers.get("CF-Connecting-IP", par).strip()
        except ValueError:
            pass
        return par

    def _bruto(self) -> bytes:
        tamanho = int(self.headers.get("Content-Length") or 0)
        if tamanho > 65536:
            raise ValueError("grande demais")
        return self.rfile.read(tamanho)

    def do_GET(self):
        ip = self._ip()
        if lim_geral.bloqueado(ip):
            return self._json(429, {"erro": "Muitos pedidos. Espere um minuto."})
        lim_geral.registrar(ip)
        if self.path == "/api/health":
            return self._json(200, {"ok": True})
        if self.path == "/api/info":
            with _lock:
                abertas = len(_salas)
            return self._json(200, {"abertas": abertas, "max": MAX_SALAS, "senhaMin": SENHA_MIN})
        return self._json(404, {"erro": "Não encontrado."})

    def do_POST(self):
        try:
            bruto = self._bruto()
        except ValueError:
            return self._json(413, {"erro": "Pedido grande demais."})

        if self.path == "/api/lk":
            return self.webhook(bruto)

        ip = self._ip()
        if lim_geral.bloqueado(ip):
            return self._json(429, {"erro": "Muitos pedidos. Espere um minuto."})
        lim_geral.registrar(ip)
        try:
            dados = json.loads(bruto or b"{}")
            if not isinstance(dados, dict) or len(bruto) > 8192:
                raise ValueError
        except (ValueError, json.JSONDecodeError):
            return self._json(400, {"erro": "Pedido inválido."})

        p = self.path.strip("/").split("/")  # ["api", ...]
        if p == ["api", "salas"]:
            return self.criar(ip, dados)
        if p == ["api", "entrar"]:
            return self.entrar(ip, dados)
        if len(p) == 3 and p[:2] == ["api", "convite"]:
            return self.convite(ip, p[2])
        if p == ["api", "sala"]:
            return self.info_sala(dados)
        if len(p) == 4 and p[:2] == ["api", "salas"] and p[3] == "ajustes":
            return self.ajustes(p[2], dados)
        if len(p) == 4 and p[:2] == ["api", "salas"] and p[3] == "encerrar":
            return self.encerrar(p[2], dados)
        if p == ["api", "token"]:
            return self.token(dados)
        return self._json(404, {"erro": "Não encontrado."})

    # -------------------------------------------------- ações
    def criar(self, ip, dados):
        nome = limpar_nome(dados.get("nome"), 40)
        senha = str(dados.get("senha", ""))
        try:
            ocioso = min(OCIOSO_MAX, max(OCIOSO_MIN, int(dados.get("ocioso", OCIOSO_PADRAO))))
        except (TypeError, ValueError):
            ocioso = OCIOSO_PADRAO
        if len(senha) < SENHA_MIN or len(senha) > 128:
            return self._json(400, {"erro": f"A senha precisa ter pelo menos {SENHA_MIN} caracteres."})
        if lim_criar.bloqueado(ip):
            return self._json(429, {"erro": "Você criou muitas salas na última hora. Espere um pouco."})
        sal = secrets.token_bytes(16)
        dono = secrets.token_urlsafe(24)
        with _lock:
            if len(_salas) >= MAX_SALAS:
                return self._json(409, {"erro": f"Já tem {MAX_SALAS} salas abertas agora. Espere uma fechar."})
            if nome and any(chave_nome(s["nome"]) == chave_nome(nome) for s in _salas.values()):
                return self._json(409, {"erro": "Já tem uma sala aberta com esse nome. Escolha outro."})
            sid = aleatorio(8)
            while sid in _salas:
                sid = aleatorio(8)
            # sem nome, a sala se chama pelo próprio código
            nome = nome or formatar_codigo(sid)
            agora = time.time()
            sala = {
                "id": sid, "nome": nome, "sal": b64url(sal), "hash": hash_senha(senha, sal),
                "dono": hashlib.sha256(dono.encode()).hexdigest(), "versao": secrets.token_hex(4),
                "convite": aleatorio(10), "ocioso": ocioso,
                "criada": agora, "semStreamDesde": agora, "teveGente": False,
            }
            _salas[sid] = sala
            salvar()
        lim_criar.registrar(ip)
        garantir_sala_lk(sala)
        return self._json(200, {**publico(sala), "chave": nova_chave(sala), "dono": dono})

    def entrar(self, ip, dados):
        if lim_senha.bloqueado(ip):
            return self._json(429, {"erro": "Muitas tentativas erradas. Espere 15 minutos."})
        codigo = limpar_codigo(dados.get("codigo"))
        nome = chave_nome(limpar_nome(dados.get("nome"), 40))
        with _lock:
            if codigo:
                sala = _salas.get(codigo)
            else:
                sala = next((s for s in _salas.values() if nome and chave_nome(s["nome"]) == nome), None)
        if not sala:
            lim_senha.registrar(ip)
            return self._json(404, {"erro": "Não achei essa sala. Ela pode ter fechado."})
        if lim_senha_sala.bloqueado(sala["id"]):
            return self._json(429, {"erro": "Muitas tentativas erradas nessa sala. Espere 15 minutos."})
        senha = str(dados.get("senha", ""))[:128]
        if not hmac.compare_digest(hash_senha(senha, b64url_dec(sala["sal"])), sala["hash"]):
            lim_senha.registrar(ip)
            lim_senha_sala.registrar(sala["id"])
            return self._json(401, {"erro": "Senha errada. Confira com quem te chamou."})
        return self._json(200, {**publico(sala), "chave": nova_chave(sala)})

    def convite(self, ip, codigo):
        if lim_senha.bloqueado(ip):
            return self._json(429, {"erro": "Muitas tentativas erradas. Espere 15 minutos."})
        codigo = limpar_codigo(codigo)
        with _lock:
            sala = next((s for s in _salas.values() if codigo and hmac.compare_digest(s["convite"], codigo)), None)
        if not sala:
            lim_senha.registrar(ip)
            return self._json(404, {"erro": "Esse convite não vale mais. A sala fechou ou o link foi trocado."})
        return self._json(200, {**publico(sala), "chave": nova_chave(sala)})

    def info_sala(self, dados):
        sala = sala_da_chave(dados.get("chave"))
        if not sala:
            return self._json(404, {"erro": "Essa sala não existe mais."})
        return self._json(200, publico(sala))

    def ajustes(self, sid, dados):
        with _lock:
            sala = _salas.get(sid)
            dono = hashlib.sha256(str(dados.get("dono", "")).encode()).hexdigest()
            if not sala or not hmac.compare_digest(dono, sala["dono"]):
                return self._json(403, {"erro": "Só quem criou a sala pode mudar isso."})
            if "ocioso" in dados:
                try:
                    sala["ocioso"] = min(OCIOSO_MAX, max(OCIOSO_MIN, int(dados["ocioso"])))
                except (TypeError, ValueError):
                    return self._json(400, {"erro": "Tempo inválido."})
            if dados.get("novoConvite"):
                sala["convite"] = aleatorio(10)
            salvar()
        publicar_metadados(sala)
        return self._json(200, publico(sala))

    def encerrar(self, sid, dados):
        with _lock:
            sala = _salas.get(sid)
            dono = hashlib.sha256(str(dados.get("dono", "")).encode()).hexdigest()
            if not sala or not hmac.compare_digest(dono, sala["dono"]):
                return self._json(403, {"erro": "Só quem criou a sala pode encerrar."})
        apagar(sid, derrubar=True)
        return self._json(200, {"ok": True})

    def token(self, dados):
        sala = sala_da_chave(dados.get("chave"))
        if not sala:
            return self._json(404, {"erro": "Essa sala não existe mais."})
        nome = limpar_nome(dados.get("nome"))
        if not nome:
            return self._json(400, {"erro": "Escolha um apelido antes de entrar."})
        garantir_sala_lk(sala)
        identity = f"{nome}#{secrets.token_hex(3)}"
        return self._json(200, {"token": token_participante(sala["id"], identity, nome),
                                "sala": publico(sala), "chave": nova_chave(sala)})

    def webhook(self, bruto):
        claims = jwt_valido(self.headers.get("Authorization", "").removeprefix("Bearer ").strip())
        if not claims:
            return self._json(401, {"erro": "assinatura"})
        esperado = base64.b64encode(hashlib.sha256(bruto).digest()).decode()
        if not hmac.compare_digest(str(claims.get("sha256", "")), esperado):
            return self._json(401, {"erro": "corpo"})
        try:
            ev = json.loads(bruto)
        except json.JSONDecodeError:
            return self._json(400, {"erro": "json"})
        nome_lk = (ev.get("room") or {}).get("name", "")
        sid = nome_lk[2:] if nome_lk.startswith("s-") else ""
        evento = ev.get("event", "")
        if sid:
            if evento == "room_finished":
                apagar(sid, derrubar=False)
            elif evento in ("participant_joined", "participant_left", "track_published", "track_unpublished"):
                threading.Thread(target=conferir, args=(sid,), daemon=True).start()
        return self._json(200, {"ok": True})

    def log_message(self, fmt, *args):
        # loga só o essencial (sem senha, sem token, sem IP)
        if "/api/lk" not in (args[0] if args else ""):
            print(fmt % args, flush=True)


if __name__ == "__main__":
    os.makedirs(DIR_ESTADO, exist_ok=True)
    carregar()
    threading.Thread(target=vigia, daemon=True).start()
    servidor = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    servidor.daemon_threads = True
    print(f"StreamStudio API ouvindo em 127.0.0.1:{PORT} ({len(_salas)} salas)", flush=True)
    servidor.serve_forever()
