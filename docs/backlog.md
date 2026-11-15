# Backlog do Nimbo

Itens observados em uso real que ainda não foram resolvidos. Mais antigos primeiro.

## Lives

### ~~Mini travada nas outras lives ao abrir ou fechar a própria~~ (resolvido em 02/11/2026)
Resolvido na linha da ideia abaixo: os envios não são mais desmontados, só trocam de faixa
(`syncSenders` em `src/live/room.ts`), sem renegociar.

Ao iniciar ou encerrar a própria live, as lives dos outros congelam por uma fração de segundo e voltam.

- **Causa provável:** cada início/fim de live adiciona ou para transceivers na `RTCPeerConnection`
  compartilhada com cada pessoa, o que dispara uma renegociação (nova oferta/resposta). Durante a
  renegociação o navegador reajusta jitter buffer e estimativa de banda das outras faixas.
- **Ideia de correção:** negociar uma vez transceivers fixos por tipo (tela e câmera, vídeo e áudio)
  e usar `sender.replaceTrack()` para ligar/desligar, como já é feito no "Trocar tela". Sem
  renegociação, sem travada. Atenção ao `streamId` anunciado (precisa continuar identificando
  tela × câmera para quem assiste).

### Quedas ocasionais na live de um amigo (relatado em 01/11/2026)
Às vezes a live de um amigo cai e volta.

- **Hipóteses:** upload insuficiente (1080p60 pede até ~6 Mbps por espectador), perda de pacotes na
  rede dele, ou reconexão do Firebase (`.info/connected`) que hoje recria todas as conexões.
- **Para investigar:** rodar `nimboDebug()` nos dois lados durante uma queda (rota, bytes, quadros).
- **Ideias:** bitrate adaptativo (baixar `maxBitrate` quando houver perda), aviso discreto de rede
  instável, não recriar as conexões P2P quando só a conexão com o Firebase oscilou.

### ~~Reconexão mais rápida logo depois de entrar no canal~~ (feito na 0.3.9, 04/11/2026)
Ao reabrir o app (por exemplo, depois de atualizar) a pessoa entra no canal com uma sessão nova e a
conexão com quem está transmitindo é refeita do zero. Quando a primeira tentativa não pega, o vigia
de `src/live/room.ts` (`checkMissing`) só reconecta depois de 20 s sem a live chegar, e no máximo uma
vez a cada 45 s; nesse intervalo a live fica em "Conectando". Ideia: nos primeiros ~30 s depois de
entrar no canal, considerar a live presa com ~6 s e permitir a primeira reconexão sem esperar os
45 s; depois disso volta aos tempos atuais (para não ficar reconectando em rede ruim).

### Servidor de retransmissão (TURN) pendente de configurar (adiado em 09/11/2026)
O app já está pronto (aba Rede: credencial testada antes de salvar, limite mensal padrão 800 GB,
quem pode configurar). Falta o dono: cadastrar cartão na Cloudflare (Realtime → TURN Server →
Get Started; US$ 0 agora, 1.000 GB grátis/mês), criar alerta em Billing → Notifications, criar o
TURN "Nimbo", gerar a credencial (comando PowerShell com `generate-ice-servers`, ttl 2592000) e
colar na aba Rede; liberar o Guilherme. Ficou parado porque o código de verificação do cartão (Link)
não chegou. Caso para isso: amigos na mesma casa (Mie e irmão) com NAT que bloqueia conexão direta.

## App e atualizações

### ~~Selo "Atualização disponível" no topo~~ (feito na 0.3.4, 02/11/2026)
Quando houver versão nova, mostrar no canto superior direito da barra do topo um selo verde pequeno
com ícone de download e o texto "Atualização disponível". Um clique atualiza sozinho: no app,
reinicia e instala a versão baixada (`desktop.installUpdate`); no site, recarrega a página.

- Reaproveitar a detecção que já existe em `src/components/UpdateBanner.tsx` (app: `watchUpdate`;
  site: `siteUpdateAvailable`). O selo fica visível enquanto a atualização estiver pendente, mesmo
  depois de a pessoa fechar o cartão com "Depois".
- Entra no próximo Release do app (precisa de `nimboDesktop.watchUpdate`, presente a partir do 0.3.2).

### ~~Rodapé só com "Nimbo 0.3.4"~~ (feito na 0.3.5, 04/11/2026)
Tirar o "· app 0.3.4" do rodapé da barra lateral (`src/components/UserPanel.tsx`, `.version-line`):
fica só "Nimbo <versão>". A versão do app continua no título da janela.

### ~~Novidades sincronizadas com a atualização~~ (feito na 0.3.8, 04/11/2026)
Hoje a janela de novidades aparece quando o site carregado tem versão maior que a última vista
(`nimbo.seenVersion`), independente de a pessoa ter clicado em atualizar. O fluxo desejado é:
clicou em atualizar → o app (ou o site) reinicia → abre já mostrando as novidades da versão
nova. Ideia: ao clicar em atualizar, gravar a versão de destino; depois do reinício, mostrar as
novidades dessa versão e só então marcar como vista. Cuidar para site e app chegarem na mesma
versão (o site pode estar à frente do app) e para quem atualiza sem clicar (abriu o app depois).

## Comunidade

### ~~Foto de perfil visível para todos~~ (feito em 05/11/2026: opções A e B juntas)
Restrições: sem Firebase Storage para fotos (combinado no handoff) e custo zero (plano Spark).

- **Opção A, foto do Google:** o login já traz `user.photoURL`. Guardar essa URL no perfil
  (`community/profiles/$uid/photo`) e mostrar no lugar da inicial (barra lateral, participantes,
  painel, cartões de live). Sem upload e sem custo. Regra: só `https://` em `*.googleusercontent.com`,
  até ~300 caracteres.
- **Opção B, foto própria:** a pessoa escolhe uma imagem, o navegador recorta em quadrado, reduz
  para 96x96 e converte para WebP (~5 a 8 KB), e o texto em base64 vai para um nó separado
  (`community/avatars/$uid`), lido uma vez e guardado em cache, para não pesar no perfil. Regra:
  dono escreve a própria, começa com `data:image/webp;base64,` e no máximo ~16 KB. Com 20 pessoas
  são ~160 KB por carregamento, bem dentro do Spark (10 GB/mês de download).
- **Sugestão:** A como padrão e B como "Trocar foto" no perfil; sem foto, continua a inicial colorida.
  Moderação: o proprietário pode remover a foto de alguém (regra de escrita para owner).

### Tela do instalador mais bonita (pedido em 04/11/2026, para a próxima atualização do app)
Hoje a instalação e cada atualização mostram a janelinha padrão do NSIS ("Nimbo Setup ·
Installing, please wait..."), em inglês e cinza.

- **Feito na 0.3.8 — atualização sem janela:** chamar `autoUpdater.quitAndInstall(true, true)` (instalação silenciosa
  e reabre o app sozinho). Quem atualiza pelo selo/cartão não vê mais o instalador. É o ganho maior.
- **Primeira instalação:** o instalador de um clique do NSIS quase não deixa mudar o visual. Dá para
  pôr em português (`nsis.installerLanguages: ['pt_BR']`, "Instalando, aguarde...") e trocar ícone e
  cabeçalho. Para uma tela realmente no estilo do Nimbo (roxo, mascote, barra animada), as opções
  são uma página customizada em script NSIS (`nsis.include`) ou trocar o instalador por um próprio
  (um app Electron pequeno que baixa e instala, com a nossa tela). Avaliar custo antes.


## Ideias anotadas (ainda sem data)

### App full Rust, em etapas, mantendo o site (anotado em 15/11/2026)
O dono gostou da ideia depois de ver que o Discord anunciou em 04/11/2026 a troca do Electron por um app em
Rust (miolo compartilhado, interface desenhada com GPUI, sem Tauri), e que o site deles continua existindo.
Hoje no Nimbo só a transmissão é Rust; a interface e o player de quem assiste são o site dentro do WebView2.
Decisão de 15/11/2026 (à noite): o dono liberou começar, numa branch à parte (`feat/full-rust`) e validando
bem antes de trocar. O estado de cada etapa fica em `docs/rust-app.md`, seção "App full Rust". A ordem mudou
em relação à lista abaixo: o miolo e a janela vieram primeiro (são a base para testar o resto), com o player
sendo feito em paralelo. A 0.5.5 continua precisando de uso real (jogo de verdade: som, tela cheia, anticheat).

Etapas, cada uma já utilizável e sem desligar o site:

1. **Player em Rust**: receber, decodificar e desenhar a live dentro do app atual. É a parte mais difícil e a que
   mais rende (menos memória, controle da rede de quem assiste). Vale mesmo que o resto não saia.
2. **Miolo em Rust**: login do Google e a conversa com o banco, sem interface, testado contra os emuladores.
3. **Interface em Rust**: canais, palco, seletor, perfil. O painel de administração pode continuar no site.
4. **Troca**: o app deixa de carregar o site, que continua para celular, navegador e administração.

Custos conhecidos: duas interfaces para manter; correção rápida pelo site deixa de valer para o app (cada ajuste
vira versão nova, que chega em até 30 minutos e pede reinício, como no Discord); ganho de memória menor que o
deles (o Nimbo usa 385 MB hoje, medido em 15/11/2026, contra 544 MB do app antigo).

### Chat de texto e, mais adiante, chamada de voz (perguntado em 15/11/2026)
Voz e chat continuam fora do escopo até o dono pedir; aqui fica só a viabilidade no plano gratuito.

- **Chat de texto**: cabe com folga. Mensagem é pequena (centenas de bytes) e o banco em tempo real já é a
  sinalização das lives. Cuidados: carregar só as últimas mensagens de cada canal (o limite que aperta é o de
  download do banco), regras de escrita por canal e por autor, apagar e moderar. Sem anexos: imagem e arquivo
  pediriam um lugar para guardar, e o Firebase Storage está vetado para fotos de perfil.
- **Chamada de voz**: cabe para grupos pequenos, com cada pessoa ligada direto às outras (áudio gasta de 30 a
  60 kbps por pessoa). Até umas 8 pessoas por canal funciona assim; acima disso precisa de um servidor que
  misture ou redistribua o áudio, que é onde começa a custar. Depende do servidor de retransmissão (TURN) para
  os pares que não fecham conexão direta, e hoje o app não concede o microfone à página (só a câmera).
- **Limite que vale para tudo**: o plano gratuito do banco aceita no máximo 100 conexões ao mesmo tempo, e não
  dá para aumentar sem mudar de plano. Para o grupo de hoje sobra; conferir os números de armazenamento e
  download na página de preços antes de contar com eles.
