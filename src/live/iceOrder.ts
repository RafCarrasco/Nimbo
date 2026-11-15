// Quem manda o primeiro pacote quando duas pessoas procuram um caminho direto entre si.
//
// Alguns roteadores matam a conversa quando o primeiro pacote vem de fora. Medido em 15/11/2026 no
// modem da Vivo em modo roteador, contra um navegador num servidor fora de casa: com o lado de fora
// mandando primeiro, a conexão falhou; com o lado de dentro mandando primeiro, conectou (as duas
// rodadas no mesmo dia, no app e no Electron). O navegador não deixa escolher a hora do pacote, mas
// um lado só consegue mandar para os endereços que já recebeu. Então dá para decidir a ordem:
//
//  - 'me': seguro os meus endereços até já ter mandado testes para os do outro lado.
//  - 'them': demoro a aplicar os endereços do outro lado, para ele mandar primeiro.
//  - 'natural': sem interferir (na prática, quem responde à oferta manda primeiro).
//
// Cada lado age sozinho e as duas ações levam ao mesmo resultado, então funciona mesmo que o outro
// lado esteja numa versão antiga do site. Com os DOIS lados atrás de roteadores assim, nenhuma ordem
// serve: só o servidor de retransmissão (TURN) resolve.
export type IceOrder = 'me' | 'them' | 'natural';
export type IceSide = 'me' | 'them';

// Ordem da tentativa `attempt` (0 = primeira) com uma pessoa. A primeira é a de sempre, a não ser que
// esta rede já tenha mostrado que precisa mandar primeiro (`lead`). Depois de uma falha, as duas
// ordens são tentadas em sequência. `lower` (meu uid é o menor dos dois) faz os dois lados chegarem à
// mesma conclusão sem combinar nada.
export function orderFor(attempt: number, lower: boolean, lead: boolean): IceOrder {
  const turn = attempt % 3;
  if (turn === 1) return lower ? 'me' : 'them';
  if (turn === 2) return lower ? 'them' : 'me';
  return lead ? 'me' : 'natural';
}

// Quem manda primeiro numa rodada. Na ordem natural é quem respondeu à oferta: ele já tem os
// endereços do outro lado quando cria a resposta, e o outro ainda espera a resposta chegar.
export function leaderOf(order: IceOrder, answered: boolean): IceSide {
  if (order === 'natural') return answered ? 'me' : 'them';
  return order;
}

// O que uma conexão que deu certo ensina sobre esta rede: só conectou comigo mandando primeiro,
// depois de falhar com o outro lado mandando primeiro → esta rede precisa mandar primeiro. O
// contrário desfaz a anotação (a rede mudou, ou era a do outro lado).
export function lesson(leader: IceSide, failed: ReadonlySet<IceSide>): 'lead' | 'free' | null {
  if (leader === 'me' && failed.has('them')) return 'lead';
  if (leader === 'them' && failed.has('me')) return 'free';
  return null;
}

const KEY = 'nimbo.ice.lead';
// A anotação vale por 30 dias: trocar de roteador ou de operadora não pode deixar marcado para sempre.
const KEEP_MS = 30 * 24 * 3600 * 1000;

// Esta rede precisa mandar primeiro? (aprendido numa conexão anterior, neste aparelho)
export function mustLead(now = Date.now()): boolean {
  try {
    return Number(localStorage.getItem(KEY)) > now;
  } catch {
    return false;
  }
}

export function remember(result: 'lead' | 'free' | null, now = Date.now()) {
  try {
    if (result === 'lead') localStorage.setItem(KEY, String(now + KEEP_MS));
    else if (result === 'free') localStorage.removeItem(KEY);
  } catch {
    // Preferência apenas local.
  }
}

// Para testes: `sessionStorage['nimbo.iceOrder'] = 'me' | 'them' | 'natural'` força a ordem só nesta aba
// (ou `localStorage`, para todas).
export function forcedOrder(): IceOrder | null {
  try {
    const value = sessionStorage.getItem('nimbo.iceOrder') || localStorage.getItem('nimbo.iceOrder');
    return value === 'me' || value === 'them' || value === 'natural' ? value : null;
  } catch {
    return null;
  }
}

// Oferta ou resposta sem os endereços de rede (eles seguem depois, um a um).
export function withoutCandidates(desc: RTCSessionDescriptionInit): RTCSessionDescriptionInit {
  const sdp = (desc.sdp || '')
    .split('\n')
    .filter((line) => !/^a=(candidate:|end-of-candidates)/.test(line))
    .join('\n');
  return { type: desc.type, sdp };
}

// O identificador da rodada de rotas dentro de uma oferta ou resposta (muda a cada "rota nova").
export function iceUfrag(sdp: string | undefined): string {
  return /^a=ice-ufrag:(\S+)/m.exec(sdp || '')?.[1] || '';
}

// Tempos, em ms.
// Segurando os próprios endereços: espera mínima desde o primeiro endereço do outro lado (os demais
// endereços dele chegam logo em seguida) e teto, caso algum teste nunca saia.
export const HOLD_MIN_MS = 700;
export const HOLD_MAX_MS = 2500;
// Esperando o outro lado mandar primeiro: quanto tempo depois de a oferta e a resposta estarem
// aplicadas os endereços dele passam a valer aqui.
export const WAIT_MS = 2000;
// Teto de qualquer espera, contado do começo da rodada (por exemplo, os dois lados segurando).
export const ROUND_MAX_MS = 5000;
// Duas "rotas novas" nesse intervalo são a mesma rodada (os dois lados pediram ao mesmo tempo).
export const SAME_ROUND_MS = 1500;
