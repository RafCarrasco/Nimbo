import { useEffect, useMemo, useRef, useState } from 'react';
import { onValue, ref } from 'firebase/database';
import { db } from '../firebase';
import {
  DEFAULT_CATEGORIES,
  SYSTEM_CHANNELS,
  repairRoleMembers,
  watchCategories,
  watchCategoryAccess,
  watchChannelAccess,
  watchChannels,
  watchOne,
  watchProfiles,
  watchRoles,
  watchCategoryRoles,
  watchChannelRoles,
  watchMemberRoles,
  watchAvatars,
  watchNetAdmins,
  watchRoleDefs,
  watchRoleMembers,
  type AccessData,
  type IndexedWatch,
  type AccessLevel,
  type Category,
  type Channel,
  type CommunityRole,
  type Profile,
  type RoleDef,
  type RoleLists,
} from '../community';
import { parseBroadcasts, type Broadcasts } from '../live/room';

export type CommunityData = AccessData & {
  loaded: boolean;
  // Só o proprietário recebe a comunidade inteira; os demais, o que é deles e dos grupos deles.
  owner: boolean;
  categories: Record<string, Category>;
  profiles: Record<string, Profile>;
  roleDefs: Record<string, RoleDef>;
  avatars: Record<string, string>;
  netAdmins: Record<string, true>;
  // Pessoas cujo perfil ainda está sendo buscado (ver usePeople).
  resolving: Record<string, true>;
};

type Flags = Record<string, true>;
const NOBODY: Flags = {};
const patch = <T>(old: Record<string, T>, id: string, value: T | null) => {
  const next = { ...old };
  if (value === null) delete next[id];
  else next[id] = value;
  return next;
};

export function useCommunity(uid: string): CommunityData {
  const [channels, setChannels] = useState<Record<string, Channel>>(SYSTEM_CHANNELS);
  const [categories, setCategories] = useState<Record<string, Category>>(DEFAULT_CATEGORIES);
  const [channelAccess, setChannelAccess] = useState<Record<string, AccessLevel>>({});
  const [categoryAccess, setCategoryAccess] = useState<Record<string, AccessLevel>>({});
  const [categoryRoles, setCategoryRoles] = useState<RoleLists>({});
  const [channelRoles, setChannelRoles] = useState<RoleLists>({});
  // Nível da própria conta: decide se o resto é lido em lista (proprietário) ou item a item.
  const [level, setLevel] = useState<CommunityRole | undefined>(undefined);
  const owner = level === 'owner';
  const [profiles, setProfiles] = useState<Record<string, Profile> | null>(null);
  const [avatars, setAvatars] = useState<Record<string, string>>({});
  const [netAdmins, setNetAdmins] = useState<Flags>({});
  // Proprietário: árvores inteiras. Demais: só os próprios cargos (ids).
  const [allRoles, setAllRoles] = useState<Record<string, CommunityRole>>({});
  const [allDefs, setAllDefs] = useState<Record<string, RoleDef>>({});
  const [allMemberRoles, setAllMemberRoles] = useState<Record<string, Flags> | null>(null);
  const [roleMembers, setRoleMembers] = useState<Record<string, Flags> | null>(null);
  const [ownRoles, setOwnRoles] = useState<Flags>({});

  useEffect(() => {
    const unsubs = [
      watchOne<CommunityRole>(`community/roles/${uid}`, (v) => setLevel(v || 'member')),
      watchChannelAccess(setChannelAccess),
      watchCategoryAccess(setCategoryAccess),
      watchCategoryRoles(setCategoryRoles),
      watchChannelRoles(setChannelRoles),
    ];
    return () => unsubs.forEach((u) => u());
  }, [uid]);

  useEffect(() => {
    if (level === undefined) return;
    const unsubs = owner
      ? [
          watchProfiles(setProfiles),
          watchRoles(setAllRoles),
          watchAvatars(setAvatars),
          watchNetAdmins(setNetAdmins),
          watchRoleDefs(setAllDefs),
          watchMemberRoles(setAllMemberRoles),
          watchRoleMembers(setRoleMembers),
        ]
      : [
          watchOne<Profile>(`community/profiles/${uid}`, (v) => setProfiles(v ? { [uid]: v } : {})),
          watchOne<string>(`community/avatars/${uid}`, (v) => setAvatars(v ? { [uid]: v } : {})),
          watchOne<true>(`community/netAdmins/${uid}`, (v) => setNetAdmins(v ? { [uid]: true } : {})),
          watchOne<Flags>(`community/memberRoles/${uid}`, (v) => setOwnRoles(v || {})),
        ];
    return () => unsubs.forEach((u) => u());
  }, [uid, level, owner]);

  // Proprietário: mantém o índice cargo → pessoas igual aos cargos de cada um.
  useEffect(() => {
    if (owner && allMemberRoles && roleMembers)
      void repairRoleMembers(allMemberRoles, roleMembers).catch(() => {});
  }, [owner, allMemberRoles, roleMembers]);

  const roles = useMemo(
    () => (owner ? allRoles : level ? { [uid]: level } : {}),
    [owner, allRoles, level, uid],
  );
  // Nomes, cores e colegas dos cargos: só o proprietário lê (regras do banco). Os demais sabem só os
  // próprios cargos (ids), que servem para a interface decidir o que mostrar, e nada disso aparece na tela.
  const roleDefs = useMemo(() => (owner ? allDefs : {}), [owner, allDefs]);
  const memberRoles = useMemo(() => {
    if (owner) return allMemberRoles || {};
    return Object.keys(ownRoles).length ? { [uid]: ownRoles } : {};
  }, [owner, allMemberRoles, ownRoles, uid]);

  // O servidor só entrega as categorias e canais que a pessoa pode ver, e um pedido recusado não é
  // refeito sozinho. Quando cargos ou permissões mudam, as leituras são refeitas para refletir o
  // novo acesso na hora (ganhar um cargo mostra a categoria sem recarregar a página).
  const permissions = JSON.stringify([
    level,
    memberRoles[uid],
    channelAccess,
    categoryAccess,
    channelRoles,
    categoryRoles,
  ]);
  const indexed = useRef<IndexedWatch[]>([]);
  useEffect(() => {
    if (level === undefined) return;
    const watches = [watchChannels(setChannels), watchCategories(setCategories)];
    indexed.current = watches;
    return () => {
      indexed.current = [];
      watches.forEach((w) => w.off());
    };
  }, [uid, level]);
  // Mudou alguma permissão: pede de novo só o que tinha sido recusado, sem derrubar o resto.
  useEffect(() => {
    indexed.current.forEach((w) => w.retry());
  }, [permissions]);

  return useMemo(
    () => ({
      loaded: level !== undefined && profiles !== null,
      owner,
      channels,
      categories,
      channelAccess,
      categoryAccess,
      profiles: profiles || {},
      roles,
      roleDefs,
      avatars,
      netAdmins,
      memberRoles,
      categoryRoles,
      channelRoles,
      resolving: NOBODY,
    }),
    [
      level,
      owner,
      channels,
      categories,
      channelAccess,
      categoryAccess,
      profiles,
      roles,
      roleDefs,
      avatars,
      netAdmins,
      memberRoles,
      categoryRoles,
      channelRoles,
    ],
  );
}

// Perfis e fotos de quem aparece nos canais visíveis, um pedido por pessoa. Fora o proprietário,
// ninguém consegue listar a comunidade: só dá para ler quem a pessoa já encontra num canal.
export function usePeople(ids: string[], enabled: boolean) {
  const [profiles, setProfiles] = useState<Record<string, Profile | null>>({});
  const [avatars, setAvatars] = useState<Record<string, string>>({});
  const subs = useRef(new Map<string, () => void>());
  const key = enabled ? [...new Set(ids)].sort().join('|') : '';
  useEffect(() => {
    const wanted = new Set(key ? key.split('|') : []);
    for (const [id, off] of [...subs.current]) {
      if (wanted.has(id)) continue;
      off();
      subs.current.delete(id);
    }
    for (const id of wanted) {
      if (subs.current.has(id)) continue;
      const offs = [
        watchOne<Profile>(`community/profiles/${id}`, (v) => setProfiles((old) => ({ ...old, [id]: v }))),
        watchOne<string>(`community/avatars/${id}`, (v) => setAvatars((old) => patch(old, id, v))),
      ];
      subs.current.set(id, () => offs.forEach((off) => off()));
    }
  }, [key]);
  useEffect(() => {
    const active = subs.current;
    return () => {
      active.forEach((off) => off());
      active.clear();
    };
  }, []);
  // Quem saiu dos canais deixa de ser acompanhado; o que ficou guardado dele não é mais mostrado.
  return useMemo(() => {
    const known: Record<string, Profile> = {};
    const photos: Record<string, string> = {};
    const resolving: Flags = {};
    for (const id of key ? key.split('|') : []) {
      const profile = profiles[id];
      if (profile) known[id] = profile;
      else if (profile === undefined) resolving[id] = true;
      if (avatars[id]) photos[id] = avatars[id];
    }
    return { profiles: known, avatars: photos, resolving };
  }, [profiles, avatars, key]);
}

export type RoomPresence = { members: string[]; streams: Broadcasts };

// Presença de cada canal visível na barra lateral. Escuta apenas members e streams (nunca a sala
// inteira, que incluiria a sinalização). Canais sem permissão são recusados pelas regras e ficam vazios.
export function useRoomsPresence(channelIds: string[]) {
  const [presence, setPresence] = useState<Record<string, RoomPresence>>({});
  const key = [...channelIds].sort().join('|');
  useEffect(() => {
    const ids = key ? key.split('|') : [];
    const patch = (id: string, field: keyof RoomPresence, value: unknown) =>
      setPresence((prev) => ({
        ...prev,
        [id]: { ...(prev[id] ?? { members: [], streams: {} }), [field]: value },
      }));
    const unsubs = ids.flatMap((id) => [
      onValue(
        ref(db, `rooms/${id}/members`),
        (s) => patch(id, 'members', Object.keys(s.val() || {})),
        () => patch(id, 'members', []),
      ),
      onValue(
        ref(db, `rooms/${id}/streams`),
        (s) => patch(id, 'streams', parseBroadcasts(s.val())),
        () => patch(id, 'streams', {}),
      ),
    ]);
    return () => unsubs.forEach((u) => u());
  }, [key]);
  return presence;
}
