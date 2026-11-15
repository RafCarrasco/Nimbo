// Testes das regras do Realtime Database. Rode com `npm run test:rules` (usa o emulador local,
// projeto demo-nimbo, sem tocar no Firebase de produção).
import { readFileSync } from 'node:fs';
import { after, before, beforeEach, describe, test } from 'node:test';
import { assertFails, assertSucceeds, initializeTestEnvironment } from '@firebase/rules-unit-testing';
import { get, ref, remove, set, update } from 'firebase/database';

const rules = readFileSync(
  process.env.RULES_FILE || new URL('../database.rules.json', import.meta.url),
  'utf8',
);
let env;

const as = (uid, provider = 'google.com') =>
  env
    .authenticatedContext(uid, {
      email: uid + '@example.com',
      firebase: { sign_in_provider: provider, identities: {} },
    })
    .database();
const owner = () => as('owner');
const admin = () => as('admin');
const member = () => as('member');
const pending = () => as('pending');

before(async () => {
  const [host, port] = (process.env.FIREBASE_DATABASE_EMULATOR_HOST || '127.0.0.1:9000').split(':');
  env = await initializeTestEnvironment({
    projectId: 'demo-nimbo',
    database: { rules, host, port: Number(port) },
  });
});
after(() => env?.cleanup());

beforeEach(async () => {
  await env.clearDatabase();
  await env.withSecurityRulesDisabled(async (ctx) => {
    await set(ref(ctx.database(), '/'), {
      community: {
        approved: { owner: true, admin: true, member: true, outro: true, anon: true },
        roles: { owner: 'owner', admin: 'admin', member: 'member' },
        profiles: {
          member: { displayName: 'Membro', createdAt: 1, updatedAt: 1 },
          outro: { displayName: 'Outro', createdAt: 1, updatedAt: 1 },
        },
        requests: { pending: { email: 'pending@example.com', createdAt: 1 } },
        categories: {
          vipcat: { name: 'VIP', createdAt: 1, createdBy: 'owner', order: 1 },
          grupoA: { name: 'Faculdade', createdAt: 1, createdBy: 'owner', order: 2 },
          grupoB: { name: 'Trampo', createdAt: 1, createdBy: 'owner', order: 3 },
        },
        categoryIds: { vipcat: 1, grupoA: 2, grupoB: 3 },
        channelIds: { staff: true, vip: true, off: true, canalA: true, canalB: true },
        roleDefs: {
          faculdade: { name: 'Faculdade', color: '#7c6cff', createdAt: 1 },
          trampo: { name: 'Trampo', color: '#3ecf8e', createdAt: 1 },
        },
        memberRoles: { member: { faculdade: true }, outro: { trampo: true } },
        roleMembers: { faculdade: { member: true }, trampo: { outro: true } },
        categoryRoles: { grupoA: ['faculdade'], grupoB: ['trampo'] },
        channels: {
          staff: {
            name: 'Staff',
            description: '',
            createdBy: 'owner',
            createdAt: 1,
            visibility: 'public',
            enabled: true,
          },
          vip: {
            name: 'VIP',
            description: '',
            categoryId: 'vipcat',
            createdBy: 'owner',
            createdAt: 1,
            visibility: 'public',
            enabled: true,
          },
          canalA: {
            name: 'A',
            description: '',
            categoryId: 'grupoA',
            createdBy: 'owner',
            createdAt: 1,
            visibility: 'public',
            enabled: true,
          },
          canalB: {
            name: 'B',
            description: '',
            categoryId: 'grupoB',
            createdBy: 'owner',
            createdAt: 1,
            visibility: 'public',
            enabled: true,
          },
          off: {
            name: 'Off',
            description: '',
            createdBy: 'owner',
            createdAt: 1,
            visibility: 'public',
            enabled: false,
          },
        },
        channelAccess: { staff: 'admin' },
        categoryAccess: { vipcat: 'owner' },
      },
      rooms: {
        geral: {
          members: { owner: { joinedAt: 1 }, member: { joinedAt: 1 } },
          streams: { owner: { screen: { startedAt: 1, streamId: 's1' } } },
          signals: { owner: { member: { m1: { type: 'ice', candidate: { candidate: 'x' } } } } },
        },
        vip: { members: { owner: { joinedAt: 1 } } },
      },
    });
  });
});

describe('autenticação e aprovação', () => {
  test('visitante sem login não lê nada', async () => {
    const db = env.unauthenticatedContext().database();
    await assertFails(get(ref(db, 'rooms/geral/members')));
    await assertFails(get(ref(db, 'community/channels')));
  });
  test('conta pendente não lê canais nem salas', async () => {
    await assertFails(get(ref(pending(), 'community/channels')));
    await assertFails(get(ref(pending(), 'rooms/geral/members')));
  });
  test('login anônimo é bloqueado mesmo com aprovação', async () => {
    const db = as('anon', 'anonymous');
    await assertFails(get(ref(db, 'community/channels')));
    await assertFails(get(ref(db, 'rooms/geral/members')));
  });
  test('usuário lê apenas a própria aprovação', async () => {
    await assertSucceeds(get(ref(pending(), 'community/approved/pending')));
    await assertFails(get(ref(pending(), 'community/approved/member')));
    await assertFails(get(ref(member(), 'community/approved')));
    await assertSucceeds(get(ref(owner(), 'community/approved')));
  });
  test('ninguém além do proprietário aprova', async () => {
    await assertFails(set(ref(pending(), 'community/approved/pending'), true));
    await assertFails(set(ref(admin(), 'community/approved/pending'), true));
    await assertSucceeds(set(ref(owner(), 'community/approved/pending'), true));
  });
  test('proprietário não revoga a própria aprovação', async () => {
    await assertFails(set(ref(owner(), 'community/approved/owner'), false));
    await assertFails(remove(ref(owner(), 'community/approved/owner')));
    await assertSucceeds(remove(ref(owner(), 'community/approved/member')));
  });
});

describe('solicitações', () => {
  test('usuário cria a própria solicitação com o próprio e-mail', async () => {
    await assertSucceeds(
      set(ref(pending(), 'community/requests/pending'), { email: 'pending@example.com', createdAt: 2 }),
    );
    await assertFails(
      set(ref(pending(), 'community/requests/pending'), { email: 'outro@example.com', createdAt: 2 }),
    );
    await assertFails(
      set(ref(pending(), 'community/requests/member'), { email: 'pending@example.com', createdAt: 2 }),
    );
    await assertFails(
      set(ref(pending(), 'community/requests/pending'), {
        email: 'pending@example.com',
        createdAt: 2,
        role: 'owner',
      }),
    );
  });
  test('somente o proprietário lê e remove solicitações', async () => {
    await assertFails(get(ref(admin(), 'community/requests')));
    await assertSucceeds(get(ref(owner(), 'community/requests')));
    await assertFails(remove(ref(admin(), 'community/requests/pending')));
    await assertSucceeds(remove(ref(owner(), 'community/requests/pending')));
  });
});

describe('cargos', () => {
  test('membro não se promove', async () => {
    await assertFails(set(ref(member(), 'community/roles/member'), 'admin'));
  });
  test('administrador não altera cargos', async () => {
    await assertFails(set(ref(admin(), 'community/roles/member'), 'admin'));
    await assertFails(set(ref(admin(), 'community/roles/admin'), 'owner'));
  });
  test('proprietário altera cargos mas não se rebaixa', async () => {
    await assertSucceeds(set(ref(owner(), 'community/roles/member'), 'admin'));
    await assertFails(set(ref(owner(), 'community/roles/owner'), 'member'));
    await assertFails(remove(ref(owner(), 'community/roles/owner')));
    await assertFails(set(ref(owner(), 'community/roles/member'), 'superuser'));
  });
});

describe('perfis', () => {
  test('nickname de 3 a 24 caracteres, sem campos extras', async () => {
    const p = (displayName, extra = {}) => ({ displayName, createdAt: 1, updatedAt: 2, ...extra });
    await assertSucceeds(set(ref(member(), 'community/profiles/member'), p('Rafa')));
    await assertFails(set(ref(member(), 'community/profiles/member'), p('ab')));
    await assertFails(set(ref(member(), 'community/profiles/member'), p('x'.repeat(25))));
    await assertFails(set(ref(member(), 'community/profiles/member'), p('Rafa', { role: 'owner' })));
    await assertFails(set(ref(member(), 'community/profiles/admin'), p('Rafa')));
    await assertFails(set(ref(pending(), 'community/profiles/pending'), p('Rafa')));
  });
});

describe('servidor de retransmissão', () => {
  const turn = { urls: 'turn:relay.example.com:3478', username: 'u', credential: 'p', updatedAt: 1 };
  test('só o proprietário grava; aprovados leem; pendentes não', async () => {
    await assertSucceeds(set(ref(owner(), 'community/turn'), turn));
    await assertFails(set(ref(admin(), 'community/turn'), turn));
    await assertFails(set(ref(member(), 'community/turn'), turn));
    await assertSucceeds(get(ref(member(), 'community/turn')));
    await assertFails(get(ref(pending(), 'community/turn')));
  });
  test('quem o proprietário libera também configura a rede', async () => {
    await assertFails(set(ref(member(), 'community/netAdmins/member'), true));
    await assertSucceeds(set(ref(owner(), 'community/netAdmins/member'), true));
    await assertSucceeds(set(ref(member(), 'community/turn'), { ...turn, monthlyLimitGb: 800 }));
    await assertFails(set(ref(admin(), 'community/turn'), turn));
    await assertSucceeds(remove(ref(owner(), 'community/netAdmins/member')));
    await assertFails(set(ref(member(), 'community/turn'), turn));
  });
  test('contador de uso só cresce, com saltos limitados', async () => {
    await assertSucceeds(set(ref(member(), 'community/turnUsage/2026-11'), 1000));
    await assertSucceeds(set(ref(member(), 'community/turnUsage/2026-11'), 2000));
    await assertFails(set(ref(member(), 'community/turnUsage/2026-11'), 500));
    await assertFails(set(ref(member(), 'community/turnUsage/2026-11'), 2000 + 6e9));
    await assertFails(set(ref(member(), 'community/turnUsage/novembro'), 10));
    await assertFails(set(ref(pending(), 'community/turnUsage/2026-11'), 3000));
    await assertSucceeds(get(ref(member(), 'community/turnUsage')));
  });
  test('formato validado', async () => {
    await assertFails(set(ref(owner(), 'community/turn'), { ...turn, monthlyLimitGb: 0 }));
    await assertFails(set(ref(owner(), 'community/turn'), { ...turn, urls: 'https://evil.example' }));
    await assertFails(set(ref(owner(), 'community/turn'), { ...turn, username: '' }));
    await assertFails(set(ref(owner(), 'community/turn'), { ...turn, extra: 1 }));
    await assertSucceeds(set(ref(owner(), 'community/turn'), { ...turn, urls: 'turns:relay.example.com:5349,turn:relay.example.com:3478?transport=tcp' }));
    await assertSucceeds(remove(ref(owner(), 'community/turn')));
  });
});

describe('fotos de perfil', () => {
  const webp = 'data:image/webp;base64,' + 'A'.repeat(4000);
  test('cada um troca só a própria foto, pequena e de imagem', async () => {
    await assertSucceeds(set(ref(member(), 'community/avatars/member'), webp));
    await assertSucceeds(set(ref(member(), 'community/avatars/member'), 'https://lh3.googleusercontent.com/a/abc=s128-c'));
    await assertFails(set(ref(member(), 'community/avatars/admin'), webp));
    await assertFails(set(ref(member(), 'community/avatars/member'), 'data:image/webp;base64,' + 'A'.repeat(30000)));
    await assertFails(set(ref(member(), 'community/avatars/member'), 'data:text/html;base64,PHNjcmlwdD4='));
    await assertFails(set(ref(member(), 'community/avatars/member'), 'https://evil.example/foto.png'));
    await assertFails(set(ref(member(), 'community/avatars/member'), { url: webp }));
    await assertFails(set(ref(pending(), 'community/avatars/pending'), webp));
    await assertSucceeds(remove(ref(member(), 'community/avatars/member')));
  });
  test('membros aprovados veem a foto de uma pessoa; pendentes não', async () => {
    await assertSucceeds(get(ref(member(), 'community/avatars/admin')));
    await assertFails(get(ref(pending(), 'community/avatars/admin')));
  });
  test('proprietário pode apagar a foto de alguém, mas não trocar', async () => {
    await env.withSecurityRulesDisabled((ctx) => set(ref(ctx.database(), 'community/avatars/member'), webp));
    await assertFails(set(ref(owner(), 'community/avatars/member'), webp));
    await assertFails(remove(ref(admin(), 'community/avatars/member')));
    await assertSucceeds(remove(ref(owner(), 'community/avatars/member')));
  });
});

describe('canais e categorias', () => {
  const channel = {
    name: 'Novo',
    description: '',
    createdBy: 'admin',
    createdAt: 1,
    visibility: 'public',
    enabled: true,
  };
  test('membro não cria canal; administrador cria', async () => {
    await assertFails(set(ref(member(), 'community/channels/novo'), channel));
    await assertSucceeds(set(ref(admin(), 'community/channels/novo'), channel));
  });
  test('canais padrão não podem ser alterados', async () => {
    await assertFails(set(ref(owner(), 'community/channels/geral'), channel));
  });
  test('categoria padrão é protegida', async () => {
    await assertFails(
      set(ref(owner(), 'community/categories/lives'), { name: 'X', createdAt: 1, createdBy: 'o', order: 0 }),
    );
  });
});

describe('permissões de acesso', () => {
  test('administrador restringe canal a admins', async () => {
    await assertSucceeds(set(ref(admin(), 'community/channelAccess/geral'), 'admin'));
    await assertFails(set(ref(member(), 'community/channelAccess/geral'), 'admin'));
  });
  test('administrador não mexe em restrições de proprietário', async () => {
    await assertFails(set(ref(admin(), 'community/channelAccess/geral'), 'owner'));
    await assertFails(set(ref(admin(), 'community/categoryAccess/vipcat'), 'member'));
    await assertFails(remove(ref(admin(), 'community/categoryAccess/vipcat')));
    await assertSucceeds(set(ref(owner(), 'community/categoryAccess/vipcat'), 'member'));
  });
  test('valor de acesso inválido é rejeitado', async () => {
    await assertFails(set(ref(owner(), 'community/channelAccess/geral'), 'everyone'));
  });
});

describe('salas', () => {
  test('membro aprovado lê presença e lives de canal liberado', async () => {
    await assertSucceeds(get(ref(member(), 'rooms/geral/members')));
    await assertSucceeds(get(ref(member(), 'rooms/geral/streams')));
  });
  test('ninguém lê a sala inteira (inclui sinalização alheia)', async () => {
    await assertFails(get(ref(member(), 'rooms/geral')));
    await assertFails(get(ref(owner(), 'rooms')));
  });
  test('canal restrito a admins', async () => {
    await assertFails(get(ref(member(), 'rooms/staff/members')));
    await assertSucceeds(get(ref(admin(), 'rooms/staff/members')));
    await assertSucceeds(get(ref(owner(), 'rooms/staff/members')));
    await assertFails(set(ref(member(), 'rooms/staff/members/member'), { joinedAt: 1 }));
  });
  test('herança: categoria somente do proprietário bloqueia o canal', async () => {
    await assertFails(get(ref(member(), 'rooms/vip/members')));
    await assertFails(get(ref(admin(), 'rooms/vip/members')));
    await assertSucceeds(get(ref(owner(), 'rooms/vip/members')));
  });
  test('categoria padrão restrita bloqueia canais sem categoria', async () => {
    await env.withSecurityRulesDisabled((ctx) =>
      set(ref(ctx.database(), 'community/categoryAccess/lives'), 'admin'),
    );
    await assertFails(get(ref(member(), 'rooms/geral/members')));
    await assertSucceeds(get(ref(admin(), 'rooms/geral/members')));
  });
  test('canal desativado ou inexistente não abre', async () => {
    await assertFails(get(ref(owner(), 'rooms/off/members')));
    await assertFails(get(ref(owner(), 'rooms/fantasma/members')));
    await assertFails(set(ref(owner(), 'rooms/fantasma/members/owner'), { joinedAt: 1 }));
  });
  test('entrar no canal: somente o próprio registro e com formato válido', async () => {
    await assertSucceeds(set(ref(admin(), 'rooms/geral/members/admin'), { joinedAt: 5 }));
    await assertFails(set(ref(member(), 'rooms/geral/members/admin'), { joinedAt: 5 }));
    await assertFails(set(ref(member(), 'rooms/geral/members/member'), { joinedAt: 'agora' }));
    await assertFails(set(ref(member(), 'rooms/geral/members/member'), { joinedAt: 1, host: true }));
    // Versão do app na presença: texto curto, e só isso além do joinedAt.
    await assertSucceeds(set(ref(member(), 'rooms/geral/members/member'), { joinedAt: 1, app: '0.5.1 (Rust)' }));
    await assertFails(set(ref(member(), 'rooms/geral/members/member'), { joinedAt: 1, app: 51 }));
    await assertFails(set(ref(member(), 'rooms/geral/members/member'), { joinedAt: 1, app: 'x'.repeat(33) }));
    await assertFails(set(ref(member(), 'rooms/geral/members/member'), { app: '0.5.1' }));
    // Identificador da sessão: texto curto.
    await assertSucceeds(set(ref(member(), 'rooms/geral/members/member'), { joinedAt: 1, app: 'navegador', sid: 'a1b2c3d4e5f6' }));
    await assertFails(set(ref(member(), 'rooms/geral/members/member'), { joinedAt: 1, sid: 12 }));
    await assertFails(set(ref(member(), 'rooms/geral/members/member'), { joinedAt: 1, sid: 'x'.repeat(25) }));
  });
  test('sair é sempre permitido, mesmo após perder o acesso', async () => {
    await env.withSecurityRulesDisabled((ctx) =>
      set(ref(ctx.database(), 'community/channelAccess/geral'), 'owner'),
    );
    await assertSucceeds(remove(ref(member(), 'rooms/geral/members/member')));
  });
  test('administrador remove sessão abandonada; membro não remove outros', async () => {
    await assertFails(remove(ref(member(), 'rooms/geral/members/owner')));
    await assertSucceeds(remove(ref(admin(), 'rooms/geral/members/member')));
  });
  test('transmissão exige estar no canal', async () => {
    const live = { startedAt: 1, streamId: 'abc' };
    await assertSucceeds(set(ref(member(), 'rooms/geral/streams/member/screen'), live));
    await assertSucceeds(set(ref(member(), 'rooms/geral/streams/member/camera'), live));
    await assertFails(set(ref(admin(), 'rooms/geral/streams/admin/screen'), live));
    await assertFails(set(ref(member(), 'rooms/geral/streams/owner/screen'), live));
    await assertFails(set(ref(member(), 'rooms/geral/streams/member/virus'), live));
    await assertFails(set(ref(member(), 'rooms/geral/streams/member'), true));
    await assertFails(set(ref(member(), 'rooms/geral/streams/member/screen'), { startedAt: 1 }));
  });
  test('nome do jogo na live é texto curto', async () => {
    const live = { startedAt: 1, streamId: 's2', native: true };
    await assertSucceeds(set(ref(member(), 'rooms/geral/streams/member/screen'), { ...live, game: 'Counter-Strike 2' }));
    await assertFails(set(ref(member(), 'rooms/geral/streams/member/screen'), { ...live, game: 'x'.repeat(49) }));
    await assertFails(set(ref(member(), 'rooms/geral/streams/member/screen'), { ...live, game: 7 }));
  });
  test('plateia: cada pessoa informa só o que ela mesma assiste, e quem está no canal lê', async () => {
    const mine = { owner: { screen: true } };
    await assertSucceeds(set(ref(member(), 'rooms/geral/watching/member'), mine));
    await assertSucceeds(get(ref(owner(), 'rooms/geral/watching')));
    // Não escreve a plateia de outra pessoa, nem a própria sem estar no canal.
    await assertFails(set(ref(member(), 'rooms/geral/watching/owner'), mine));
    await assertFails(set(ref(admin(), 'rooms/geral/watching/admin'), mine));
    // Só "screen" e "camera", e só `true`.
    await assertFails(set(ref(member(), 'rooms/geral/watching/member'), { owner: { tela: true } }));
    await assertFails(set(ref(member(), 'rooms/geral/watching/member'), { owner: { screen: 'sim' } }));
    // Quem não pode ver o canal não lê a plateia dele.
    await assertFails(get(ref(member(), 'rooms/vip/watching')));
    await assertFails(get(ref(pending(), 'rooms/geral/watching')));
    // Sair apaga a própria; administrador limpa a de uma sessão abandonada; os outros, não.
    await assertFails(remove(ref(as('outro'), 'rooms/geral/watching/member')));
    await assertSucceeds(remove(ref(admin(), 'rooms/geral/watching/member')));
    await assertSucceeds(set(ref(member(), 'rooms/geral/watching/member'), mine));
    await assertSucceeds(remove(ref(member(), 'rooms/geral/watching/member')));
  });
  test('encerrar uma live mantém a outra; sair remove todas', async () => {
    const live = { startedAt: 1, streamId: 'abc' };
    await assertSucceeds(set(ref(member(), 'rooms/geral/streams/member'), { screen: live, camera: live }));
    await assertSucceeds(remove(ref(member(), 'rooms/geral/streams/member/screen')));
    await assertSucceeds(remove(ref(member(), 'rooms/geral/streams/member')));
  });
});

describe('sinalização WebRTC', () => {
  const offer = { type: 'offer', sdp: { type: 'offer', sdp: 'v=0' } };
  test('cada um lê somente a própria caixa', async () => {
    await assertSucceeds(get(ref(owner(), 'rooms/geral/signals/owner')));
    await assertFails(get(ref(member(), 'rooms/geral/signals/owner')));
    await assertFails(get(ref(member(), 'rooms/geral/signals')));
  });
  test('envio somente entre participantes da sala e sem falsificar remetente', async () => {
    await assertSucceeds(set(ref(member(), 'rooms/geral/signals/owner/member/m2'), offer));
    await assertFails(set(ref(member(), 'rooms/geral/signals/admin/member/m2'), offer));
    await assertFails(set(ref(member(), 'rooms/geral/signals/owner/admin/m2'), offer));
    await assertFails(set(ref(member(), 'rooms/geral/signals/owner/member/m1'), { type: 'bogus' }));
  });
  test('mensagens não podem ser sobrescritas pelo remetente', async () => {
    await env.withSecurityRulesDisabled((ctx) =>
      set(ref(ctx.database(), 'rooms/geral/signals/owner/member/m3'), offer),
    );
    await assertFails(set(ref(member(), 'rooms/geral/signals/owner/member/m3'), offer));
  });
  test('live enviada pelo motor do app: campos extras aceitos só no formato certo', async () => {
    const signal = (extra) => ({ ...offer, ...extra });
    const to = (id) => ref(member(), 'rooms/geral/signals/owner/member/' + id);
    await assertSucceeds(set(to('n1'), signal({ native: 'screen', nonce: 'a'.repeat(36) })));
    await assertSucceeds(set(to('n2'), signal({ native: 'camera' })));
    await assertFails(set(to('n3'), signal({ native: 'outro' })));
    await assertFails(set(to('n4'), signal({ native: true })));
    await assertFails(set(to('n5'), signal({ native: 'screen', nonce: 'a'.repeat(65) })));
    await assertFails(set(to('n6'), signal({ native: 'screen', nonce: 1 })));
    const live = { startedAt: 1, streamId: 'abc' };
    await assertSucceeds(set(ref(member(), 'rooms/geral/streams/member/screen'), { ...live, native: true }));
    await assertFails(set(ref(member(), 'rooms/geral/streams/member/screen'), { ...live, native: 'sim' }));
    await assertSucceeds(set(ref(member(), 'rooms/geral/streams/member/screen'), { ...live, native: true, sound: true }));
    await assertFails(set(ref(member(), 'rooms/geral/streams/member/screen'), { ...live, native: true, sound: 'alto' }));
    // Ninguém marca a live dos outros como nativa.
    await assertFails(set(ref(member(), 'rooms/geral/streams/owner/screen'), { ...live, native: true }));
  });
  test('destinatário limpa a própria caixa em lote', async () => {
    await assertFails(update(ref(member(), 'rooms/geral'), { 'signals/owner/member/m1': null }));
    await assertSucceeds(update(ref(owner(), 'rooms/geral'), { 'signals/owner/member/m1': null }));
  });
});

describe('cargos personalizados (grupos)', () => {
  test('o índice mostra só ids; nomes de categorias de outro grupo ficam ocultos', async () => {
    await assertSucceeds(get(ref(member(), 'community/categoryIds')));
    await assertSucceeds(get(ref(member(), 'community/categories/grupoA')));
    await assertFails(get(ref(member(), 'community/categories/grupoB')));
    await assertFails(get(ref(member(), 'community/categories')));
  });
  test('canais herdam o cargo da categoria', async () => {
    await assertSucceeds(get(ref(member(), 'community/channels/canalA')));
    await assertFails(get(ref(member(), 'community/channels/canalB')));
    await assertSucceeds(get(ref(member(), 'rooms/canalA/members')));
    await assertFails(get(ref(member(), 'rooms/canalB/members')));
    await assertFails(set(ref(member(), 'rooms/canalB/members/member'), { joinedAt: 1 }));
  });
  test('canais sem cargo continuam visíveis para todos', async () => {
    await assertSucceeds(get(ref(member(), 'rooms/geral/members')));
  });
  test('ganhar o cargo libera a categoria na hora', async () => {
    await assertSucceeds(set(ref(owner(), 'community/memberRoles/member/trampo'), true));
    await assertSucceeds(get(ref(member(), 'community/categories/grupoB')));
    await assertSucceeds(get(ref(member(), 'rooms/canalB/members')));
  });
  test('canal pode exigir um cargo próprio dentro da categoria', async () => {
    await env.withSecurityRulesDisabled((ctx) =>
      set(ref(ctx.database(), 'community/channelRoles/canalA'), ['trampo']),
    );
    await assertSucceeds(get(ref(member(), 'community/categories/grupoA')));
    await assertFails(get(ref(member(), 'community/channels/canalA')));
    await assertFails(get(ref(member(), 'rooms/canalA/members')));
  });
  test('qualquer um dos cargos marcados libera', async () => {
    await env.withSecurityRulesDisabled((ctx) =>
      set(ref(ctx.database(), 'community/categoryRoles/grupoB'), ['outro', 'faculdade']),
    );
    await assertSucceeds(get(ref(member(), 'community/categories/grupoB')));
  });
  test('só o proprietário vê tudo; administrador segue os cargos', async () => {
    await assertFails(get(ref(admin(), 'community/categories/grupoB')));
    await assertFails(get(ref(admin(), 'community/channels/canalB')));
    await assertFails(get(ref(admin(), 'rooms/canalB/members')));
    await assertFails(set(ref(admin(), 'rooms/canalB/members/admin'), { joinedAt: 1 }));
    await assertSucceeds(get(ref(owner(), 'community/channels/canalA')));
    await assertSucceeds(get(ref(owner(), 'rooms/canalB/members')));
    // Com o cargo, o administrador entra como qualquer pessoa do grupo.
    await assertSucceeds(set(ref(owner(), 'community/memberRoles/admin/trampo'), true));
    await assertSucceeds(get(ref(admin(), 'community/categories/grupoB')));
    await assertSucceeds(get(ref(admin(), 'rooms/canalB/members')));
  });
  test('administrador não mexe no que não enxerga', async () => {
    const channel = {
      name: 'Xis',
      description: '',
      categoryId: 'grupoB',
      createdBy: 'admin',
      createdAt: 1,
      visibility: 'public',
      enabled: true,
    };
    await assertFails(set(ref(admin(), 'community/channels/canalB'), channel));
    await assertFails(update(ref(admin(), 'community/channels/canalB'), { name: 'Hackeado' }));
    await assertFails(update(ref(admin(), 'community'), { 'channels/canalB': null, 'channelIds/canalB': null }));
    await assertFails(remove(ref(admin(), 'community/channelIds/canalB')));
    await assertFails(remove(ref(admin(), 'community/categoryIds/grupoB')));
    await assertFails(update(ref(admin(), 'community/categories/grupoB'), { name: 'Hackeado' }));
    await assertFails(set(ref(admin(), 'community/channelAccess/canalB'), 'admin'));
    await assertFails(set(ref(admin(), 'community/categoryAccess/grupoB'), 'admin'));
    await env.withSecurityRulesDisabled((ctx) =>
      set(ref(ctx.database(), 'rooms/canalB/members/outro'), { joinedAt: 1 }),
    );
    await assertFails(remove(ref(admin(), 'rooms/canalB/members/outro')));
    // O que ele enxerga (ou cria) continua com ele; o proprietário mexe em tudo.
    await assertSucceeds(update(ref(admin(), 'community/channels/staff'), { name: 'Equipe' }));
    await assertSucceeds(
      update(ref(admin(), 'community'), {
        'channels/novo': { ...channel, categoryId: 'lives' },
        'channelIds/novo': true,
      }),
    );
    await assertSucceeds(update(ref(admin(), 'community'), { 'channels/novo': null, 'channelIds/novo': null }));
    await assertSucceeds(update(ref(owner(), 'community/channels/canalB'), { name: 'B2' }));
    await assertSucceeds(remove(ref(owner(), 'rooms/canalB/members/outro')));
  });
  test('um grupo não enxerga quem é do outro nem o nome dele', async () => {
    // Cargos das pessoas: cada um lê só os próprios.
    await assertSucceeds(get(ref(member(), 'community/memberRoles/member')));
    await assertFails(get(ref(member(), 'community/memberRoles/outro')));
    await assertFails(get(ref(member(), 'community/memberRoles')));
    await assertFails(get(ref(admin(), 'community/memberRoles')));
    await assertSucceeds(get(ref(owner(), 'community/memberRoles')));
    // Colegas do próprio grupo, sim; do outro, não.
    await assertSucceeds(get(ref(member(), 'community/roleMembers/faculdade')));
    await assertFails(get(ref(member(), 'community/roleMembers/trampo')));
    await assertFails(get(ref(member(), 'community/roleMembers')));
    await assertFails(get(ref(admin(), 'community/roleMembers/faculdade')));
    await assertSucceeds(get(ref(owner(), 'community/roleMembers')));
    // Nome e cor do cargo: só de quem tem.
    await assertSucceeds(get(ref(member(), 'community/roleDefs/faculdade')));
    await assertFails(get(ref(member(), 'community/roleDefs/trampo')));
    await assertFails(get(ref(member(), 'community/roleDefs')));
    await assertFails(get(ref(admin(), 'community/roleDefs')));
    await assertSucceeds(get(ref(owner(), 'community/roleDefs')));
  });
  test('só o proprietário lista a comunidade; os demais leem uma pessoa por vez', async () => {
    for (const path of ['profiles', 'avatars', 'roles', 'netAdmins']) {
      await assertFails(get(ref(member(), 'community/' + path)));
      await assertFails(get(ref(admin(), 'community/' + path)));
      await assertSucceeds(get(ref(owner(), 'community/' + path)));
    }
    await assertSucceeds(get(ref(member(), 'community/profiles/outro')));
    await assertSucceeds(get(ref(member(), 'community/avatars/outro')));
    await assertFails(get(ref(pending(), 'community/profiles/outro')));
    // Nível e permissão de rede: só os próprios.
    await assertSucceeds(get(ref(member(), 'community/roles/member')));
    await assertFails(get(ref(member(), 'community/roles/admin')));
    await assertSucceeds(get(ref(member(), 'community/netAdmins/member')));
    await assertFails(get(ref(member(), 'community/netAdmins/admin')));
  });
  test('o índice cargo → pessoas é só do proprietário', async () => {
    await assertFails(set(ref(member(), 'community/roleMembers/trampo/member'), true));
    await assertFails(set(ref(admin(), 'community/roleMembers/trampo/admin'), true));
    await assertFails(set(ref(owner(), 'community/roleMembers/trampo/member'), 'sim'));
    await assertSucceeds(
      update(ref(owner(), 'community'), {
        'memberRoles/member/trampo': true,
        'roleMembers/trampo/member': true,
      }),
    );
    await assertSucceeds(get(ref(member(), 'community/roleMembers/trampo')));
    await assertSucceeds(
      update(ref(owner(), 'community'), { 'roleDefs/trampo': null, 'roleMembers/trampo': null }),
    );
  });
  test('aprovar já num grupo e revogar tirando nível e cargos é do proprietário', async () => {
    const approve = {
      'approved/pending': true,
      'roles/pending': null,
      'memberRoles/pending': { trampo: true },
      'roleMembers/trampo/pending': true,
    };
    await assertFails(update(ref(admin(), 'community'), approve));
    await assertSucceeds(update(ref(owner(), 'community'), approve));
    await assertSucceeds(get(ref(pending(), 'community/channels/canalB')));
    await assertFails(get(ref(pending(), 'community/channels/canalA')));
    const revoke = {
      'approved/admin': null,
      'roles/admin': null,
      'netAdmins/admin': null,
      'memberRoles/admin': null,
      'roleMembers/trampo/admin': null,
    };
    await assertFails(update(ref(member(), 'community'), revoke));
    await assertSucceeds(update(ref(owner(), 'community'), revoke));
    await assertFails(get(ref(admin(), 'rooms/geral/members')));
    // O proprietário não revoga a si mesmo.
    await assertFails(update(ref(owner(), 'community'), { 'approved/owner': null, 'roles/owner': null }));
  });
  test('sala de canal apagado pode ser limpa; sala de canal que existe, não', async () => {
    await env.withSecurityRulesDisabled((ctx) =>
      set(ref(ctx.database(), 'rooms/sumiu'), {
        members: { member: { joinedAt: 1 } },
        streams: { member: { screen: { startedAt: 1, streamId: 's9' } } },
      }),
    );
    await assertFails(remove(ref(member(), 'rooms/sumiu')));
    await assertFails(remove(ref(admin(), 'rooms/geral')));
    await assertFails(remove(ref(owner(), 'rooms/geral')));
    await assertFails(remove(ref(owner(), 'rooms/vip')));
    await assertFails(set(ref(admin(), 'rooms/sumiu/members/admin'), { joinedAt: 1 }));
    await assertSucceeds(remove(ref(admin(), 'rooms/sumiu')));
  });
  test('somente o proprietário cria e atribui cargos', async () => {
    const role = { name: 'Novo', color: '#ff0000', createdAt: 1 };
    await assertFails(set(ref(member(), 'community/memberRoles/member/trampo'), true));
    await assertFails(set(ref(admin(), 'community/memberRoles/member/trampo'), true));
    await assertFails(set(ref(admin(), 'community/roleDefs/novo'), role));
    await assertFails(set(ref(admin(), 'community/categoryRoles/grupoA'), ['trampo']));
    await assertSucceeds(set(ref(owner(), 'community/roleDefs/novo'), role));
    await assertSucceeds(set(ref(owner(), 'community/categoryRoles/grupoA'), ['trampo', 'novo']));
  });
  test('validação de cargos', async () => {
    await assertFails(set(ref(owner(), 'community/roleDefs/x'), { name: 'X', color: 'roxo', createdAt: 1 }));
    await assertFails(
      set(ref(owner(), 'community/roleDefs/x'), { name: 'Ok', color: '#123456', createdAt: 1, admin: true }),
    );
    await assertFails(set(ref(owner(), 'community/categoryRoles/grupoA'), { 7: 'trampo' }));
    await assertFails(set(ref(owner(), 'community/memberRoles/member/trampo'), 'sim'));
  });
});
