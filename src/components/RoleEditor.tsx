import { useEffect, useMemo, useState } from 'react';
import { Check, Hash, LoaderCircle, Plus, Search, Trash2, X } from 'lucide-react';
import { Avatar } from './Avatar';
import {
  createRole,
  deleteRole,
  setCategoryRoles,
  setChannelRoles,
  setMemberRole,
  updateRole,
  DEFAULT_CATEGORY_ID,
  ROLE_COLORS,
  ROLE_SLOTS,
} from '../community';
import type { CommunityData } from '../hooks/useCommunity';
import { useDialogs } from '../ui/Modal';
import { useToast } from '../ui/Toast';

// Mais duas cores além das de criação automática, para a paleta ter dez como a do Discord.
const PALETTE = [...ROLE_COLORS, '#e879f9', '#fb923c'];
const nameRule = (v: string) => (v.trim().length < 2 || v.trim().length > 24 ? 'Use de 2 a 24 caracteres.' : '');

// Cargos no estilo do Discord: lista à esquerda, editor à direita (Exibição, Acesso, Membros).
// Só o proprietário chega aqui: para os demais o sistema de cargos não existe na tela.
export function RoleEditor({
  data,
  members,
  nameOf,
  initial,
}: {
  data: CommunityData;
  // Pessoas aprovadas (sem o proprietário), já em ordem alfabética.
  members: string[];
  nameOf: (id: string) => string;
  initial?: string;
}) {
  const toast = useToast();
  const dialogs = useDialogs();
  const roles = Object.entries(data.roleDefs).sort((a, b) => a[1].createdAt - b[1].createdAt);
  const [selected, setSelected] = useState(initial || roles[0]?.[0] || '');
  const [tab, setTab] = useState<'display' | 'access' | 'members'>('display');
  const [busy, setBusy] = useState('');
  const current = data.roleDefs[selected] ? selected : roles[0]?.[0] || '';
  const def = data.roleDefs[current];
  const holders = (roleId: string) => members.filter((m) => data.memberRoles[m]?.[roleId]);

  const run = async (key: string, action: () => Promise<unknown>, done?: string) => {
    setBusy(key);
    try {
      await action();
      if (done) toast.success(done);
    } catch (e) {
      toast.error(e);
    } finally {
      setBusy('');
    }
  };

  const create = async () => {
    const name = await dialogs.prompt({
      title: 'Criar cargo',
      label: 'Nome do cargo (ex.: Faculdade, Trampo)',
      validate: nameRule,
      confirmLabel: 'Criar',
    });
    if (!name) return;
    await run('create', async () => {
      const id = await createRole(name, PALETTE[roles.length % PALETTE.length]);
      setSelected(id);
      setTab('members');
    }, 'Cargo criado. Agora coloque as pessoas.');
  };

  return (
    <div className="role-editor">
      <aside className="role-list">
        <button className="button primary" disabled={busy !== ''} onClick={() => void create()}>
          <Plus size={16} /> Criar cargo
        </button>
        {roles.length === 0 && (
          <p className="muted role-empty">
            Cargos separam grupos de amigos: quem é de um não vê os canais, as pessoas nem o nome do outro. Só
            você vê os cargos.
          </p>
        )}
        {roles.map(([roleId, role]) => (
          <button
            key={roleId}
            className={'role-list-item' + (roleId === current ? ' on' : '')}
            style={{ '--role': role.color } as React.CSSProperties}
            onClick={() => setSelected(roleId)}
          >
            <span className="role-dot" />
            <span className="role-row-name">{role.name}</span>
            <small className="muted">{holders(roleId).length}</small>
          </button>
        ))}
      </aside>
      {def ? (
        <section className="role-pane" key={current}>
          <header className="role-pane-head">
            <h3 style={{ color: def.color }}>{def.name}</h3>
            <div className="segmented">
              <button className={tab === 'display' ? 'on' : ''} onClick={() => setTab('display')}>
                Exibição
              </button>
              <button className={tab === 'access' ? 'on' : ''} onClick={() => setTab('access')}>
                Acesso
              </button>
              <button className={tab === 'members' ? 'on' : ''} onClick={() => setTab('members')}>
                Membros ({holders(current).length})
              </button>
            </div>
          </header>
          {tab === 'display' && <DisplayTab roleId={current} name={def.name} color={def.color} />}
          {tab === 'access' && (
            <AccessTab data={data} roleId={current} busy={busy} run={run} />
          )}
          {tab === 'members' && (
            <MembersTab data={data} roleId={current} members={members} nameOf={nameOf} busy={busy} run={run} />
          )}
          <footer className="role-pane-foot">
            <button
              className="button ghost danger-text"
              disabled={busy !== ''}
              onClick={async () => {
                const ok = await dialogs.confirm({
                  title: `Excluir ${def.name}?`,
                  message: `${def.name} sai de todas as pessoas, categorias e canais. Áreas que só esse cargo via passam a ser visíveis para todos.`,
                  confirmLabel: 'Excluir cargo',
                  danger: true,
                });
                if (ok)
                  await run(
                    'delete',
                    () => deleteRole(current, data.memberRoles, data.categoryRoles, data.channelRoles),
                    'Cargo excluído',
                  );
              }}
            >
              <Trash2 size={15} /> Excluir cargo
            </button>
          </footer>
        </section>
      ) : (
        <section className="role-pane role-pane-empty">
          <p className="muted">Crie um cargo para começar.</p>
        </section>
      )}
    </div>
  );
}

function DisplayTab({ roleId, name, color }: { roleId: string; name: string; color: string }) {
  const toast = useToast();
  const [draft, setDraft] = useState({ name, color });
  const [saving, setSaving] = useState(false);
  useEffect(() => setDraft({ name, color }), [name, color]);
  const changed = draft.name.trim() !== name || draft.color.toLowerCase() !== color.toLowerCase();
  const error = nameRule(draft.name);
  const valid = /^#[0-9a-fA-F]{6}$/.test(draft.color);
  return (
    <div className="role-pane-body">
      <label className="field">
        <span>NOME DO CARGO</span>
        <input value={draft.name} maxLength={24} onChange={(e) => setDraft((d) => ({ ...d, name: e.target.value }))} />
        {error && <small className="field-error">{error}</small>}
      </label>
      <div className="field">
        <span>COR DO CARGO</span>
        <small className="muted">É a cor do nome das pessoas com este cargo. Só você vê.</small>
        <div className="palette">
          {PALETTE.map((swatch) => (
            <button
              key={swatch}
              className={'palette-swatch' + (draft.color.toLowerCase() === swatch ? ' on' : '')}
              style={{ background: swatch }}
              aria-label={'Cor ' + swatch}
              onClick={() => setDraft((d) => ({ ...d, color: swatch }))}
            >
              {draft.color.toLowerCase() === swatch && <Check size={14} />}
            </button>
          ))}
          <label className="palette-custom" title="Outra cor">
            <input
              type="color"
              value={valid ? draft.color : '#7c6cff'}
              onChange={(e) => setDraft((d) => ({ ...d, color: e.target.value }))}
            />
          </label>
        </div>
        <span className="role-preview" style={{ color: valid ? draft.color : undefined }}>
          {draft.name.trim() || 'Nome do cargo'} · assim aparece o nome
        </span>
      </div>
      {changed && (
        <div className="save-bar inline" role="status">
          <span>Alterações não salvas.</span>
          <button className="button ghost" disabled={saving} onClick={() => setDraft({ name, color })}>
            Desfazer
          </button>
          <button
            className="button primary"
            disabled={saving || Boolean(error) || !valid}
            onClick={() => {
              setSaving(true);
              void updateRole(roleId, { name: draft.name.trim(), color: draft.color.toLowerCase() })
                .then(() => toast.success('Cargo atualizado'), (e) => toast.error(e))
                .finally(() => setSaving(false));
            }}
          >
            {saving && <LoaderCircle size={15} className="spin" />} Salvar alterações
          </button>
        </div>
      )}
    </div>
  );
}

function AccessTab({
  data,
  roleId,
  busy,
  run,
}: {
  data: CommunityData;
  roleId: string;
  busy: string;
  run: (key: string, action: () => Promise<unknown>, done?: string) => Promise<void>;
}) {
  const toggle = (kind: 'category' | 'channel', id: string) => {
    const lists = kind === 'category' ? data.categoryRoles : data.channelRoles;
    const list = lists[id] || [];
    const on = list.includes(roleId);
    if (!on && list.length >= ROLE_SLOTS) return;
    const next = on ? list.filter((r) => r !== roleId) : [...list, roleId];
    void run(
      kind + id,
      () => (kind === 'category' ? setCategoryRoles(id, next) : setChannelRoles(id, next)),
      on ? 'Acesso removido' : 'Acesso liberado',
    );
  };
  const box = (kind: 'category' | 'channel', id: string) => {
    const list = (kind === 'category' ? data.categoryRoles : data.channelRoles)[id] || [];
    const on = list.includes(roleId);
    const full = !on && list.length >= ROLE_SLOTS;
    return { on, full, others: list.filter((r) => r !== roleId).map((r) => data.roleDefs[r]?.name).filter(Boolean) };
  };
  const categories = Object.entries(data.categories).sort((a, b) => a[1].order - b[1].order);
  return (
    <div className="role-pane-body">
      <small className="muted">
        Marque onde este cargo entra. Marcar deixa o lugar <strong>privado</strong>: só quem tem um dos cargos
        marcados nele passa a ver. Lugar sem nenhum cargo marcado continua aberto a todos.
      </small>
      {categories.map(([categoryId, category]) => {
        const c = box('category', categoryId);
        const channels = Object.entries(data.channels).filter(
          ([, ch]) => (ch.categoryId || DEFAULT_CATEGORY_ID) === categoryId,
        );
        return (
          <div className="access-group" key={categoryId}>
            <button
              className={'access-row category' + (c.on ? ' on' : '')}
              disabled={busy !== '' || c.full}
              onClick={() => toggle('category', categoryId)}
            >
              <span className="access-name">{category.name.toUpperCase()}</span>
              <small className="muted">
                {c.others.length ? 'também: ' + c.others.join(', ') : c.on ? 'privada' : 'aberta a todos'}
              </small>
              <span className={'menu-check' + (c.on ? ' on' : '')}>{c.on && <Check size={13} />}</span>
            </button>
            {channels.map(([channelId, channel]) => {
              const ch = box('channel', channelId);
              return (
                <button
                  key={channelId}
                  className={'access-row' + (ch.on ? ' on' : '')}
                  disabled={busy !== '' || ch.full}
                  onClick={() => toggle('channel', channelId)}
                >
                  <span className="access-name">
                    <Hash size={13} /> {channel.name}
                  </span>
                  <small className="muted">
                    {ch.others.length ? 'também: ' + ch.others.join(', ') : ch.on ? 'privado' : ''}
                  </small>
                  <span className={'menu-check' + (ch.on ? ' on' : '')}>{ch.on && <Check size={13} />}</span>
                </button>
              );
            })}
          </div>
        );
      })}
    </div>
  );
}

function MembersTab({
  data,
  roleId,
  members,
  nameOf,
  busy,
  run,
}: {
  data: CommunityData;
  roleId: string;
  members: string[];
  nameOf: (id: string) => string;
  busy: string;
  run: (key: string, action: () => Promise<unknown>, done?: string) => Promise<void>;
}) {
  const [query, setQuery] = useState('');
  const inside = members.filter((m) => data.memberRoles[m]?.[roleId]);
  const fold = (text: string) => text.normalize('NFD').replace(/\p{Diacritic}/gu, '').toLowerCase();
  const candidates = useMemo(
    () => members.filter((m) => !data.memberRoles[m]?.[roleId] && fold(nameOf(m)).includes(fold(query.trim()))),
    [members, data.memberRoles, roleId, query, nameOf],
  );
  const role = data.roleDefs[roleId];
  return (
    <div className="role-pane-body">
      <label className="search-field">
        <Search size={15} />
        <input placeholder="Adicionar pessoas: procure pelo nome" value={query} onChange={(e) => setQuery(e.target.value)} />
      </label>
      {query.trim() !== '' && (
        <div className="member-pick">
          {candidates.length === 0 && <small className="muted">Ninguém com esse nome fora do cargo.</small>}
          {candidates.slice(0, 8).map((m) => (
            <button
              key={m}
              className="member-row"
              disabled={busy !== ''}
              onClick={() => void run('add' + m, () => setMemberRole(m, roleId, true), `${nameOf(m)} agora é ${role?.name}`)}
            >
              <Avatar photo={data.avatars[m]} name={nameOf(m)} small />
              <span>{nameOf(m)}</span>
              <Plus size={15} className="push-right" />
            </button>
          ))}
        </div>
      )}
      <span className="option-label">
        {inside.length === 0 ? 'NINGUÉM COM ESTE CARGO AINDA' : `COM ESTE CARGO (${inside.length})`}
      </span>
      <div className="member-list">
        {inside.map((m) => (
          <div className="member-row" key={m}>
            <Avatar photo={data.avatars[m]} name={nameOf(m)} small />
            <span style={{ color: role?.color }}>{nameOf(m)}</span>
            <button
              className="icon-button tiny push-right"
              title={'Tirar ' + nameOf(m)}
              aria-label={'Tirar ' + nameOf(m) + ' do cargo'}
              disabled={busy !== ''}
              onClick={() => void run('rm' + m, () => setMemberRole(m, roleId, false), `${nameOf(m)} saiu de ${role?.name}`)}
            >
              {busy === 'rm' + m ? <LoaderCircle size={14} className="spin" /> : <X size={14} />}
            </button>
          </div>
        ))}
      </div>
      {members.length === 0 && <small className="muted">Ainda não há membros aprovados.</small>}
    </div>
  );
}
