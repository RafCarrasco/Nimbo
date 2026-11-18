// Novidades mostradas uma vez depois de atualizar (mais nova primeiro). Ao publicar uma versão,
// acrescente a entrada dela aqui com a mesma versão do package.json.
export type Release = { version: string; date: string; items: string[] };

export const CHANGELOG: Release[] = [
  {
    version: '0.5.7',
    date: '18/11/2026',
    items: [
      'Transmitir a tela inteira com o som do computador não leva mais as vozes da chamada: o som do Discord, TeamSpeak, Teams, Zoom e parecidos fica de fora da live, como no Discord.',
      'Cargos no estilo do Discord: botão direito na pessoa para dar cargo, janela de configurações do canal e da categoria, e um editor de cargos. Só o dono vê os cargos.',
      'Dar ou tirar um cargo não derruba mais quem está nas lives.',
      'O aviso de versão nova confere a atualização do app na hora, e o botão de atualizar não fica mais parado sem fazer nada.',
    ],
  },
  {
    version: '0.5.6',
    date: '15/11/2026',
    items: [
      'Lives de tela pelo app agora saem a 60 quadros por segundo de verdade. Antes, numa tela de 144 Hz a live saía a 48, e numa de 75 Hz a 37: o app descartava quadros a mais.',
      'Em cima de cada live: há quanto tempo está no ar, quem está assistindo e a resolução e os quadros que estão chegando.',
      'Janela flutuante: tire a live para uma janelinha que fica por cima dos outros programas (botão ao lado da tela cheia). Duplo clique na live abre a tela cheia.',
      'Prévia das lives: o palco mostra um cartão com a foto de cada live do canal, e parar o mouse em cima de quem está ao vivo mostra a prévia, mesmo antes de entrar no canal.',
      'Aviso com o botão Assistir quando alguém começa uma live no canal em que você está.',
      'Webcam e lives de quem transmite pelo site passam a conectar em redes com modem que barra conexão iniciada de fora: depois de uma falha, o Nimbo tenta de novo invertendo quem começa.',
    ],
  },
  {
    version: '0.5.5',
    date: '15/11/2026',
    items: [
      'Quem assiste com internet mais fraca agora vê a live em resolução menor, em vez de imagem parada: o app de quem transmite baixa para 720p, 540p ou 360p quando a rede aperta e volta a subir quando melhora.',
      'A qualidade para de subir e descer toda hora em rede instável: o app mede quanto realmente chega a quem assiste e segura a taxa abaixo disso.',
      'A mesma conta aberta em dois lugares (app e navegador, ou dois aparelhos) não derruba mais a live: a sessão antiga sai do canal e avisa.',
    ],
  },
  {
    version: '0.5.4',
    date: '15/11/2026',
    items: [
      'Lives entre casas: o app de quem transmite abre o caminho de volta na hora em que alguém pede a live, e com o alcance mínimo para a rede de cada um. Ajuda quem assiste atrás de modem que barra conexão iniciada de fora.',
    ],
  },
  {
    version: '0.5.3',
    date: '15/11/2026',
    items: [
      'Botão para ocultar a barra da esquerda e deixar a live com a janela inteira (ao lado do nome do canal).',
      'Quando uma live não conecta porque o app de quem transmite está desatualizado, o aviso diz isso e o que fazer.',
      'O app procura atualização a cada 30 minutos (antes, a cada 6 horas) e avisa quando você está numa versão antiga.',
    ],
  },
  {
    version: '0.5.2',
    date: '15/11/2026',
    items: [
      'Lives que ficavam em "conectando" para sempre entre casas diferentes agora conectam: alguns roteadores barravam a conversa porque o app de quem transmite falava primeiro.',
      'Vale para quem transmite: é quem está transmitindo que precisa estar na 0.5.2.',
    ],
  },
  {
    version: '0.5.1',
    date: '14/11/2026',
    items: [
      'Lives entre casas diferentes voltam a conectar: o app novo procurava menos caminhos de rede que o antigo e alguns amigos deixaram de se ver.',
      'A live agora tenta todas as redes do computador: IPv6, rede de jogo (Radmin, Hamachi) e segunda placa.',
    ],
  },
  {
    version: '0.5.0',
    date: '14/11/2026',
    items: [
      'O Nimbo agora é um app novo, feito do zero: instalador de 5 MB e bem mais leve aberto.',
      'Sua live é preparada uma vez só para todo mundo que assiste: plateia maior não pesa mais no seu jogo.',
      'Imagem em 1080p nas cenas de muito movimento, onde antes a resolução caía.',
      'Tela nova para escolher o que transmitir, com prévia de cada tela, janela e jogo.',
    ],
  },
  {
    version: '0.4.1',
    date: '06/11/2026',
    items: [
      'Fechou o jogo? A live dele encerra sozinha, com um aviso.',
      'Live presa procurando caminho de rede tenta uma rota nova sozinha, e o aviso mostra "conectando" em vez de "sem imagem".',
    ],
  },
  {
    version: '0.4.0',
    date: '05/11/2026',
    items: [
      'Foto de perfil: em Seu perfil, escolha uma imagem ou use a foto do Google. Todo mundo vê no lugar da letra.',
      'Lives estáveis com o app atrás do jogo ou minimizado: o Windows freava o Nimbo em segundo plano e quem entrava via "Conectado, mas sem imagem".',
      'Botão "Copiar diagnóstico" quando uma live trava: cole para quem cuida do Nimbo e o problema aparece na hora.',
    ],
  },
  {
    version: '0.3.9',
    date: '04/11/2026',
    items: [
      'O seletor de transmissão não some mais minimizado deixando o Nimbo travado: ele sempre volta para a frente.',
      'Um aviso só de atualização no app (antes vinha um do site e depois outro do app).',
      'Quem tem o app instalado não recebe mais o aviso de baixar de novo: a atualização chega sozinha.',
      'Ao entrar num canal, uma live que não carregou reconecta em segundos.',
    ],
  },
  {
    version: '0.3.8',
    date: '04/11/2026',
    items: [
      'Atualizar ficou de verdade: o selo mostra o download, o clique abre a tela "Atualizando o Nimbo", o app instala sem janela e reabre sozinho.',
      'Depois de atualizar, o Nimbo abre direto com as novidades da versão nova.',
      'Se a instalação não der certo, o app avisa e oferece o instalador, em vez de pedir para atualizar de novo.',
      'App antigo (que não se atualiza sozinho) recebe um aviso para baixar o novo.',
    ],
  },
  {
    version: '0.3.7',
    date: '04/11/2026',
    items: [
      'Jogo aberto sem janela visível (Valorant com anticheat, por exemplo) aparece em Aplicativos pelo nome do jogo: transmite a tela dele, com o som só do jogo.',
      'Janelas sem título também entram na lista, com o nome do programa.',
      'Botão "Não achou o jogo?" no topo do seletor, nas duas abas, para ver o diagnóstico da captura.',
    ],
  },
  {
    version: '0.3.6',
    date: '04/11/2026',
    items: [
      'Jogos que o Windows esconde da lista (Valorant e outros com anticheat) agora entram em Aplicativos.',
      'Novo "Não achou o jogo?" no seletor: mostra o que o Nimbo enxergou, com botão de copiar.',
      'Na aba Telas, a dica para transmitir o monitor do jogo quando ele não aparece.',
    ],
  },
  {
    version: '0.3.5',
    date: '04/11/2026',
    items: [
      'Jogos com anticheat (Valorant, por exemplo) aparecem no seletor, mesmo em tela cheia ou minimizados.',
      'Se o Windows não deixar pegar só o som do jogo, a live usa o som do PC (sem o Nimbo) em vez de ficar muda.',
      'Rodapé mais limpo, só com a versão do Nimbo.',
    ],
  },
  {
    version: '0.3.4',
    date: '02/11/2026',
    items: [
      'Qualidade automática: se a rede de quem assiste apertar, a live baixa a qualidade para aquela pessoa em vez de travar, e volta sozinha quando melhora.',
      'Saiu do jogo durante a live? Quem assiste vê a tela "Pausado · volta já" em vez da sua área de trabalho.',
      'Selo verde "Atualização disponível" no topo: um clique e pronto.',
      'Esta janela de novidades, que aparece uma vez depois de cada atualização.',
      'Versões novas do site aparecem na hora, sem precisar limpar cache.',
    ],
  },
  {
    version: '0.3.3',
    date: '02/11/2026',
    items: ['Ícone novo do Nimbo em todo lugar.', 'Versão no título da janela do app.'],
  },
  {
    version: '0.3.2',
    date: '02/11/2026',
    items: [
      'Jogo em tela cheia de verdade: escolha o jogo e o Nimbo transmite o monitor dele, com o som só do jogo.',
      'Aviso de atualização com a cara do Nimbo e versão no rodapé.',
    ],
  },
];

const parse = (v: string) => v.split('.').map((n) => Number(n) || 0);
export function newer(a: string, b: string) {
  const [x, y] = [parse(a), parse(b)];
  for (let i = 0; i < 3; i++) if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) > (y[i] || 0);
  return false;
}
