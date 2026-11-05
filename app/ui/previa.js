// Planos sem perda: crus ou DEFLATE/zlib, descomprimido pelo navegador.
export async function abrirPlanos(bytes, compartilhado) {
  if (bytes.byteLength < 20) throw Error('Quadro de prévia incompleto');
  const cab = new DataView(bytes), w = cab.getUint32(0,true), h = cab.getUint32(4,true), modo = cab.getUint32(16,true);
  if (!w || !h || w>4096 || h>4096 || (w&1) || (h&1)) throw Error('Dimensões da prévia inválidas');
  const tamanho = w*h*3/2;
  if (modo === 2) {
    if (bytes.byteLength !== 20 || !compartilhado || compartilhado.byteLength < tamanho) throw Error('Memória da prévia indisponível');
    return new Uint8Array(compartilhado,0,tamanho);
  }
  if (modo === 0) {
    if (bytes.byteLength !== 20+tamanho) throw Error('Quadro de prévia inválido');
    return new Uint8Array(bytes,20);
  }
  if (modo !== 1) throw Error('Pacote de prévia inválido');
  const fluxo = new Blob([new Uint8Array(bytes,20)]).stream().pipeThrough(new DecompressionStream('deflate'));
  const leitor = fluxo.getReader(), planos = new Uint8Array(tamanho); let destino=0;
  try {
    while(true) {
      const {done,value} = await leitor.read(); if(done) break;
      if(destino+value.length>tamanho) throw Error('Quadro de prévia maior que o esperado');
      planos.set(value,destino); destino+=value.length;
    }
    if(destino !== tamanho) throw Error('Quadro de prévia truncado');
    return planos;
  } finally { await leitor.cancel().catch(() => {}); leitor.releaseLock(); }
}

// I420 em três texturas. A faixa limitada BT.601 é a mesma usada pelo libyuv na captura.
export function lerMemoria(memoria, ultimo) {
  const h=new DataView(memoria.buffer), indice=h.getUint32(0,true);
  if(indice>2) return null;
  const base=16+indice*memoria.passo, seq=h.getBigUint64(base,true);
  if(!seq || Number(seq)===ultimo) return null;
  const w=h.getUint32(base+8,true), altura=h.getUint32(base+12,true), n=w*altura*3/2;
  if(!w || !altura || (w&1) || (altura&1) || w>4096 || altura>4096 || n>memoria.capacidade) return null;
  const bytes=new ArrayBuffer(20), cab=new DataView(bytes);
  cab.setUint32(0,w,true); cab.setUint32(4,altura,true); cab.setBigUint64(8,seq,true); cab.setUint32(16,2,true);
  return {bytes,planos:new Uint8Array(memoria.buffer,base+32,n),valido:() => h.getBigUint64(base,true)===seq};
}

export function criarPrevia(canvas) {
  const gl = canvas.getContext('webgl', {alpha: false, antialias: false, depth: false, preserveDrawingBuffer: false});
  if (!gl) throw Error('WebGL indisponível');
  function shader(tipo, codigo) {
    const s = gl.createShader(tipo); gl.shaderSource(s, codigo); gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw Error(gl.getShaderInfoLog(s));
    return s;
  }
  const programa = gl.createProgram();
  const vert = shader(gl.VERTEX_SHADER, 'attribute vec2 p; varying vec2 uv; void main(){gl_Position=vec4(p,0.,1.); uv=vec2((p.x+1.)*.5,(1.-p.y)*.5);}');
  const frag = shader(gl.FRAGMENT_SHADER, `precision mediump float;
    varying vec2 uv; uniform sampler2D y; uniform sampler2D u; uniform sampler2D v;
    void main(){float l=1.16438356*(texture2D(y,uv).r-16./255.);
      float a=texture2D(u,uv).r-128./255., b=texture2D(v,uv).r-128./255.;
      gl_FragColor=vec4(l+1.59602678*b,l-.39176229*a-.81296765*b,l+2.01723214*a,1.);}`);
  gl.attachShader(programa, vert); gl.attachShader(programa, frag); gl.linkProgram(programa);
  gl.deleteShader(vert); gl.deleteShader(frag);
  if (!gl.getProgramParameter(programa, gl.LINK_STATUS)) throw Error(gl.getProgramInfoLog(programa));
  gl.useProgram(programa);
  const buffer = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1,-1,1,-1,-1,1,1,1]), gl.STATIC_DRAW);
  const p = gl.getAttribLocation(programa, 'p'); gl.enableVertexAttribArray(p); gl.vertexAttribPointer(p,2,gl.FLOAT,false,0,0);
  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  const texturas = ['y','u','v'].map((nome,i) => {
    const t = gl.createTexture(); gl.activeTexture(gl.TEXTURE0+i); gl.bindTexture(gl.TEXTURE_2D,t);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_MIN_FILTER,gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_MAG_FILTER,gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_WRAP_S,gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_WRAP_T,gl.CLAMP_TO_EDGE);
    gl.uniform1i(gl.getUniformLocation(programa,nome),i); return t;
  });
  const perder = e => { e.preventDefault(); canvas.dataset.erro = 'Prévia interrompida pela placa de vídeo'; };
  const restaurar = () => { canvas._restaurar = true; };
  canvas.addEventListener('webglcontextlost', perder);
  canvas.addEventListener('webglcontextrestored', restaurar);
  let dimensoes = '';
  let restaurado;
  const desenhar = (bytes, planos, valido = () => true) => {
    if (gl.isContextLost()) return 0;
    if (canvas._restaurar) { canvas._restaurar = false; restaurado = criarPrevia(canvas); }
    if (restaurado) return restaurado(bytes, planos, valido);
    const h = new DataView(bytes), w = h.getUint32(0,true), altura = h.getUint32(4,true);
    if (planos.length !== w*altura*3/2) throw Error('Planos da prévia inválidos');
    const mudou = dimensoes !== `${w}x${altura}`;
    if (mudou) { canvas.width=w; canvas.height=altura; dimensoes=`${w}x${altura}`; gl.viewport(0,0,w,altura); }
    let pos=0;
    texturas.forEach((t,i) => {
      const largura = i ? w/2 : w, linhas = i ? altura/2 : altura;
      const plano = planos.subarray(pos,pos+largura*linhas); pos += plano.length;
      gl.activeTexture(gl.TEXTURE0+i); gl.bindTexture(gl.TEXTURE_2D,t);
      if (mudou) gl.texImage2D(gl.TEXTURE_2D,0,gl.LUMINANCE,largura,linhas,0,gl.LUMINANCE,gl.UNSIGNED_BYTE,plano);
      else gl.texSubImage2D(gl.TEXTURE_2D,0,0,0,largura,linhas,gl.LUMINANCE,gl.UNSIGNED_BYTE,plano);
    });
    if(!valido()) return 0;
    gl.drawArrays(gl.TRIANGLE_STRIP,0,4);
    delete canvas.dataset.erro;
    return Number(h.getBigUint64(8,true));
  };
  desenhar.fechar = () => {
    if (restaurado) restaurado.fechar();
    canvas.removeEventListener('webglcontextlost', perder);
    canvas.removeEventListener('webglcontextrestored', restaurar);
    texturas.forEach(t => gl.deleteTexture(t));
    gl.deleteBuffer(buffer); gl.deleteProgram(programa);
  };
  return desenhar;
}
