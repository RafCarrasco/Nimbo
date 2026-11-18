import { useEffect, useMemo, useState } from 'react';
import { Check, Hash, Lock, LoaderCircle, Settings, Shield, Trash2, Users, X } from 'lucide-react';
import { Avatar } from './Avatar';
import {
  categoryAllowed,
  channelAllowed,
  deleteCategory,
  deleteChannel,
  editChannel,
  isSystemChannel,
  renameCategory,
  setCategoryAccess,
  setCategoryRoles,
  setChannelAccess,
  setChannelRoles,
  watchApprovedMembers,
  DEFAULT_CATEGORY_ID,
  ROLE_SLOTS,
  type AccessLevel,
} from '../community';
import type { CommunityData } from '../hooks/useCommunity';
import { useDialogs } from '../ui/Modal';
import { useToast } from '../ui/Toast';

export type SettingsTarget = { kind: 'category' | 'channel'; id: string; tab?: 'overview' | 'permissions' };

type Draft = {
  name: string;
  description: string;
  categoryId: string;
  level: AccessLevel;
  private: boolean;
  roles: string[];
};

const LEVELS: Array<[AccessLevel, string]> = [
  ['member', 'Todos'],
  ['admin', 'Administradores'],
  ['owner', 'Só o proprietário'],
];

// Configurações de um canal ou categoria, no estilo do Discord: navegação à esquerda, alterações
// guardadas num rascunho e uma barra para salvar ou desfazer. Quem garante o acesso são as regras do
// banco; aqui só aparece o que a pessoa pode mudar. Cargos são só do proprietário: para os demais,
// nada sobre cargos aparece (ninguém deve saber que existem outros grupos).
export function ChannelSettings({
  uid,
  data,
  target,
  onClose,
}: {
  uid: string;
  data: CommunityData;
  target: SettingsTarget;
  onClose: () => void;
}) {
  const toast = useToast();
  const dialogs = useDialogs();
  const owner = data.roles[uid] === 'owner';
  const { kind, id } = target;
  const channel = kind === 'channel' ? data.channels[id] : undefined;
  const category = kind === 'category' ? data.categories[id] : undefined;
  const fixed = kind === 'channel' ? isSystemChannel(id) : id === DEFAULT_CATEGORY_ID;
  const kindName = kind === 'channel' ? 'canal' : 'categoria';

  const saved = useMemo<Draft>(() => {
    const roles = (kind === 'channel' ? data.channelRoles : data.categoryRoles)[id] || [];
    return {
      name: channel?.name ?? category?.name ?? '',
      description: channel?.description ?? '',
      categoryId: channel?.categoryId || DEFAULT_CATEGORY_ID,
      level: (kind === 'channel' ? data.channelAccess : data.categoryAccess)[id] || 'member',
      private: roles.length > 0,
      roles,
    };
  }, [kind, id, channel, category, data.channelRoles, data.categoryRoles, data.channelAccess, data.categoryAccess]);
  const [draft, setDraft] = useState<Draft>(saved);
  const [tab, setTab] = useState(target.tab ?? 'overview');
  const [saving, setSaving] = useState(false);
  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((old) => ({ ...old, [key]: value }));

  // Quem está aprovado, para mostrar quem vê com o rascunho aplicado (só o proprietário lê a lista).
  const [approved, setApproved] = useState<Record<string, boolean>>({});
  useEffect(() => (owner ? watchApprovedMembers(setApproved, () => {}) : undefined), [owner]);

  const roles = Object.entries(data.roleDefs).sort((a, b) => a[1].createdAt - b[1].createdAt);
  const draftRoles = draft.private ? draft.roles : [];
  const changed =
    draft.name.trim() !== saved.name ||
    draft.description.trim() !== saved.description ||
    draft.categoryId !== saved.categoryId ||
    draft.level !== saved.level ||
    draftRoles.join('|') !== saved.roles.join('|');
  const nameError = draft.name.trim().length < 2 || draft.name.trim().length > 32 ? 'Use de 2 a 32 caracteres.' : '';
  const privateError = draft.private && draft.roles.length === 0 ? 'Marque pelo menos um cargo.' : '';
  const blocked = (!fixed && nameError) || (owner && privateError);

  // Quem vê com o rascunho aplicado: a mesma conta que as regras fazem.
  const viewers = useMemo(() => {
    if (!owner) return [];
    const sim = {
      ...data,
      channelAccess: kind === 'channel' ? { ...data.channelAccess, [id]: draft.level } : data.channelAccess,
      categoryAccess: kind === 'category' ? { ...data.categoryAccess, [id]: draft.level } : data.categoryAccess,
      channelRoles: kind === 'channel' ? { ...data.channelRoles, [id]: draftRoles } : data.channelRoles,
      categoryRoles: kind === 'category' ? { ...data.categoryRoles, [id]: draftRoles } : data.categoryRoles,
      channels:
        kind === 'channel' && channel ? { ...data.channels, [id]: { ...channel, categoryId: draft.categoryId } } : data.channels,
    };
    return Object.keys(approved)
      .filter((member) => approved[member])
      .filter((member) => (kind === 'channel' ? channelAllowed(id, member, sim) : categoryAllowed(id, member, sim)))
      .sort((a, b) => (data.profiles[a]?.displayName || '').localeCompare(data.profiles[b]?.displayName || ''));
    // draftRoles muda de identidade a cada desenho; o conteúdo é o que importa.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [owner, data, kind, id, draft.level, draftRoles.join('|'), draft.categoryId, approved, channel]);

  const close = async () => {
    if (changed) {
      const ok = await dialogs.confirm({
        title: 'Sair sem salvar?',
        message: 'As alterações deste ' + kindName + ' ainda não foram salvas.',
        confirmLabel: 'Sair sem salvar',
        danger: true,
      });
      if (!ok) return;
    }
    onClose();
  };
  useEffect(() => {
    const key = (e: KeyboardEvent) => e.key === 'Escape' && void close();
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  });

  const save = async () => {
    if (blocked) return;
    setSaving(true);
    try {
      const name = draft.name.trim();
      if (kind === 'channel' && !fixed) {
        const fields: Parameters<typeof editChannel>[1] = {};
        if (name !== saved.name) fields.name = name;
        if (draft.description.trim() !== saved.description) fields.description = draft.description.trim().slice(0, 160);
        if (draft.categoryId !== saved.categoryId) fields.categoryId = draft.categoryId;
        if (Object.keys(fields).length) await editChannel(id, fields);
      }
      if (kind === 'category' && !fixed && name !== saved.name) await renameCategory(id, name);
      if (draft.level !== saved.level)
        await (kind === 'channel' ? setChannelAccess(id, draft.level) : setCategoryAccess(id, draft.level));
      if (owner && draftRoles.join('|') !== saved.roles.join('|'))
        await (kind === 'channel' ? setChannelRoles(id, draftRoles) : setCategoryRoles(id, draftRoles));
      toast.success('Alterações salvas');
    } catch (e) {
      toast.error(e);
    } finally {
      setSaving(false);
    }
  };

  // Depois de salvar, o rascunho acompanha o que o banco devolveu.
  useEffect(() => {
    if (!saving) setDraft(saved);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [saved]);

  const remove = async () => {
    const children = kind === 'category' ? Object.values(data.channels).filter((c) => (c.categoryId || DEFAULT_CATEGORY_ID) === id) : [];
    if (children.length) {
      toast.info('Mova ou exclua os canais desta categoria antes.');
      return;
    }
    const ok = await dialogs.confirm({
      title: kind === 'channel' ? 'Excluir canal?' : 'Excluir categoria?',
      message:
        kind === 'channel'
          ? `${saved.name} some para todos. Quem estiver nele será desconectado.`
          : `${saved.name} some para todos.`,
      confirmLabel: 'Excluir',
      danger: true,
    });
    if (!ok) return;
    try {
      await (kind === 'channel' ? deleteChannel(id) : deleteCategory(id));
      toast.success(kind === 'channel' ? 'Canal excluído' : 'Categoria excluída');
      onClose();
    } catch (e) {
      toast.error(e);
    }
  };

  if (!channel && !category) return null;
  const levelLocked = !owner && saved.level === 'owner';

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && void close()}>
      <section className="modal settings-modal" role="dialog" aria-modal="true" aria-label={'Configurações de ' + saved.name}>
        <nav className="settings-nav" aria-label="Seções">
          <span className="settings-nav-title">
            {kind === 'channel' ? <Hash size={13} /> : null}
            {saved.name}
            <small>{kind === 'channel' ? 'CANAL' : 'CATEGORIA'}</small>
          </span>
          <button className={tab === 'overview' ? 'on' : ''} onClick={() => setTab('overview')}>
            <Settings size={15} /> Visão geral
          </button>
          <button className={tab === 'permissions' ? 'on' : ''} onClick={() => setTab('permissions')}>
            <Shield size={15} /> Permissões
          </button>
          {!fixed && (
            <button className="danger push-bottom" onClick={() => void remove()}>
              <Trash2 size={15} /> Excluir {kindName}
            </button>
          )}
        </nav>
        <div className="settings-main">
          <header className="settings-head">
            <h2>{tab === 'overview' ? 'Visão geral' : 'Permissões'}</h2>
            <button className="icon-button" aria-label="Fechar" onClick={() => void close()}>
              <X size={18} />
            </button>
          </header>
          <div className="settings-body">
            {tab === 'overview' && (
              <>
                <label className="field">
                  <span>NOME D{kind === 'channel' ? 'O CANAL' : 'A CATEGORIA'}</span>
                  <input
                    value={draft.name}
                    maxLength={32}
                    disabled={fixed}
                    onChange={(e) => set('name', e.target.value)}
                  />
                  {fixed ? (
                    <small className="muted">
                      {kind === 'channel' ? 'Canais padrão' : 'A categoria padrão'} não mudam de nome.
                    </small>
                  ) : (
                    nameError && <small className="field-error">{nameError}</small>
                  )}
                </label>
                {kind === 'channel' && (
                  <label className="field">
                    <span>DESCRIÇÃO</span>
                    <textarea
                      rows={3}
                      maxLength={160}
                      disabled={fixed}
                      placeholder="Sobre o que é este canal?"
                      value={draft.description}
                      onChange={(e) => set('description', e.target.value)}
                    />
                  </label>
                )}
                {kind === 'channel' && !fixed && (
                  <label className="field">
                    <span>CATEGORIA</span>
                    <select value={draft.categoryId} onChange={(e) => set('categoryId', e.target.value)}>
                      {Object.entries(data.categories)
                        .sort((a, b) => a[1].order - b[1].order)
                        .map(([cid, cat]) => (
                          <option key={cid} value={cid}>
                            {cat.name}
                          </option>
                        ))}
                    </select>
                  </label>
                )}
              </>
            )}

            {tab === 'permissions' && (
              <>
                {owner && (
                  <div className="settings-card">
                    <label className="switch-row">
                      <span className="switch-text">
                        <strong>
                          <Lock size={14} /> {kind === 'channel' ? 'Canal privado' : 'Categoria privada'}
                        </strong>
                        <small className="muted">
                          Só quem tem um dos cargos marcados vê{' '}
                          {kind === 'channel' ? 'este canal' : 'esta categoria e os canais dela'}. Os demais nem
                          sabem que existe.
                        </small>
                      </span>
                      <input
                        type="checkbox"
                        className="switch"
                        checked={draft.private}
                        onChange={(e) => set('private', e.target.checked)}
                      />
                    </label>
                    {draft.private && (
                      <div className="settings-roles">
                        <span className="option-label">QUEM PODE VER</span>
                        {roles.length === 0 && (
                          <small className="muted">
                            Nenhum cargo criado ainda. Crie em Gerenciar comunidade → Cargos.
                          </small>
                        )}
                        {roles.map(([roleId, def]) => {
                          const on = draft.roles.includes(roleId);
                          const full = !on && draft.roles.length >= ROLE_SLOTS;
                          const count = Object.values(data.memberRoles).filter((r) => r[roleId]).length;
                          return (
                            <button
                              key={roleId}
                              className={'role-row' + (on ? ' on' : '')}
                              style={{ '--role': def.color } as React.CSSProperties}
                              disabled={full}
                              aria-pressed={on}
                              title={full ? `No máximo ${ROLE_SLOTS} cargos` : undefined}
                              onClick={() =>
                                set('roles', on ? draft.roles.filter((r) => r !== roleId) : [...draft.roles, roleId])
                              }
                            >
                              <span className="role-dot" />
                              <span className="role-row-name">{def.name}</span>
                              <small className="muted">{count === 1 ? '1 pessoa' : `${count} pessoas`}</small>
                              <span className={'menu-check' + (on ? ' on' : '')}>{on && <Check size={13} />}</span>
                            </button>
                          );
                        })}
                        {privateError && <small className="field-error">{privateError}</small>}
                      </div>
                    )}
                  </div>
                )}

                <div className="option-group">
                  <span className="option-label">NÍVEL MÍNIMO</span>
                  <div className="segmented wide">
                    {LEVELS.filter(([level]) => owner || level !== 'owner' || saved.level === 'owner').map(
                      ([level, label]) => (
                        <button
                          key={level}
                          className={draft.level === level ? 'on' : ''}
                          disabled={levelLocked || (!owner && level === 'owner')}
                          onClick={() => set('level', level)}
                        >
                          {label}
                        </button>
                      ),
                    )}
                  </div>
                  <small className="muted">
                    {levelLocked
                      ? 'Só o proprietário muda esta opção.'
                      : draft.level === 'member'
                        ? 'Todos os membros aprovados podem ver e entrar.'
                        : draft.level === 'admin'
                          ? 'Só administradores e o proprietário.'
                          : 'Só você.'}
                    {kind === 'channel' && ' O canal também segue as permissões da categoria.'}
                  </small>
                </div>

                {owner && (
                  <div className="option-group">
                    <span className="option-label">
                      <Users size={12} /> QUEM VÊ COM ESTAS PERMISSÕES ({viewers.length})
                    </span>
                    <div className="viewer-list">
                      {viewers.slice(0, 24).map((member) => (
                        <span className="viewer-chip" key={member} title={data.profiles[member]?.displayName}>
                          <Avatar photo={data.avatars[member]} name={data.profiles[member]?.displayName || '?'} small />
                          {member === uid ? 'Você' : data.profiles[member]?.displayName || 'Sem nickname'}
                        </span>
                      ))}
                      {viewers.length > 24 && <span className="muted">+{viewers.length - 24}</span>}
                    </div>
                  </div>
                )}
              </>
            )}
          </div>
          {changed && (
            <div className="save-bar" role="status">
              <span>Cuidado: há alterações não salvas.</span>
              <button className="button ghost" disabled={saving} onClick={() => setDraft(saved)}>
                Desfazer
              </button>
              <button className="button primary" disabled={saving || Boolean(blocked)} onClick={() => void save()}>
                {saving && <LoaderCircle size={15} className="spin" />} Salvar alterações
              </button>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
