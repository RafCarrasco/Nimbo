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
3. Login real do Google no app novo (só foi testado com credenciais de mentira).
4. A primeira atualização pelo GitHub (o ciclo foi testado com um servidor local).
5. Placas Intel e AMD, e máquina sem codificador na placa.

Se algo disso falhar feio, o caminho de volta é publicar de novo o instalador do Electron (Release 0.4.1) como
"latest" e tirar o convite do site (`LEGACY_APP` em `src/components/UpdateBanner.tsx`). O código do app antigo
continua em `desktop/main.cjs` por isso.

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
