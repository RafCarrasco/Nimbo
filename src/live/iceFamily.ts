// Ordem das famílias de endereço ao assistir uma live enviada pelo app: IPv4 primeiro, IPv6 um
// instante depois.
//
// Medido em 23/11/2026 na rede do dono (PC direto no modem da operadora; IPv6 com endereço, mas sem
// volta da internet) contra a live de um amigo, com um espectador de teste em Chromium nesta ordem:
//   - resposta aplicada inteira: nenhum caminho respondeu (18 pedidos, 0 respostas), sempre;
//   - resposta sem os endereços IPv6 do transmissor: conectou em ~0,2 s, com vídeo;
//   - endereços IPv6 oferecidos 1,2 s depois: conectou igual (o IPv6 nem chegou a ser usado).
//
// Por quê: o navegador testa os pares IPv6 antes do endereço público IPv4. O pacote IPv6 chega ao
// motor de quem transmite, que entende que quem assiste já falou e passa a mandar por todos os caminhos
// com vida normal (ver `Quiet` em desktop/rust-media/src/engine.rs). O pacote IPv4 dele bate no
// roteador de quem assiste antes do primeiro pacote IPv4 de dentro, e há roteador que mata a conversa
// por isso (ver docs/rust-app.md). Com o IPv4 testado primeiro, quem abre o caminho IPv4 é quem assiste.

/** Espera antes de oferecer ao navegador os endereços IPv6 do transmissor. O primeiro teste do
 *  endereço público IPv4 sai em ~0,2 s; o resto é folga para computador lento. */
export const V6_AFTER_MS = 1000;

const isCandidate = (line: string) => line.startsWith('a=candidate:');
const addressOf = (line: string) => line.split(' ')[4] || '';
const isV6 = (line: string) => isCandidate(line) && addressOf(line).includes(':');
const isV4Udp = (line: string) =>
  isCandidate(line) && !addressOf(line).includes(':') && (line.split(' ')[2] || '').toLowerCase() === 'udp';

/** Separa a resposta do transmissor: o SDP sem os endereços IPv6 e, à parte, esses endereços, para
 *  oferecer ao navegador depois. Sem IPv4 na resposta não há o que pôr na frente: devolve como veio. */
export function v4First(sdp: string): { sdp: string; later: RTCIceCandidateInit[] } {
  const eol = sdp.includes('\r\n') ? '\r\n' : '\n';
  const lines = sdp.split(eol);
  if (!lines.some(isV4Udp) || !lines.some(isV6)) return { sdp, later: [] };
  // Cada endereço pertence a uma seção de mídia; o `a=mid` da seção pode vir depois dele.
  const mids: string[] = [];
  let section = -1;
  for (const line of lines) {
    if (line.startsWith('m=')) section += 1;
    else if (section >= 0 && line.startsWith('a=mid:')) mids[section] = line.slice('a=mid:'.length).trim();
  }
  const later: RTCIceCandidateInit[] = [];
  const kept: string[] = [];
  section = -1;
  for (const line of lines) {
    if (line.startsWith('m=')) section += 1;
    if (section >= 0 && isV6(line)) {
      later.push({ candidate: line.slice('a='.length), sdpMLineIndex: section, sdpMid: mids[section] ?? null });
      continue;
    }
    kept.push(line);
  }
  return { sdp: kept.join(eol), later };
}
