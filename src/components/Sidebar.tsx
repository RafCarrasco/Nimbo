import { useEffect, useRef, useState } from 'react';
import {
  ChevronDown,
  ChevronRight,
  Lock,
  MonitorPlay,
  Plus,
  Radio,
  Settings,
  Trash2,
  Video,
} from 'lucide-react';
import { Avatar } from './Avatar';
import { categoryAllowed, channelAllowed, isStaff, loadPreviews, type AccessLevel } from '../community';
import { validPreview } from '../live/preview';
import type { CommunityData, RoomPresence } from '../hooks/useCommunity';
import type { SettingsTarget } from './ChannelSettings';
import type { MenuTarget } from './Menus';

const COLLAPSE_KEY = 'nimbo.collapsed';

// Prévia da live ao lado da barra, enquanto o mouse está em cima de quem transmite (como no Discord).
// A foto é lida do banco só nessa hora.
function LivePeek({ channelId, memberId, name, anchor }: { channelId: string; memberId: string; name: string; anchor: DOMRect }) {
  const [images, setImages] = useState<string[] | null>(null);
  useEffect(() => {
    let alive = true;
    void loadPreviews(channelId, memberId).then((kinds) => {
      if (alive) setImages(['screen', 'camera'].map((kind) => kinds[kind]).filter(validPreview));
    });
    return () => {
      alive = false;
    };
  }, [channelId, memberId]);
  if (!images?.length) return null;
  return (
    <div className="live-peek" style={{ left: anchor.right + 10, top: Math.max(8, Math.min(anchor.top - 8, innerHeight - 190 * images.length)) }}>
      {images.map((src) => (
        <img key={src.slice(-24)} src={src} alt="" />
      ))}
      <small>{name} · clique para assistir</small>
    </div>
  );
}

export function Sidebar({
  uid,
  data,
  presence,
  room,
  disabled,
  adminOpen,
  onJoin,
  onWatch,
  onOpenAdmin,
  onSettings,
  onMenu,
  onCreateChannel,
  onRemoveSession,
  footer,
}: {
  uid: string;
  data: CommunityData;
  presence: Record<string, RoomPresence>;
  room: string;
  disabled: boolean;
  adminOpen: boolean;
  onJoin: (channelId: string) => void;
  onWatch: (channelId: string, memberId: string) => void;
  onOpenAdmin: () => void;
  onSettings: (target: SettingsTarget) => void;
  // Botão direito numa pessoa, canal ou categoria.
  onMenu: (event: React.MouseEvent, target: MenuTarget) => void;
  onCreateChannel: (categoryId: string) => void;
  onRemoveSession: (channelId: string, memberId: string) => void;
  footer: React.ReactNode;
}) {
  const role = data.roles[uid] || 'member';
  const staff = isStaff(role);
  const owner = role === 'owner';
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>(() => {
    try {
      return JSON.parse(localStorage.getItem(COLLAPSE_KEY) || '{}');
    } catch {
      return {};
    }
  });
  // Quem está com o mouse em cima (e ao vivo), para mostrar a prévia depois de um instante.
  const [peek, setPeek] = useState<{ channelId: string; memberId: string; name: string; anchor: DOMRect } | null>(null);
  const peekTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const hover = (target: typeof peek) => {
    clearTimeout(peekTimer.current);
    if (!target) setPeek(null);
    else peekTimer.current = setTimeout(() => setPeek(target), 350);
  };
  useEffect(() => () => clearTimeout(peekTimer.current), []);
  const toggle = (id: string) =>
    setCollapsed((old) => {
      const next = { ...old, [id]: !old[id] };
      try {
        localStorage.setItem(COLLAPSE_KEY, JSON.stringify(next));
      } catch {
        // Preferência apenas local.
      }
      return next;
    });

  const categories = Object.entries(data.categories).sort((a, b) => a[1].order - b[1].order);
  // Texto do cadeado. O proprietário vê todos os cargos; os demais só conhecem o nome dos que têm.
  const restrictedLabel = (level?: AccessLevel, roleIds?: string[]) => {
    if (!owner) return '';
    const names = (roleIds || []).map((r) => data.roleDefs[r]?.name || (owner ? 'removido' : '')).filter(Boolean);
    return [
      level === 'owner' ? 'Somente proprietário' : level === 'admin' ? 'Somente administradores' : '',
      names.length ? 'Cargos: ' + names.join(', ') : roleIds?.length ? 'Restrito por cargo' : '',
    ]
      .filter(Boolean)
      .join(' · ');
  };
  // Cor do primeiro cargo da pessoa, como no Discord.
  const colorOf = (memberId: string) => {
    if (!owner) return undefined;
    const first = Object.keys(data.memberRoles[memberId] || {}).find((r) => data.roleDefs[r]);
    return first ? data.roleDefs[first].color : undefined;
  };

  return (
    <nav className="sidebar" aria-label="Comunidade">
      <header className="sidebar-head">
        <img className="brand-mark" src="/nimbo-icon-128.png" alt="" />
        <strong>Nimbo</strong>
        {(staff || data.netAdmins[uid]) && (
          <button
            className={'icon-button' + (adminOpen ? ' on' : '')}
            aria-label="Gerenciar comunidade"
            title="Gerenciar comunidade"
            onClick={onOpenAdmin}
          >
            <Settings size={18} />
          </button>
        )}
      </header>
      <div className="channel-list">
        {categories.map(([categoryId, category]) => {
          const channels = Object.entries(data.channels).filter(
            ([, c]) => c.enabled && (c.categoryId || 'lives') === categoryId,
          );
          // Só o proprietário vê o que está trancado para ele mesmo. Administrador vê a categoria
          // vazia (para criar canais nela), desde que tenha acesso a ela.
          const visibleChannels = channels.filter(([id]) => owner || channelAllowed(id, uid, data));
          if (!owner && (!categoryAllowed(categoryId, uid, data) || (!staff && !visibleChannels.length)))
            return null;
          const isCollapsed = collapsed[categoryId];
          const categoryRestriction = restrictedLabel(
            data.categoryAccess[categoryId],
            data.categoryRoles[categoryId],
          );
          return (
            <div className="category" key={categoryId}>
              <div
                className="category-row"
                onContextMenu={(e) => staff && onMenu(e, { kind: 'category', id: categoryId })}
              >
                <button
                  className="category-toggle"
                  aria-expanded={!isCollapsed}
                  onClick={() => toggle(categoryId)}
                >
                  {isCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
                  <span>{category.name}</span>
                  {categoryRestriction && <Lock size={11} aria-label={categoryRestriction} />}
                </button>
                {staff && (
                  <span className="row-actions">
                    <button
                      className="icon-button tiny"
                      title={'Criar canal em ' + category.name}
                      aria-label={'Criar canal em ' + category.name}
                      onClick={() => onCreateChannel(categoryId)}
                    >
                      <Plus size={14} />
                    </button>
                    <button
                      className="icon-button tiny"
                      title={'Editar categoria ' + category.name}
                      aria-label={'Editar categoria ' + category.name}
                      onClick={() => onSettings({ kind: 'category', id: categoryId })}
                    >
                      <Settings size={13} />
                    </button>
                  </span>
                )}
              </div>
              {!isCollapsed &&
                visibleChannels.map(([id, channel]) => {
                  const allowed = channelAllowed(id, uid, data);
                  const here = (allowed && presence[id]) || { members: [], streams: {} };
                  const streams = here.streams;
                  // Sem perfil: sessão abandonada, que só a equipe vê (para remover). Enquanto o
                  // perfil ainda está sendo buscado, a pessoa não aparece como abandonada.
                  const members = here.members.filter(
                    (m) => m === uid || data.profiles[m] || (staff && !data.resolving[m]),
                  );
                  const liveCount = members.filter((m) => streams[m]).length;
                  const restriction = restrictedLabel(data.channelAccess[id], data.channelRoles[id]);
                  return (
                    <div className="channel" key={id}>
                      <div
                        className={
                          'channel-row' + (room === id ? ' active' : '') + (allowed ? '' : ' locked')
                        }
                        onContextMenu={(e) => staff && onMenu(e, { kind: 'channel', id })}
                      >
                        <button
                          className="channel-button"
                          disabled={disabled || !allowed}
                          title={allowed ? 'Entrar em ' + channel.name : 'Você não tem acesso a este canal'}
                          onClick={() => onJoin(id)}
                        >
                          {allowed ? <MonitorPlay size={16} /> : <Lock size={15} />}
                          <span className="channel-name">{channel.name}</span>
                          {restriction && allowed && (
                            <Lock size={11} aria-label={restriction} className="muted" />
                          )}
                          {liveCount > 0 && (
                            <span className="live-count" title={`${liveCount} ao vivo`}>
                              <Radio size={11} />
                              {liveCount}
                            </span>
                          )}
                        </button>
                        {staff && (
                          <button
                            className="icon-button tiny row-actions"
                            title={'Editar canal ' + channel.name}
                            aria-label={'Editar canal ' + channel.name}
                            onClick={() => onSettings({ kind: 'channel', id })}
                          >
                            <Settings size={13} />
                          </button>
                        )}
                      </div>
                      {members.length > 0 && (
                        <ul className="participants">
                          {members.map((memberId) => {
                            const profileName = data.profiles[memberId]?.displayName;
                            const name =
                              memberId === uid && profileName ? profileName + ' (você)' : profileName;
                            const kinds = streams[memberId] || {};
                            const stream = Boolean(kinds.screen || kinds.camera);
                            return (
                              <li
                                key={memberId}
                                onContextMenu={(e) => {
                                  hover(null);
                                  onMenu(e, { kind: 'member', memberId, channelId: id, live: stream && allowed });
                                }}
                              >
                                <button
                                  className={'participant' + (stream ? ' is-live' : '')}
                                  disabled={!stream || !allowed}
                                  title={stream ? 'Assistir' : undefined}
                                  onClick={() => {
                                    hover(null);
                                    onWatch(id, memberId);
                                  }}
                                  onMouseEnter={(e) =>
                                    stream &&
                                    allowed &&
                                    hover({
                                      channelId: id,
                                      memberId,
                                      name: profileName || 'Participante',
                                      anchor: e.currentTarget.getBoundingClientRect(),
                                    })
                                  }
                                  onMouseLeave={() => hover(null)}
                                >
                                  <Avatar photo={data.avatars[memberId]} name={name} small />
                                  <span className="participant-name" style={{ color: colorOf(memberId) }}>
                                    {name || 'Sessão sem perfil'}
                                  </span>
                                  {stream && (
                                    <span className="live-pill">
                                      {kinds.screen && <Radio size={10} />}
                                      {kinds.camera && <Video size={10} />}
                                      AO VIVO
                                    </span>
                                  )}
                                </button>
                                {staff && !name && (
                                  <button
                                    className="icon-button tiny"
                                    title="Remover sessão abandonada"
                                    aria-label="Remover sessão abandonada"
                                    onClick={() => onRemoveSession(id, memberId)}
                                  >
                                    <Trash2 size={12} />
                                  </button>
                                )}
                              </li>
                            );
                          })}
                        </ul>
                      )}
                    </div>
                  );
                })}
            </div>
          );
        })}
      </div>
      {footer}
      {peek && <LivePeek {...peek} />}
    </nav>
  );
}
