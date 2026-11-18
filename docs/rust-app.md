# Nimbo em Rust: plano

Objetivo: o app de Windows deixa de ser Electron e passa a ser Rust, sem perder nada do que o
app atual faz. Este documento diz o que muda, o que ganha e em que ordem.

## O que foi medido (11/11/2026, i5-14600KF + RTX 5070)

| Cenário (1080p, muito movimento, teto de 8 Mbps) | Codificador | Resolução entregue | CPU do app |
|---|---|---|---|
| App atual, padrão (VP8) | software | 480x270 | ~3% |
| App atual com H.264 da placa de vídeo | NVIDIA | 1280x720 | ~3% |

Primeira versão do motor em Rust (`desktop/rust-media`, etapas 1 e 2), mesma cena, tela 1920x1080:

| Medida | Rust | App atual (Electron, H.264 da placa) |
|---|---|---|
| Só captura | 48 quadros/s, ~0% de CPU | não medido separado |
| Captura + H.264 pela placa, 8 Mbps | 48 quadros/s em 1920x1080, 4% a 25% de um núcleo (3 rodadas) | 48 quadros/s em 1280x720, ~3% de um núcleo |
| Tamanho do executável | 0,4 MB | ~100 MB (instalador) |

Os 48 quadros/s são o limite da cena de teste, não do capturador. O Rust manteve 1080p cheio, mas
**ainda não gasta menos CPU que o Electron**: o codificador usado nesta etapa (MediaTranscoder do
Windows) converte cor na CPU. Trocar por Media Foundation direto na textura da GPU é parte da etapa 2.

Envio por WebRTC (`nimbo-media serve`, etapas 3 e 4), navegadores assistindo a tela enviada pelo Rust:

| Espectadores | Quadros recebidos por cada um | Pacotes perdidos | CPU do Rust (um núcleo) |
|---|---|---|---|
| 1 | 24/s em 960x540 | 0 | ~25% |
| 4 | 24/s em 960x540 | 0 | ~27% a 32% |

Um codificador só serviu os quatro: o custo quase não cresceu com a plateia. A resolução e os
quadros estão baixos porque esta versão ainda codifica por software (OpenH264) e copia cada quadro
da GPU para a memória; a codificação pela placa substitui isso.

Com a codificação pela placa direto na GPU (`hw.rs`, sem copiar o quadro para a memória) e controle de
banda, mesma cena, 1920x1080 a 8 Mbps:

| Espectadores | Rust: recebido por cada um | Rust: CPU (um núcleo) | Electron: recebido | Electron: CPU (um núcleo) |
|---|---|---|---|---|
| 1 | 1920x1080, 48 quadros/s, 0 perdidos | ~1% a 3% | 1280x720, 47 quadros/s | 2,6% |
| 5 | 1920x1080, 48 quadros/s, 0 perdidos | ~0,3% a 3% (7% nos primeiros 5 s) | 1280x720, 47 quadros/s | 6,4% |
| 8 | 1920x1080, 48 quadros/s, 0 perdidos | não medido isolado | - | - |

- Codificar um quadro custa 0,25 ms de CPU; nenhum quadro foi descartado.
- O Electron usa um codificador da placa por espectador e entregou 720p; o Rust usa um só e entregou 1080p.
- A taxa de bits muda com a live no ar (pedido de 8 para 2 Mbps: a média caiu de 6,4 para 3,9 Mbps em 4 s).
- Os 48 quadros/s são o limite da cena de teste. O número do Electron inclui desenhar a cena de teste.
- Não medido: uso da GPU, máquinas Intel/AMD, rede real com perda.
- Em 14/11, com som e as correções da revisão: 5 espectadores em 1920x1080 a ~40 quadros/s, som sem perda,
  6% a 14% de um núcleo. A banda alvo ficou em 8 Mbps depois de 12 s de tela parada (antes caía para 1,3 Mbps).

Conclusões:

1. Em máquina forte o CPU não é o gargalo; a qualidade por megabit é. O H.264 pela placa de vídeo
   já entrou no app atual (PR #5).
2. O custo que cresce de verdade é **um codificador por espectador**. O WebRTC do navegador
   codifica o mesmo vídeo de novo para cada amigo que assiste. Com 5 amigos são 5 codificações.
   É isso que pesa em máquina fraca e é isso que só um motor próprio resolve.

## Onde o Rust ganha

| Parte | Hoje (Electron) | Em Rust | Ganho |
|---|---|---|---|
| Codificação | 1 por espectador | **1 para todos** | CPU/GPU não cresce com a plateia |
| Captura | Chromium copia quadros entre processos | Windows Graphics Capture direto na GPU | Menos cópia, menos atraso |
| Áudio do jogo | Programa auxiliar em C# | WASAPI no mesmo processo | Um programa a menos |
| Instalador | ~100 MB | ~10 a 15 MB | Download e atualização rápidos |
| Memória parado | ~250 MB | ~80 MB | |

O que **não** muda: a interface continua sendo HTML/CSS (a mesma do site), desenhada pelo
WebView2 que já vem no Windows. Reescrever botões em Rust não deixaria nada mais rápido e
duplicaria todo o trabalho entre site e app.

## Arquitetura

```
┌──────────────────────── Nimbo.exe (Rust, Tauri 2) ────────────────────────┐
│  WebView2: interface do site (login, canais, assistir lives)              │
│      │  comandos (IPC)                    ▲ eventos                        │
│      ▼                                    │                                │
│  nimbo-media (Rust)                                                        │
│    captura  → Windows Graphics Capture (janela, tela, jogo)                │
│    vídeo    → codificador H.264 da placa (Media Foundation), 1 só          │
│    áudio    → WASAPI process loopback → Opus                               │
│    envio    → WebRTC em Rust (str0m): 1 conexão por espectador,            │
│               todas recebendo os mesmos pacotes já codificados             │
│  janela, seletor de tela, vigia do jogo, atualização, login                │
└────────────────────────────────────────────────────────────────────────────┘
```

- **Quem assiste não muda.** Site, celular e app continuam recebendo WebRTC normal.
- **Sinalização não muda.** A interface continua falando com o Firebase; ela só repassa a
  oferta/resposta e os candidatos ICE para o motor em Rust em vez de para o navegador.
- **Assistir** continua no WebView2 (decodificação pela placa de vídeo já acontece lá).
- **TURN, trava de gasto e regras do banco** continuam os mesmos.

## Por que não deu certo antes

A primeira tentativa (`refactor/nimbo-rust-migration`) reescreveu a interface em Rust/WASM e
deixou a captura por conta do WebView2. O WebView2 não deixa o app escolher a janela nem separar
o áudio do jogo, então CS e Valorant voltariam a falhar. Aqui é o contrário: a interface fica, e
o Rust assume justamente a captura e o envio.

## Etapas

Em 14/11/2026 o app em Rust virou o Nimbo oficial (versão 0.5.0): é o que o link de download entrega, e o
instalador dele remove o app antigo em Electron. Estado:

| # | Entrega | Estado |
|---|---|---|
| 1 | Captura nativa de tela/janela | **Feito** (`desktop/rust-media`) |
| 2 | Codificação H.264 pela placa, uma só | **Feito**: direto na GPU; o quadro sai ~3 ms depois de entregue |
| 3 | Envio WebRTC em Rust | **Feito**: 1080p, com controle de banda |
| 4 | Vários espectadores com 1 codificador | **Feito**: 8 abas em 1080p, nenhum pacote perdido |
| 5a | Sinalização: o site conecta quem assiste ao motor de quem transmite | **Feito** (`src/live/native.ts`), revisado e corrigido |
| 5b | Janela do app (Tauri) com a interface do site e a ponte | **Feito** (`desktop/rust-app`): live de verdade testada no emulador |
| 6a | Som do jogo, capturado e enviado junto com o vídeo | **Feito** (`desktop/rust-audio` + Opus no motor): quem assiste recebe a faixa de áudio |
| 6b | Lista de janelas, tela cheia e vigia do jogo | **Feito** (`desktop/rust-native`) |
| 6c | Seletor "o que transmitir", com miniaturas e ícones | **Feito** (`src/components/SourcePicker.tsx`) |
| 6d | Pausa (tela escura para quem assiste) e fim da live quando o jogo ou a janela fecha | **Feito no motor e no app**; a pausa foi vista por quem assiste. Não testado com jogo de verdade |
| 6e | Login pelo navegador | **Feito** com credenciais de teste; falta um login real do Google |
| 7a | Instalador e atualização automática assinada | **Feito**: instalado numa máquina com o app antigo aberto (fechou, removeu, trocou os atalhos); ciclo de atualização testado localmente |
| 7b | Troca: o app em Rust vira o Nimbo | **Feito em 14/11/2026** (0.5.0). Quem está no app antigo vê no site o convite para baixar o novo |

### O que ainda não foi testado com gente de verdade

1. Jogo de verdade: som, tela cheia, Alt+Tab, anticheat (CS2, Valorant).
2. Alguém em outra rede e o servidor de retransmissão (TURN).
3. ~~Login real do Google no app novo~~: funcionou em 14/11/2026.
4. A primeira atualização pelo GitHub (o ciclo foi testado com um servidor local): a 0.5.1 é a primeira.
5. Placas Intel e AMD, e máquina sem codificador na placa.

Se algo disso falhar feio, o caminho de volta é publicar de novo o instalador do Electron (Release 0.4.1) como
"latest" e tirar o convite do site (`LEGACY_APP` em `src/components/UpdateBanner.tsx`). O código do app antigo
continua em `desktop/main.cjs` por isso.

### Caminhos de rede (corrigido na 0.5.1)

No primeiro dia da 0.5.0, duas pessoas que se viam no app antigo deixaram de se ver (`connection failed`, nenhum
candidato remoto aproveitado). Medido na mesma máquina: o WebView2 oferecia 2 endereços (um nome `.local` e o
endereço público) contra 9 do Electron (todas as placas, IPv6, rede virtual de jogo). Do outro lado, o motor só
oferecia a rota padrão em IPv4. Duas correções:

- **Casca** (`desktop/rust-app/src/network.rs`): o Chromium só libera todos os endereços para a página que tem
  permissão de câmera, então o app concede a câmera à origem do Nimbo no perfil do WebView2, e desliga a troca do
  endereço local por nome `.local`. Depois disso: 9 endereços, igual ao Electron.
- **Motor** (`accept` em `desktop/rust-media/src/engine.rs`): um soquete por endereço local (até 6 IPv4 e 4
  IPv6), cada um com seu candidato. Conferido mantendo um candidato por vez na resposta: segunda placa, rede
  virtual e IPv6 conectam e passam vídeo.

Quem está atrás de CGNAT dos dois lados, sem IPv6 e sem rede virtual em comum, continua precisando de TURN.

Medido em 14/11/2026 com um espectador remoto de verdade (Chrome num servidor do GitHub Actions, sinalização
cifrada pela branch `test/remote-viewer`):

- **Motor pela internet funciona**: espectador com NAT amigável conectou em 0,4 s pelo endereço público dos dois
  lados e recebeu vídeo (155 quadros em 5 s).
- **Rede difícil não conecta nem no app antigo**: com o servidor simulando NAT simétrico (porta pública que muda a
  cada destino), a conexão falhou tanto no WebView2 quanto no Electron que tem regra de entrada no Firewall do
  Windows. A regra de firewall do app antigo não era o que fazia a diferença; para esses casos só o TURN resolve.
- O app antigo ganhava uma regra de entrada no firewall (o Windows perguntava na primeira vez); o novo não tem.
  Não mudou o resultado nos testes acima.

### Quem manda o primeiro pacote (corrigido na 0.5.2)

A 0.5.1 não resolveu o caso que a motivou. A causa de verdade, medida em 15/11/2026 na rede do dono (dois
roteadores em série em casa: o dele e o modem da operadora) contra o servidor de teste:

| Quem manda primeiro | Resultado |
| --- | --- |
| O lado de fora (como o motor fazia ao aceitar a oferta) | 0 pacotes passam, nos dois sentidos, mesmo depois de o lado de dentro mandar: a conversa fica morta |
| O lado de dentro | 90 de 90 |
| O lado de dentro, só com pacotes de vida curta (TTL 3 ou 4) que morrem no provedor | 90 de 90, e o lado de fora não é alcançado antes da hora |
| Idem com TTL 2 | 0: não atravessa os dois roteadores de casa |

No app antigo quem assistia testava os caminhos primeiro; no motor, quem transmite respondia à oferta e já saía
testando, então o pacote dele chegava ao roteador de quem assiste antes do primeiro pacote de quem assiste. A
versão do app, a regra de firewall e o Radmin (desligado na máquina dele desde 11/10) não tinham relação.

Correção (`Quiet` em `desktop/rust-media/src/engine.rs`): até o espectador mandar o primeiro pacote, ou 1,5 s,
o que sai do motor leva 4 saltos de vida e vai no máximo um pacote a cada 300 ms por destino. Isso abre a
passagem nos roteadores de quem transmite sem alcançar o roteador de quem assiste. Conferido com o motor de
verdade e silêncio de 8 s (`NIMBO_QUIET_MS=8000`): o espectador remoto mandou primeiro e a conexão fechou em
0,4 s, com vídeo. Na rede local o pacote chega direto e o silêncio acaba na hora.

**0.5.4, depois de a 0.5.2 e a 0.5.3 não resolverem o caso do dono.** O que se soube em 15/11/2026:

- O aparelho que mata a conversa é o modem da Vivo (192.168.15.1), também sozinho, sem o roteador do dono
  atrás (medido pelo Wi-Fi direto do modem: de fora primeiro, 0 de 90; de dentro primeiro, 90 de 90). O técnico
  tinha tirado o modem do modo bridge na véspera: por isso "antes funcionava".
- Pelo Wi-Fi do modem a live do amigo conecta porque os dois lados têm IPv6; pelo cabo, atrás do roteador do
  dono, não há IPv6 e sobra o IPv4 que o modem barra.
- A rede de todos no canal tem porta pública estável e igual à local (lido das ofertas que cada app mandou para
  uma live de teste): ninguém ali precisa de TURN por causa de NAT simétrico.
- Defeitos do silêncio da 0.5.2/0.5.3: 4 saltos para todo mundo é um a mais que o necessário para quem tem um
  roteador só (o pacote anda um salto mais longe e pode alcançar o modem de quem assiste), e a abertura dependia
  da fila de testes do WebRTC (um a cada 50 ms, o endereço público por último). Agora o motor mede os roteadores
  de casa (`tracert` até 1.1.1.1, endereços privados ou de CGNAT no começo da rota) e usa esse número mais um; e
  manda um pacote de abertura para cada endereço de quem assiste já ao aceitar a oferta, antes de a resposta sair.
  Conferido com espectador remoto: mediu 2 roteadores pelo cabo e 1 pelo Wi-Fi, e conectou em 0,5 s.
- Não foi conferido entre o dono (pelo cabo) e o amigo: precisa de quem transmite na 0.5.4.
- Nesta máquina, UDP entre dois soquetes em 127.0.0.1 não é entregue (filtro de rede local): testes de rede usam
  o endereço da placa.

### Lives pelo navegador: quem manda o primeiro pacote (site, 15/11/2026)

O silêncio do motor só vale para as lives de tela do app. Webcam e quem transmite pelo site usam a conexão do
navegador, onde quem responde à oferta acaba testando os caminhos primeiro. Com quem transmite atrás de um
modem como o da Vivo, o primeiro pacote chegava de fora e a conversa morria.

O navegador não deixa escolher a hora do pacote, mas um lado só consegue mandar para os endereços que já
recebeu. Medido em 15/11/2026 com o PC do dono ligado direto no modem da Vivo (modo roteador) e um Chrome num
servidor do GitHub Actions, no app (WebView2) e no Electron:

| Como a oferta sai daqui | Resultado nos dois programas |
| --- | --- |
| Com os endereços (o lado de fora testa primeiro) | falhou |
| Sem os endereços; eles seguem 1,5 s depois de a resposta ser aplicada (este lado testa primeiro) | conectou, IPv4 público dos dois lados |

Também medido: pedir "rota nova" no Chromium abre portas novas (61338/61339 viraram 61340/61341), então uma
conversa morta numa tentativa não contamina a seguinte.

O que o site faz agora (`src/live/iceOrder.ts`, usado em `src/live/room.ts`):

- **Primeira tentativa**: como sempre foi, a não ser que este aparelho já tenha aprendido que a rede dele
  precisa mandar primeiro.
- **Depois de uma falha**, as duas ordens são tentadas em sequência. Quem manda primeiro segura os próprios
  endereços até já ter mandado testes para os do outro lado (confere pelos números do próprio navegador, com
  mínimo de 0,7 s e teto de 2,5 s); o outro lado espera 2 s antes de aplicar os endereços recebidos. Cada lado
  age sozinho e as duas ações dão no mesmo, então funciona com o outro lado em versão antiga do site.
- **Aprendizado**: se só conectou com este lado mandando primeiro, depois de falhar do outro jeito, o aparelho
  guarda isso por 30 dias (`nimbo.ice.lead` no armazenamento local) e passa a mandar primeiro já na primeira
  tentativa. O contrário apaga a anotação.
- O "Copiar diagnóstico" mostra a ordem da rodada atual (`firstPacket`).

No emulador, com duas contas e uma webcam de teste, as quatro combinações conectaram: um lado segurando, um
lado esperando, os dois esperando (2 s) e os dois segurando (libera sozinho em 5 s).

Não resolve: os **dois** lados atrás de roteadores assim (nenhuma ordem serve; só o servidor de retransmissão)
e NAT simétrico. Não medido: a correção rodando no site entre duas casas de verdade (a bancada mediu o
mecanismo com uma página de teste, não o `room.ts`), Firefox e Safari, e celular.

### Qualidade com rede apertada (0.5.5)

Bancada de 15/11/2026: janela de teste com movimento fora da tela (1080p, ~48 quadros por segundo), o servidor
de teste do motor capturando essa janela (`NIMBO_SERVE_WINDOW=<hwnd>`), e um Chrome num servidor do GitHub
Actions (EUA, 170 ms de ida e volta) assistindo 45 s e medindo a cada 5 s. O lado remoto piora a própria rede
com regras de filtro: `loss` descarta 3% do que chega; `bw` deixa passar 300 pacotes grandes por segundo (perto
de 3 Mbps em 1080p).

| Cenário | Antes | Depois |
| --- | --- | --- |
| Rede limpa | 1080p, 48 q/s, 6,5 Mbps, sem perda, buffer de 8 a 10 ms | igual |
| 3% de perda | 1080p, 48 q/s, reenvios (~180 a cada 5 s), travadas de 0,2 s | igual |
| Banda limitada | **0 q/s**, 40% de perda, imagem parada | 720p, ~24 q/s, 2,2 a 2,6 Mbps, perda de 0 a 6% |

O que estava errado com banda limitada: o controle de banda pedia 0,8 Mbps, mas o codificador da placa não
comprime 1080p com movimento abaixo de ~4 Mbps e continuava produzindo isso (`produzido_mbps` no servidor de
teste mostra o alvo e o produzido lado a lado). Duas mudanças em `adapt` (`desktop/rust-media/src/engine.rs`):

- **Escada de qualidade** (`LADDER`): 1080p, 720p, 720p a 30 q/s, 540p a 30, 360p a 30. Desce direto para o
  degrau que a taxa pedida comporta depois de 2 s apertado (ou um degrau, se é o codificador que não acompanha);
  sobe um degrau por vez depois de 6 s com folga, e essa espera dobra quando a subida não se sustenta.
- **Freio pela perda medida**: a cada segundo a conexão informa a perda e os bytes enviados a cada espectador.
  Com 10% ou mais de perda, o que chegou é o enviado menos o perdido; a taxa fica presa 15% abaixo disso por
  30 s (dobra a cada recaída, até 4 min) e também não desce além de 60% desse teto. Sem isso a estimativa de
  banda ia de 0,8 a 3,7 Mbps a cada ~10 s e a imagem travava de 1 a 5 s em cada volta.

O codificador é um só para todos: um espectador com rede apertada baixa a resolução de todos. Dois níveis de
qualidade continuam sendo a saída para isso. Não medido nesta rodada: fila de quadros por espectador (em rede
limpa o buffer de quem assiste fica em 8 a 10 ms, sem sinal de acúmulo) e intervalo entre quadros completos
(com 3% de perda nenhum pedido de quadro completo apareceu; os reenvios bastaram).

A queda periódica a cada ~18 s vista no mesmo dia não era rede nem motor: eram duas sessões da mesma conta no
canal (app e uma aba de navegador), corrigido no site com `members/{uid}/sid`.

Limites: as lives pelo navegador embutido (webcam) e quem assiste pelo site não passam pelo motor (para elas,
ver "Lives pelo navegador: quem manda o primeiro pacote"); e é quem transmite que precisa estar na 0.5.2. O relógio da máquina do dono
estava 111 s adiantado: testes com horário combinado entre máquinas precisam medir essa diferença antes.

O "Copiar diagnóstico" de uma live nativa agora traz, por tentativa: os caminhos que cada lado ofereceu (só o tipo
de endereço), se a resposta do app de quem transmite chegou e em quanto tempo, o tipo de NAT de quem assiste, os
testes de caminho e a versão do app de quem transmite (gravada na presença do canal).

### Teto de quadros por segundo (0.5.6)

O teto de 60 quadros por segundo era aplicado pela captura do Windows (`MinimumUpdateInterval` de 1/60 s), que
descarta o quadro que chega antes do intervalo. Numa fonte mais rápida que o teto isso dá um divisor da taxa da
fonte: 75 Hz sai a 37,5 (um sim, um não) e 144 Hz a 48, que é o "48 quadros por segundo" de todas as medições
anteriores deste documento (não era o limite da cena de teste). O teto de 30 dos degraus baixos da escada tinha
o mesmo defeito.

Agora a captura entrega até o dobro do teto e o ritmo é feito no motor (`Pacer` em
`desktop/rust-media/src/engine.rs`): passa o primeiro quadro depois de cada batida de um relógio próprio, então
a média fica no teto, com intervalos um pouco desiguais (em 75 Hz, quatro quadros em cada cinco).

Medido em 15/11/2026, janela de teste a 75 quadros por segundo em 1920x1032, um navegador assistindo ao
`nimbo-media serve`, as duas versões do motor na mesma bancada:

| Medida | Antes | Depois |
| --- | --- | --- |
| Quadros por segundo codificados | 37,5 | 60,0 |
| Quadros por segundo recebidos pelo navegador | 37 a 38 | 60 |
| Pacotes perdidos | 0 | 0 |
| Taxa produzida | 5,0 Mbps | 8,1 Mbps (o teto de 8) |
| CPU do motor com um espectador (um núcleo) | 2,7% (0 a 7) | 5,1% (3,1 a 7,8) |
| CPU do motor sem espectador | 1,9% | 1,5% (0,8 a 3,1) |

Mais quadros custam mais banda e um pouco mais de CPU; por quadro, a taxa ficou igual (133 e 135 kb). Não medido:
tela de 144 Hz de verdade (o teste de unidade cobre 72 a 240 Hz, e a conta dá 60), jogo de verdade, e o teto de
30 dos degraus baixos numa live real.

### Defeitos conhecidos e otimizações pendentes

- **CPU do envio subiu depois das correções**: com 5 espectadores e som, 6% a 14% de um núcleo (antes, sem som e
  com o defeito do quadro atrasado, 0,3% a 3%). O motor consulta a saída do codificador a cada 0,5 ms; esperar pelo
  aviso do próprio codificador resolve.
- **Um espectador com rede ruim segura a qualidade de todos** (codificador único). Saída planejada: dois níveis.
- **Sem retransmissão própria**: o motor não usa TURN; quem assiste usa o dele. Não testado fora da rede local.
- **Cor da tela de pausa** chega um pouco diferente da cor do site (conversão de cor na placa).
- **Só NVIDIA foi testado.** Intel, AMD e a reserva por software compilam, mas não rodaram.

## Como as peças se ligam

- **Quem transmite pelo app em Rust**: a página pede a captura ao motor (`media.start`) e anuncia a live
  com `native: true`. Para cada pessoa que quer assistir chega uma oferta WebRTC pelo Firebase; a página
  entrega ao motor (`media.accept`) e devolve a resposta. A prévia local é só mais um espectador.
- **Quem assiste** (site, celular, app antigo ou novo): ao ver `native: true`, abre uma conexão só de
  recepção direto com o motor. As regras do banco não mudam: oferta e resposta usam o caminho de
  sinalização que já existe, com um campo `native`.
- **Som, o padrão**: no app, só o som do jogo ou programa escolhido ("Jogo ou janela"); ao transmitir a tela
  inteira, o som do computador. Era "Computador" até 15/11/2026, quando a voz de quem estava na chamada com quem
  transmitia entrou na live e a pessoa passou a se ouvir. Quem estava no padrão antigo é trocado uma vez ao abrir
  (`src/live/audioDefault.ts`).
- **Som**: o motor captura o som do computador (menos o do próprio Nimbo) ou só o do programa escolhido, codifica
  em Opus uma vez e manda na mesma conexão do vídeo. O anúncio da live leva `sound: true`, e só então quem assiste
  pede áudio.
- **Pausa e fim**: a página consulta o estado do app (`media.state`) uma ou duas vezes por segundo. Jogo em tela
  cheia que sai da frente pausa a live (o motor manda uma tela escura); jogo que fecha encerra a live.
- **Webcam** continua pelo navegador embutido, como hoje.

## Riscos

- **Controle de banda.** O navegador ajusta a qualidade sozinho; no motor próprio isso é nosso.
  Com um codificador para todos, a qualidade segue o espectador com a pior rede. Saída: dois
  níveis (alto e econômico), cada um com seu codificador.
- **Jogos em tela cheia exclusiva e anticheat.** O Windows Graphics Capture tem os mesmos limites
  do Chromium; não piora, mas também não resolve sozinho.
- **Máquina sem codificador na placa.** Precisa de um codificador por software (OpenH264) de reserva.
- **Troca de app.** O instalador novo fecha e remove o app antigo (`desktop/rust-app/installer-hooks.nsh`); o login
  e as preferências do antigo não são apagados, mas o app novo pede o login do Google uma vez. O app antigo não se
  atualiza sozinho para o novo: cada pessoa baixa o instalador pelo convite do site.
- **Um espectador com rede ruim segura a qualidade de todos**, porque o codificador é um só. Quem cai sem
  avisar deixa de contar depois de 2 s; rede ruim de verdade ainda afeta a todos. Saída planejada: dois níveis.
- **AdGuard e filtros de rede locais** cortam conexões HTTP locais ao acaso (visto nesta máquina). O app
  não depende de HTTP local para a mídia; o login pelo navegador usa e já trata esse caso.

## App full Rust (em construção desde 15/11/2026, branch `feat/full-rust`)

Em 15/11/2026 o dono liberou fazer o app inteiro em Rust, sem navegador embutido, "em outra branch" e
"validando bem antes". Enquanto isso o app oficial continua sendo o `desktop/rust-app` (o site dentro do
WebView2); o nativo só toma o lugar quando fizer tudo o que o atual faz. O site continua existindo para
navegador, celular e, se for o caso, para a administração.

Peças novas:

- `desktop/rust-core` (`nimbo-core`): miolo sem interface.
  - `rtdb.rs`: cliente do Realtime Database por WebSocket (protocolo 5, o do SDK da web): login da conexão,
    escutas, gravações, "ao desconectar", mensagens em pedaços, reconexão que refaz login e escutas.
  - `auth.rs`: login pela API REST do Firebase (credencial do Google → token; renovação).
  - `community.rs`: o mesmo que `useCommunity`/`usePeople`/`useRoomsPresence` do site (proprietário lê as
    listas; os demais, item a item) e as contas de quem vê o quê.
  - `room.rs`: entrar e sair de um canal (presença, queda e volta, troca de sessão da mesma conta) e a
    sinalização das lives nativas, igual à do site: caixa de entrada, oferta de quem assiste, resposta de
    quem transmite, anúncio da própria live (reanunciada quando a conexão volta).
  - Binário `nimbo-core`: `selftest` (confere tudo contra o emulador), `sidebar <conta>` (a barra lateral em
    texto) e `watch <conta> <caminho>`.
- `desktop/rust-ui` (`nimbo-native`): a janela, em egui 0.37 + wgpu. Cores e medidas copiadas de
  `src/style.css`, fonte Segoe UI lida do Windows, ícones lucide (os mesmos do site).
  - `live.rs`: quem assiste (uma conexão do player por live no palco, com as mesmas tentativas e prazos do
    `NativeReceiver` do site) e quem transmite (o motor responde a oferta de cada pessoa).
  - `stage.rs` e `video.rs`: o palco (etiquetas das lives, ladrilhos em grade) e o desenho do vídeo. Os
    dois planos do quadro (NV12) sobem para a placa e viram cor no desenho, num shader.
  - `picker.rs`: "O que você quer compartilhar?", com a lista de telas e janelas de `desktop/rust-native`.
- `desktop/rust-media`, módulo `viewer`: o player (receber, decodificar pela placa, tocar o som).

| # | Entrega | Estado |
|---|---|---|
| N1 | Cliente do banco e login no miolo | **Feito**: `nimbo-core selftest`, 17 passos contra o emulador com as regras de verdade (desde 17/11/2026 também plateia, prévia e administração) |
| N2 | Modelo da comunidade (níveis, cargos, quem vê o quê, presença) | **Feito**: conferido com 5 contas (proprietário, administrador, um membro de cada grupo, conta sem aprovação). Desde 17/11/2026 cargos são segredo do proprietário: quem não é proprietário não acompanha `roleDefs/{id}` nem `roleMembers/{id}` e não recebe nome, cor nem cadeado de cargo (conferido no `selftest` e nas fotos) |
| N3 | Janela: entrada, espera de aprovação, barra lateral, tela inicial | **Feito**, só com contas de teste do emulador; comparado com o site na mesma conta |
| N4 | Entrar e sair de canal | **Feito**: aparece para os outros com a versão do app, some ao fechar, e a sessão antiga sai quando a mesma conta entra em outro lugar |
| N5 | Player: receber, decodificar e tocar a live | **Feito** (`nimbo_media::viewer`). Contra o servidor de teste do motor, na mesma máquina: 48 q/s recebidos, decodificados e desenhados, sem perda; 0,17 ms por quadro sem cópia e 1,55 ms com a cópia para a memória; 3,3% e 7,3% de um núcleo; 71 e 92 MB. Não testado com perda de rede, fora da rede local, nem em placa Intel/AMD |
| N6 | Sinalização das lives no miolo (`native.ts` e o resto de `room.ts`) | **Feito para lives nativas** (as do motor), nos dois sentidos, no formato do site: anúncio da live (com `startedAt` na hora do servidor e `game`), oferta com código, resposta com o mesmo código, cada mensagem entregue uma vez e apagada, live reanunciada depois de uma queda; plateia (`watching/`) e prévias (`previews/`). Conferido no `selftest`, entre apps nativos e (a live) contra o site e o app atual em 15/11. Plateia e prévias **não foram conferidas com o site lendo** (o formato é o mesmo, conferido byte a byte no `selftest`). Falta atender as lives de navegador (N10) |
| N7 | Assistir dentro do app nativo (palco, várias lives, tela cheia) | **Feito**: as lives nativas abrem no palco (sozinhas ao entrar, pelo nome na barra ou pela etiqueta do topo), uma ou várias em grade, com som; "conectando", "não conectou" e "Tentar de novo". Desde 17/11/2026: tempo no ar, jogo, plateia (olho com número e nomes), "1032p · 60 fps", cartões "Ao vivo agora" com a prévia, prévia ao parar o mouse em quem está ao vivo na barra, aviso "Fulano começou uma live" com Assistir, tela cheia por duplo clique ou botão (Esc sai). A tela cheia de verdade (sem borda, cobrindo o monitor) **não foi testada**: os testes usam `--fullscreen-in-window` para não cobrir a tela de quem usa. Falta: volume por live e silenciar, destaque de uma live, diagnóstico, servidor de retransmissão (TURN) |
| N8 | Transmitir tela pelo motor, com o seletor | **Feito o essencial**: botão "Tela" abre a lista de telas e janelas (sem miniaturas), o motor captura e responde a cada pessoa que pede para assistir, "Encerrar" e sair do canal acabam com a live, e ela acaba sozinha se a janela fecha. O clique no seletor não foi testado (os testes começam a live por argumento). Desde 17/11/2026: prévia da própria live gravada no canal (3 s depois de começar e a cada 45 s; 16 a 20 ms por foto) e mostrada no próprio ladrilho; som padrão do seletor igual ao do site (só o do programa escolhido; na tela inteira, o do computador menos o do Nimbo), **sem teste com som de jogo de verdade**. Falta: qualidade e som à escolha, trocar de fonte sem encerrar, pausa quando o jogo sai da frente |
| N9 | Login de verdade do Google e sessão guardada entre aberturas | **Feito no emulador**: o app abre a página `/desktop-login` no navegador (mesmo módulo do app atual), recebe a credencial, e ao reabrir entra sozinho com a sessão guardada (cifrada pelo Windows, DPAPI). Falta um login real do Google (`--production`), que só o dono pode fazer |
| N10 | Webcam (enviar) e lives vindas de navegador (receber) | Falta |
| N11 | Apelido, perfil e foto; administração (no app ou só pelo site) | **Administração feita** (17/11/2026), igual ao site: menu do botão direito na pessoa (cargos, nível, revogar, remover sessão), no canal e na categoria; janela de configurações (Visão geral, Permissões, Excluir, barra de alterações não salvas); "Gerenciar comunidade" com Solicitações, Membros, Cargos (editor no estilo do Discord) e Canais. Fotos de perfil aparecem (data URL e Google). Clicado de verdade (roteiro `--clicks`) e conferido no banco: dar cargo pelo menu, salvar cargos e nível do canal, aprovar já num cargo, criar e renomear cargo, pôr pessoa no cargo, criar e excluir canal, remover sessão. Falta: apelido e foto (editar), aba Rede e quem pode configurar a rede |
| N12 | Atualização automática, instalador, instância única | Falta |
| N13 | Troca: o nativo vira o Nimbo | Falta (só com o OK do dono) |

Medido em 15/11/2026 (i5-14600KF + RTX 5070), app nativo parado na tela inicial, compilado em modo otimizado:

| Medida | Nativo | App atual (medido no mesmo dia) |
|---|---|---|
| Executável | 16 MB | - |
| Memória em uso | 203 MB (DX12), 146 MB (Vulkan), 156 MB (OpenGL) | 385 MB |
| CPU parado (um núcleo) | 0% a 1,5% | não medido |

A memória ficou menor que a do app atual, mas longe de "leve": quase tudo é da placa de vídeo (o driver e o
wgpu), não da interface. Fica para otimizar depois que o player entrar, porque é ele que decide como a placa
é usada. Ainda não mede nada de vídeo.

### Recursos de live e cargos no app nativo (medido em 17/11/2026)

Emulador próprio (banco 9001, login 9098) com as regras do repositório, 4 apps nativos na mesma máquina
(i5-14600KF + RTX 5070), compilados em modo otimizado, janela de teste com movimento (1920x1032, fora da
tela). Bia transmite; Caio e Ana já estavam no canal; o Dono entra e abre tudo.

| Medida | Resultado |
| --- | --- |
| Prévia da própria live (foto pelo GDI, JPEG, base64) | 20,2 ms a primeira e 15,7 ms a segunda; 12823 e 10827 caracteres (limite 14000); uma a cada 45 s, ou seja, perto de 0,04% de um núcleo |
| Quem assiste (Dono, uma live) | 60 quadros por segundo recebidos, decodificados e desenhados, "1032p · 60 fps" no ladrilho, 7,0% a 8,6% de um núcleo |
| Plateia | o olho mostrou 1 e depois 2 pessoas nos ladrilhos e nos cartões, com os nomes ao passar o mouse |
| Aviso de live nova | Caio (já no canal) recebeu "Bia começou uma live" com Assistir; Ana (idem) viu o cartão com a prévia de verdade |
| Prévia na barra | Ana, fora do canal, parou o mouse na Bia: a foto apareceu ao lado da barra (lida uma vez do banco) |
| Tela cheia | duplo clique abriu a live sozinha na janela; Esc voltou para o palco |

Cargos e permissões, clicados pelo roteiro `--clicks` (cliques e teclas entram no egui como se fossem
do mouse) e conferidos no banco por leitura direta: dar cargo pelo menu (as duas gravações,
`memberRoles` e `roleMembers`), marcar cargo num canal e salvar, administrador mudar o nível mínimo e
salvar, aprovar uma solicitação já num cargo, criar cargo, renomear e trocar a cor, pôr uma pessoa no
cargo pela busca, criar canal pela caixa de nome, excluir canal pela confirmação, remover sessão sem
perfil, e fechar a janela com alteração pendente (pede confirmação). O que o administrador não pode, o
banco recusa (no `selftest`): dar cargo, promover alguém, pôr "só o proprietário", marcar cargo,
aprovar, editar canal que não vê.

Não testado: o site lendo a plateia e as prévias gravadas pelo nativo (e o contrário); a tela cheia que
cobre o monitor; som de jogo de verdade no padrão novo; escala de tela diferente de 100%; as regras
endurecidas do site (`roleDefs` e `roleMembers` só para o proprietário), que o nativo já não lê para quem
não é proprietário.

### Live nativa de ponta a ponta (medido em 15/11/2026)

Dois `nimbo-native` no mesmo canal do emulador, na mesma máquina (i5-14600KF + RTX 5070), compilados em modo
otimizado: um transmite uma janela de teste com movimento (1920x1032, fora da tela), o outro assiste com o
som em volume zero. A janela de teste desenha a 75 quadros por segundo, mas o motor mandou 37,5: com o teto
de 60, ele descarta o quadro que chega cedo demais e, numa tela de 75 Hz, isso é um quadro sim, um não (numa
de 144 Hz dá os 48 das medições anteriores). É do motor, vale também para o app atual, e não foi mexido aqui.

| Medida (46 s, uma live) | Quem assiste | Quem transmite |
|---|---|---|
| Quadros por segundo | 37,5 recebidos, 37,5 decodificados, 37,5 desenhados | 37,5 codificados (placa NVIDIA) |
| Descartados, pacotes faltando | 0 e 0 (11 descartados só quando entra com a live já no ar, até o primeiro quadro completo) | - |
| Decodificar, com a cópia para a memória | 1,58 ms por quadro | - |
| Do primeiro pacote ao quadro pronto | 1,79 ms | - |
| Taxa | 5,3 Mbps recebidos | 5,0 Mbps produzidos (alvo de 8) |
| Som | 1000 ms de som por segundo entregues à saída, nenhuma falha | capturado e enviado |
| CPU (um núcleo), média e faixa | 12,5% (7,8% a 16,4%) | 6,3% (2,3% a 9,4%) |
| Memória em uso / privada | 215 a 262 MB / 433 a 451 MB | 213 a 249 MB / 469 a 484 MB |

- **Para onde vai a CPU de quem assiste**: o player gasta perto de 3% (decodificador 1,5%, rede 1,4%, som
  0,1%). O resto, uns 9%, é a interface redesenhando a janela inteira a cada quadro do vídeo. É o próximo
  lugar a otimizar; desenhar direto da textura do decodificador (sem a cópia de 3 MB por quadro) é o outro.
- **Conversão de cor na CPU** (`--cpu-video`, a reserva para quando a janela não está no wgpu): 31,0% de um
  núcleo (25,8% a 34,4%), com os mesmos 37,5 quadros desenhados. Por isso o caminho normal é o shader.
- **Duas lives ao mesmo tempo** (dois players no mesmo processo, 56 s): 37,5 quadros por segundo desenhados
  em cada uma, 18,0% de um núcleo (13,3% a 23,4%).
- **Comparação com o navegador**: não foi refeita aqui. Na bancada do player, um Chromium assistindo à
  mesma live gastou 8% a 10% de um núcleo e 563 MB; o app nativo inteiro gasta mais CPU que isso hoje (12,5%)
  e menos da metade da memória.

Situações conferidas (pelos números de `--stats` de quem assiste, a cada 2 s):

- quem assiste já está no canal e a live começa: o ladrilho aparece e toca em até 2 s;
- quem assiste entra com a live já no ar: abre sozinha, como no site;
- quem transmite fecha o app: o ladrilho some em até 2 s; quando a live recomeça, volta sozinho;
- a janela transmitida fecha: a live acaba dos dois lados;
- sair do canal ou clicar em "Encerrar" tira a live (no `selftest` do miolo).

Não testado: **o site ou o app atual assistindo a uma live do app nativo, e o app nativo assistindo a uma
live do app atual** (a sinalização segue o formato do site à risca, mas nenhum navegador entrou no teste);
duas máquinas, internet, NAT e perda de rede; "Tentar de novo" e a desistência depois de 3 tentativas (não
houve falha de conexão para exercitar); som audível; o clique no seletor; monitor inteiro como fonte; mais de
duas lives; janela visível na tela (os testes abrem fora dela); queda da conexão com o banco durante a live
(só no `selftest` do miolo).

Compatibilidade com o site e com o app atual, conferida em 15/11/2026 no emulador, nos dois sentidos:

| Quem transmite | Quem assiste | Resultado |
| --- | --- | --- |
| App nativo (`--broadcast`, janela de teste) | Site no navegador (a versão publicada naquele dia) | Conectou em 0,24 s; 1920x1032 a 37,5 quadros por segundo (o teto antigo do motor); o diagnóstico do site mostrou o transmissor como "0.1.0 (nativo)" |
| App atual (`desktop/rust-app`, compilado da branch da 0.5.6, escolhendo a janela pelo seletor) | App nativo | 60 quadros por segundo recebidos, decodificados e desenhados; 1920x1032; nenhum quadro descartado nem pacote faltando; som decodificado inteiro (volume 0); 1,6 ms para decodificar cada quadro |

Tudo na mesma máquina. Duas máquinas, internet e perda de rede continuam sem teste.

Como rodar (emuladores; `--production` fala com o Firebase de verdade e ainda está em validação):

```
npm run emulators
cargo run --manifest-path desktop/rust-core/Cargo.toml -- selftest
cargo run --manifest-path desktop/rust-core/Cargo.toml -- probe
cargo run --manifest-path desktop/rust-ui/Cargo.toml -- --dev-user owner [--join <canal>] [--screenshot foto.png]
cargo run --manifest-path desktop/rust-ui/Cargo.toml          # login pelo navegador, com `npm run dev:local` no ar
# live sem clicar (sempre com --mute nos testes: nenhum som audível):
cargo run --release --manifest-path desktop/rust-ui/Cargo.toml -- --dev-user a --join geral --broadcast <janela> --sound --stats
cargo run --release --manifest-path desktop/rust-ui/Cargo.toml -- --dev-user b --join geral --watch-all --mute --stats
```

`--broadcast` recebe o identificador da janela no Windows; `--position x,y` abre a janela do app onde for
pedido (os testes usam fora da tela); `--stats` imprime os números a cada 2 s; `--cpu-video` troca o desenho
pela conversão na CPU. Outro par de emuladores (para dois testes não dividirem o banco) entra por
`NIMBO_EMULATOR_DB` e `NIMBO_EMULATOR_AUTH` (`host:porta`).

Telas abertas sem clicar, para fotos de teste: `--open-admin solicitacoes|membros|cargos|canais`,
`--open-role <nome do cargo>` com `--role-tab exibicao|acesso|membros`, `--open-settings canal:<id ou
nome>[:permissoes]` (ou `categoria:`), `--open-menu <nome da pessoa | sem-perfil>`, `--no-auto-open`
(para ver os cartões) e `--fullscreen-in-window`. `--clicks "1500:dir:78,173;2500:esq:297,377"` clica e
digita sozinho (`esq`, `dir`, `duplo`, `mover`, `tecla:Enter`, `texto:...`, `tudo`).

`--dev-user nome` entra na mesma conta que `?devUser=nome` no site. O que ainda não existe no nativo não faz
nada ao clicar (editar o perfil).

Banco de verdade: `nimbo-core probe` abre a conexão sem conta nenhuma (não lê nem grava nada). Em 15/11/2026
conectou em 0,5 s e a leitura sem login foi recusada pelas regras, como deve ser; o relógio do servidor estava
112 s atrás do da máquina do dono (o mesmo desvio já conhecido).

Não testado: banco de verdade com conta (só a conexão sem login), conta real do Google, token vencendo de
verdade (a renovação foi testada chamando direto), telas com escala diferente de 100%, sair da conta pelo botão
(apaga a sessão guardada; só o código foi revisto).
