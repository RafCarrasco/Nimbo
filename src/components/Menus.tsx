import { Eye, FolderPlus, Pencil, Shield, Tag, Trash2, UserMinus, UserX } from 'lucide-react';
import { Avatar } from './Avatar';
import {
  assignRole,
  isStaff,
  isSystemChannel,
  roleLabel,
  setMemberRole,
  DEFAULT_CATEGORY_ID,
  type CommunityRole,
} from '../community';
import type { CommunityData } from '../hooks/useCommunity';
import type { MenuItem } from '../ui/ContextMenu';
import type { SettingsTarget } from './ChannelSettings';

// O que foi clicado com o botão direito.
export type MenuTarget =
  | { kind: 'member'; memberId: string; channelId?: string; live?: boolean }
  | { kind: 'channel'; id: string }
  | { kind: 'category'; id: string };

export type MenuActions = {
  watch: (channelId: string, memberId: string) => void;
  settings: (target: SettingsTarget) => void;
  createChannel: (categoryId: string) => void;
  removeSession: (channelId: string, memberId: string) => void;
  revoke: (memberId: string) => void;
  manageRoles: () => void;
  toast: { success: (text: string) => void; error: (e: unknown) => void };
};

// Itens do menu, montados a cada desenho com os dados atuais (marcar um cargo atualiza a caixa sem
// fechar o menu). Cargos só existem para o proprietário: para os demais, nada sobre eles aparece.
export function menuItems(target: MenuTarget, uid: string, data: CommunityData, act: MenuActions): MenuItem[] {
  const owner = data.roles[uid] === 'owner';
  const staff = isStaff(data.roles[uid] || 'member');
  if (target.kind === 'member') return memberItems(target, uid, data, act, owner, staff);
  if (!staff) return [];
  if (target.kind === 'channel') {
    const channel = data.channels[target.id];
    if (!channel) return [];
    const fixed = isSystemChannel(target.id);
    return [
      { kind: 'label', label: channel.name },
      { label: 'Editar canal', icon: <Pencil size={15} />, onSelect: () => act.settings({ kind: 'channel', id: target.id }) },
      {
        label: 'Permissões',
        icon: <Shield size={15} />,
        onSelect: () => act.settings({ kind: 'channel', id: target.id, tab: 'permissions' }),
      },
      ...(fixed
        ? []
        : ([
            { kind: 'separator' },
            {
              label: 'Excluir canal',
              icon: <Trash2 size={15} />,
              danger: true,
              onSelect: () => act.settings({ kind: 'channel', id: target.id, tab: 'overview' }),
              hint: 'Abre as configurações do canal, com o botão de excluir',
            },
          ] as MenuItem[])),
    ];
  }
  const category = data.categories[target.id];
  if (!category) return [];
  return [
    { kind: 'label', label: category.name },
    { label: 'Editar categoria', icon: <Pencil size={15} />, onSelect: () => act.settings({ kind: 'category', id: target.id }) },
    { label: 'Criar canal', icon: <FolderPlus size={15} />, onSelect: () => act.createChannel(target.id) },
    {
      label: 'Permissões',
      icon: <Shield size={15} />,
      onSelect: () => act.settings({ kind: 'category', id: target.id, tab: 'permissions' }),
    },
    ...(target.id === DEFAULT_CATEGORY_ID
      ? []
      : ([
          { kind: 'separator' },
          {
            label: 'Excluir categoria',
            icon: <Trash2 size={15} />,
            danger: true,
            onSelect: () => act.settings({ kind: 'category', id: target.id }),
            hint: 'Abre as configurações da categoria, com o botão de excluir',
          },
        ] as MenuItem[])),
  ];
}

function memberItems(
  target: Extract<MenuTarget, { kind: 'member' }>,
  uid: string,
  data: CommunityData,
  act: MenuActions,
  owner: boolean,
  staff: boolean,
): MenuItem[] {
  const { memberId, channelId, live } = target;
  const name = data.profiles[memberId]?.displayName;
  const level: CommunityRole | undefined = data.roles[memberId];
  const self = memberId === uid;
  // Cor do nome: só o proprietário vê. Os cargos aparecem como caixas de marcar logo abaixo (etiquetas no
  // cabeçalho mudariam a altura do menu a cada clique e o próximo clique cairia no cargo errado).
  const mine = owner ? Object.keys(data.memberRoles[memberId] || {}).filter((r) => data.roleDefs[r]) : [];
  const color = owner && mine.length ? data.roleDefs[mine[0]].color : undefined;
  const items: MenuItem[] = [
    {
      kind: 'custom',
      node: (
        <div className="menu-profile">
          <Avatar photo={data.avatars[memberId]} name={name || '?'} />
          <span className="menu-profile-text">
            <strong style={{ color }}>
              {name || 'Sessão sem perfil'}
              {self && <span className="muted"> (você)</span>}
            </strong>
            {level && <small className="muted">{roleLabel[level]}</small>}
          </span>
        </div>
      ),
    },
  ];
  if (live && channelId && !self)
    items.push({ label: 'Assistir live', icon: <Eye size={15} />, onSelect: () => act.watch(channelId, memberId) });

  const manageable = owner && !self && level !== 'owner' && Boolean(name);
  if (manageable) {
    const roles = Object.entries(data.roleDefs).sort((a, b) => a[1].createdAt - b[1].createdAt);
    items.push({ kind: 'separator' }, { kind: 'label', label: 'CARGOS' });
    if (roles.length === 0)
      items.push({ label: 'Criar o primeiro cargo', icon: <Tag size={15} />, onSelect: act.manageRoles });
    for (const [roleId, def] of roles) {
      const on = Boolean(data.memberRoles[memberId]?.[roleId]);
      items.push({
        label: def.name,
        color: def.color,
        checked: on,
        onSelect: () =>
          void setMemberRole(memberId, roleId, !on).then(
            () => act.toast.success(on ? `${def.name} removido de ${name}` : `${name} agora é ${def.name}`),
            act.toast.error,
          ),
      });
    }
    if (roles.length > 0)
      items.push({ label: 'Gerenciar cargos…', icon: <Tag size={15} />, onSelect: act.manageRoles });
    items.push({ kind: 'separator' }, { kind: 'label', label: 'NÍVEL' });
    for (const [value, label] of [
      ['member', 'Membro'],
      ['admin', 'Administrador'],
    ] as Array<[CommunityRole, string]>)
      items.push({
        label,
        checked: (level || 'member') === value,
        onSelect: () =>
          (level || 'member') !== value &&
          void assignRole(memberId, value).then(() => act.toast.success(`${name} agora é ${label.toLowerCase()}`), act.toast.error),
      });
    items.push(
      { kind: 'separator' },
      { label: 'Revogar acesso', icon: <UserX size={15} />, danger: true, onSelect: () => act.revoke(memberId) },
    );
  }
  // Sessão sem perfil (abandonada): a equipe pode tirar do canal.
  if (staff && !name && channelId)
    items.push(
      { kind: 'separator' },
      {
        label: 'Remover sessão',
        icon: <UserMinus size={15} />,
        danger: true,
        onSelect: () => act.removeSession(channelId, memberId),
      },
    );
  return items;
}
