# GoLive Community — arquitetura do produto

## Visão
Um Discord particular focado em lives, com experiência consistente Windows e Web.
O mesmo React deve renderizar os canais e perfis no Electron e no navegador.

## Modelo
- `community/profiles/{uid}`: nome e metadados de perfil. Futuramente login durável e avatares.
- `community/channels/{channelId}`: metadados dos canais gerenciáveis.
- `community/roles/{uid}`: owner/admin/member. Apenas owner concede admin.
- `rooms/{channelId}`: presença de sessão, ofertas, ICE e lives WebRTC.
  - `streams/{uid}/{screen|camera}`: a live anunciada. `startedAt` é na hora do servidor (o tempo no ar sai
    certo mesmo com o relógio de alguém errado); `game` é o nome do jogo, só quando a fonte é um jogo.
  - `watching/{quem assiste}/{quem transmite}/{screen|camera}`: a plateia de cada live. Cada pessoa escreve só
    a própria e ela some quando a conexão cai.
- `community/access/{channelId}/{uid}`: futura lista de acesso por canal.
- Canais padrão: geral e competitivo.

## Quem vê o quê (níveis e cargos)
Vale o que está em `scripts/build-rules.mjs` (gera `database.rules.json`); a interface só espelha.

- **Nível** (`community/roles/{uid}`): `owner`, `admin` ou `member`. Só o proprietário vê tudo.
  Administrador cria e organiza categorias e canais **que enxerga** e remove sessões abandonadas
  desses canais; não aprova pessoas, não mexe em cargos e não entra em grupo do qual não faz parte.
- **Cargos** (grupos de amigos): `community/roleDefs/{id}` (nome e cor), `community/memberRoles/{uid}/{id}`
  (o que as regras consultam) e `community/roleMembers/{id}/{uid}` (o mesmo dado ao contrário, gravado
  junto por `setMemberRole`). Uma categoria ou canal pode exigir até 5 cargos
  (`categoryRoles`, `channelRoles`): só quem tem um deles lê os metadados e a sala. Sem cargo marcado,
  vale só o nível mínimo (`categoryAccess`, `channelAccess`).
- **Um grupo não enumera o outro.** Só o proprietário lê as listas inteiras de `profiles`, `avatars`,
  `roles`, `netAdmins`, `roleDefs`, `memberRoles` e `roleMembers`. Os demais leem: o próprio nível, os
  ids dos próprios cargos e perfil e foto de uma pessoa por vez, pelo id (o id só aparece para quem divide um
  canal com ela). No site isso é `useCommunity` (lista para o proprietário, item a item para os demais) e
  `usePeople`.
- **Só o proprietário vê o sistema de cargos** (pedido do dono em 17/11/2026). Nome, cor e quem tem cada cargo
  (`roleDefs`, `roleMembers`) só ele lê, nem quem tem o cargo. Na tela, os demais não veem cadeado, nome de cargo
  nem cor de cargo, e o administrador configura canais só pela visão geral e pelo nível mínimo.
- **Interface** (estilo Discord): botão direito na pessoa (cargos, nível, revogar), no canal e na categoria
  (editar, permissões, excluir); engrenagem abre a janela de configurações (`ChannelSettings.tsx`, com rascunho e
  barra de salvar); Gerenciar comunidade → Cargos é o editor (`RoleEditor.tsx`: Exibição, Acesso, Membros).
- **Mudar permissão não derruba ninguém**: a leitura dos canais não é refeita do zero quando cargos mudam (só o
  que tinha sido recusado é pedido de novo), e o app só tira alguém do canal depois de 3 s sem acesso.
- Continuam legíveis por qualquer membro aprovado, por conterem só ids: `categoryIds`, `channelIds`,
  `categoryRoles`, `channelRoles`, `categoryAccess`, `channelAccess`.

## Segurança e rollout
Esta branch prepara UI e serviços. **Não publique regras permissivas nem ofereça administração em produção sem testes de regras**. A autenticação anônima atual não é um sistema de identidade seguro para administrar usuários, pois IDs podem mudar quando o usuário limpa dados locais. Antes de liberar controles administrativos, implementar login durável (Google/email), provisionamento seguro de owner e regras testadas no emulator. O acesso de pessoas e canais restritos deve ser validado no banco, nunca só pela interface.

## Próximos marcos
1. Perfis e navegação entre canais, sidebar com lives e presença.
2. Cadastro durável de conta e provisionamento de owner.
3. RBAC de canais/membros, convites e painel administrativo.
4. Motor de voz separado do vídeo. Ao encerrar live, não derrubar peers de voz nem lives alheias.
5. Revisão multi-peer: pares negociados por participante e mídia (evitar sobrescrever streams ao receber nova track).
6. Captura Windows por jogo; opt-in para loopback de sistema; echo cancellation para microfone; isolamento de áudio.
7. Testes de regras Firebase, testes E2E multiusuário, perfis falsos e carga de 5–20 pessoas.

## Reversecord
Referência conceitual apenas; nenhum código/biblioteca binária do projeto de engenharia reversa será incorporado.
