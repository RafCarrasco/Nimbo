// Prévia de uma live: uma foto pequena do que está sendo transmitido, para quem ainda não abriu a
// live ver do que se trata (como a prévia de transmissão do Discord).
//
// Quem transmite tira a foto da própria faixa de vídeo de tempos em tempos e grava no canal; quem
// assiste só lê quando a prévia vai aparecer na tela (cartões do palco ou mouse em cima de quem está
// ao vivo). Assim o gasto de dados do banco não cresce com o número de pessoas no canal.

// Largura da foto, em pixels. Pequena de propósito: é uma prévia, e precisa caber no banco.
const WIDTH = 256;
// Tamanho máximo do texto gravado (as regras do banco conferem o mesmo número).
export const PREVIEW_LIMIT = 14000;
// De quanto em quanto tempo quem transmite renova a foto, e a espera antes da primeira.
export const PREVIEW_EVERY_MS = 45000;
export const PREVIEW_FIRST_MS = 3000;

type FrameGrabber = { grabFrame(): Promise<ImageBitmap> };

// Foto atual da faixa, como data URL (WebP; JPEG onde não houver WebP). `null` quando o navegador
// não tem como tirar a foto sem um player na tela, ou quando a faixa não está entregando imagem.
export async function snapshotOf(track: MediaStreamTrack): Promise<string | null> {
  const Capture = (window as unknown as { ImageCapture?: new (track: MediaStreamTrack) => FrameGrabber }).ImageCapture;
  if (!Capture || track.readyState !== 'live') return null;
  const bitmap = await new Capture(track).grabFrame();
  try {
    if (!bitmap.width || !bitmap.height) return null;
    const canvas = document.createElement('canvas');
    canvas.width = WIDTH;
    canvas.height = Math.max(1, Math.round((bitmap.height * WIDTH) / bitmap.width));
    const context = canvas.getContext('2d');
    if (!context) return null;
    context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    for (const quality of [0.5, 0.35, 0.2]) {
      let data = canvas.toDataURL('image/webp', quality);
      if (!data.startsWith('data:image/webp')) data = canvas.toDataURL('image/jpeg', quality);
      if (data.length <= PREVIEW_LIMIT) return data;
    }
    return null;
  } finally {
    bitmap.close();
  }
}

// Só imagens pequenas em data URL viram <img>: o que vem do banco não é confiável por si só.
export function validPreview(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length <= PREVIEW_LIMIT &&
    (value.startsWith('data:image/webp;base64,') || value.startsWith('data:image/jpeg;base64,'))
  );
}
