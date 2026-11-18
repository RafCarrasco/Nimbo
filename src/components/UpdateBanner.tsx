import { useEffect, useState } from 'react';
import { Download, RotateCw, Sparkles, X } from 'lucide-react';
import { desktop } from '../desktop';
import { APP_VERSION, SITE_VERSION, siteUpdateAvailable } from '../lib/version';
import { CHANGELOG, newer } from '../lib/changelog';
import { useEmulators } from '../firebase';
import { Modal } from '../ui/Modal';

const CHECK_EVERY = 5 * 60 * 1000;
type Started = { ok: true; value: boolean } | { ok: false; error: string } | void | undefined;
// `installUpdate` responde `false` quando não há nada baixado para instalar.
const started = (r: Started) => !r || (r.ok && r.value !== false);
// App antigo, feito em Electron (o app novo, em Rust, não leva "Electron" no user agent).
const LEGACY_APP = /Electron/i.test(navigator.userAgent);
const SEEN_KEY = 'nimbo.seenVersion';
// Gravado ao clicar em atualizar o app: de qual versão para qual. Ao reabrir, mostra as novidades
// da versão nova ou, se o app voltou na mesma versão, avisa que a instalação falhou.
const PENDING_KEY = 'nimbo.pendingUpdate';
type Pending = { from: string; to: string; at: number };
const readPending = (): Pending | null => {
  try {
    const raw = localStorage.getItem(PENDING_KEY);
    localStorage.removeItem(PENDING_KEY);
    return raw ? (JSON.parse(raw) as Pending) : null;
  } catch {
    return null;
  }
};
const writePending = (p: Pending) => {
  try {
    localStorage.setItem(PENDING_KEY, JSON.stringify(p));
  } catch {
    // Sem armazenamento: segue sem as novidades ao reabrir.
  }
};
// Lido uma vez por carregamento da página.
const PENDING = readPending();
const pendingResult = () => {
  if (!PENDING || !APP_VERSION || Date.now() - PENDING.at > 30 * 60 * 1000) return null;
  if (APP_VERSION === PENDING.from) return 'failed' as const;
  return 'installed' as const;
};

const readSeen = () => {
  try {
    return localStorage.getItem(SEEN_KEY);
  } catch {
    return null;
  }
};
const writeSeen = (v: string) => {
  try {
    localStorage.setItem(SEEN_KEY, v);
  } catch {
    // Sem armazenamento: as novidades podem reaparecer, sem outro efeito.
  }
};

// Versão nova do app (o próprio app baixa e avisa) ou do site (comparando o bundle publicado).
// Primeiro aparece um cartão; depois de "Depois" fica só o selo verde no topo, até atualizar.
// A pessoa decide quando, porque atualizar derruba a live por alguns segundos.
export function UpdateBanner({ live }: { live: boolean }) {
  const [appVersion, setAppVersion] = useState('');
  const [site, setSite] = useState(false);
  const [cardClosed, setCardClosed] = useState(false);
  const [busy, setBusy] = useState(false);
  // App instalado mais velho que o site e sem atualização chegando: o portátil e os apps antigos
  // não se atualizam sozinhos, então o jeito é baixar o instalador de novo.
  const [outdated, setOutdated] = useState('');
  // O aviso de instalação que falhou disparava errado quando a pessoa reabria o app antes de o
  // instalador terminar; fica desligado (o próprio app oferece a atualização de novo).
  const failed = false;
  const [progress, setProgress] = useState(0);
  const [updating, setUpdating] = useState('');
  // A atualização não começou (o app continuou aberto ou a página não recarregou): o que fazer.
  const [problem, setProblem] = useState('');
  // A tela "Atualizando" está baixando a versão nova (antes de instalar).
  const [downloading, setDownloading] = useState(false);
  // App em Rust mais velho que o site e sem atualização chegando: o app só procura atualização ao
  // abrir e de tempos em tempos, então quem está com ele aberto há horas precisa fechar e abrir.
  const [stale, setStale] = useState(false);
  const [notes, setNotes] = useState(() => {
    if (pendingResult() === 'installed' && PENDING) {
      writeSeen(SITE_VERSION);
      return CHANGELOG.filter((r) => newer(r.version, PENDING.from) && !newer(r.version, APP_VERSION));
    }
    let seen = readSeen();
    if (!seen) {
      // Quem já usava o Nimbo antes desta janela existir (0.3.3) vê as novidades desta versão;
      // quem chega agora não recebe novidades antigas.
      let returning = false;
      try {
        returning = Object.keys(localStorage).some((k) => k.startsWith('nimbo.'));
      } catch {
        // Sem armazenamento.
      }
      if (!returning) {
        writeSeen(SITE_VERSION);
        return [];
      }
      seen = '0.3.3';
    }
    return CHANGELOG.filter((r) => newer(r.version, seen) && !newer(r.version, SITE_VERSION));
  });

  useEffect(() => {
    // Só com emuladores: ?fakeUpdate=app|site|notes mostra cada aviso para conferir o visual.
    const fake = useEmulators ? new URLSearchParams(location.search).get('fakeUpdate') : null;
    if (fake === 'app') setAppVersion('9.9.9');
    if (fake === 'site') setSite(true);
    if (fake === 'notes') setNotes(CHANGELOG.slice(0, 2));
    if (fake === 'outdated') setOutdated('0.2.0');
    if (fake === 'progress') setProgress(45);
    if (fake === 'stale') setStale(true);
    if (fake === 'problem') {
      setAppVersion('9.9.9');
      setProblem('A atualização não começou. Feche e abra o Nimbo: ela instala ao abrir.');
    }
    const offProgress = desktop?.onUpdateProgress?.(setProgress);
    if (!desktop?.watchUpdate) return offProgress;
    void desktop.watchUpdate((info) => setAppVersion(info.version)).then((r) => {
      if (r.ok && r.value) setAppVersion(r.value.version);
    });
    return offProgress;
  }, []);

  useEffect(() => {
    if (desktop?.engine !== 'rust' || !APP_VERSION || !newer(SITE_VERSION, APP_VERSION)) return;
    let stopped = false;
    // App 0.5.7+: pede a atualização dele agora, em vez de esperar a rotina de 30 minutos.
    void desktop.checkUpdate?.().then((r) => {
      if (!stopped && r.ok && r.value) setAppVersion(r.value.version);
    });
    // Um tempo para a atualização do próprio app chegar: se chegar, o aviso dela basta.
    const timer = setTimeout(() => setStale(true), 20 * 1000);
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  }, []);

  useEffect(() => {
    // O app antigo (Electron) não recebe mais atualização: a partir da 0.5.0 o Nimbo é outro app,
    // e quem ainda está no antigo é convidado a trocar. O instalador novo remove o antigo.
    if (LEGACY_APP) setOutdated(APP_VERSION || 'antiga');
  }, []);

  useEffect(() => {
    let stopped = false;
    let waiting: ReturnType<typeof setTimeout> | undefined;
    const check = () =>
      void siteUpdateAvailable()
        .then((available) => {
          if (stopped || !available) return;
          // No app, um site novo quase sempre vem junto com um app novo. O app 0.5.7+ confere na hora:
          // se houver versão dele, o aviso é o de reiniciar o app (o site novo dentro do app velho
          // deixava coisas sem funcionar até fechar e abrir). App mais antigo: espera 2 minutos a
          // atualização dele chegar sozinha e, sem ela, mostra a do site.
          const check = desktop?.checkUpdate;
          if (check)
            void check().then((r) => {
              if (stopped) return;
              if (r.ok && r.value) setAppVersion(r.value.version);
              else setSite(true);
            });
          else if (desktop?.watchUpdate) waiting ??= setTimeout(() => setSite(true), 2 * 60 * 1000);
          else setSite(true);
        })
        .catch(() => {});
    const timer = setInterval(check, CHECK_EVERY);
    const onVisible = () => document.visibilityState === 'visible' && check();
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      stopped = true;
      clearInterval(timer);
      if (waiting) clearTimeout(waiting);
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, []);

  const closeNotes = () => {
    writeSeen(SITE_VERSION);
    setNotes([]);
  };
  const isApp = Boolean(appVersion);
  // A atualização não começou: tira a tela "Atualizando" e diz o que fazer, em vez de ficar parado.
  const gaveUp = (message: string) => {
    try {
      localStorage.removeItem(PENDING_KEY);
    } catch {
      // Sem armazenamento.
    }
    setUpdating('');
    setDownloading(false);
    setBusy(false);
    setCardClosed(false);
    setProblem(message);
  };
  // App 0.5.7+ consegue procurar e baixar a versão nova na hora (`checkUpdate`).
  const canFetch = Boolean(desktop?.checkUpdate && desktop.installUpdate);
  // Um clique faz tudo: se a versão nova já está baixada, instala; se não, baixa agora (com a barra
  // na tela "Atualizando") e instala em seguida. O app fecha e abre sozinho na versão nova. Antes,
  // o aviso de app desatualizado pedia para fechar e abrir, e reabrir só baixava: era preciso
  // reiniciar mais uma vez para instalar (relatado em 17/11/2026).
  const installApp = async () => {
    // Um instante para a tela "Atualizando" aparecer antes de o app fechar.
    await new Promise((done) => setTimeout(done, 700));
    let result: Started = await desktop?.installUpdate?.();
    if (!started(result) && desktop?.checkUpdate) {
      setDownloading(true);
      const found = await desktop.checkUpdate();
      setDownloading(false);
      if (!found.ok) return gaveUp(found.error);
      if (!found.value)
        return gaveUp('Não achei a versão nova para baixar agora. Feche e abra o Nimbo para atualizar.');
      setUpdating(found.value.version);
      result = await desktop.installUpdate?.();
    }
    if (!started(result))
      return gaveUp(
        result && !result.ok
          ? `Não deu para instalar: ${result.error}`
          : 'A versão nova ainda está baixando. Tente de novo em instantes.',
      );
    // O instalador fecha o app em poucos segundos; se depois de 40 s ele continua aberto, não começou.
    setTimeout(() => gaveUp('A atualização não começou. Feche e abra o Nimbo para atualizar.'), 40000);
  };
  const update = () => {
    setBusy(true);
    setProblem('');
    if (!isApp && !stale) {
      location.reload();
      // Recarregar some com esta página; se ela continua aqui depois de 10 s, não recarregou.
      setTimeout(() => gaveUp('A página não recarregou. Feche e abra o Nimbo para atualizar.'), 10000);
      return;
    }
    const target = appVersion || SITE_VERSION;
    writePending({ from: APP_VERSION, to: target, at: Date.now() });
    setUpdating(target);
    installApp().catch((e: unknown) =>
      gaveUp(`Não deu para instalar: ${e instanceof Error ? e.message : String(e)}`),
    );
  };

  return (
    <>
      {updating && (
        <div className="updating-screen" role="status">
          <img src="/nimbo-icon-256.png" alt="" />
          <strong>Atualizando o Nimbo</strong>
          <span>
            {downloading
              ? `Baixando a versão nova${progress > 0 && progress < 100 ? ` (${progress}%)` : ''}. Depois o app fecha e abre sozinho.`
              : `Versão ${updating}. O app fecha e abre sozinho em alguns segundos.`}
          </span>
          <div className={downloading && progress > 0 && progress < 100 ? 'updating-bar filled' : 'updating-bar'}>
            <i style={downloading && progress > 0 && progress < 100 ? { width: `${progress}%` } : undefined} />
          </div>
        </div>
      )}
      {!isApp && progress > 0 && progress < 100 && (
        <span className="update-pill downloading">
          <Download size={14} />
          Baixando atualização {progress}%
        </span>
      )}
      {outdated && !isApp && !(progress > 0 && progress < 100) && (
        <div className="update-card" role="alert">
          <img src="/nimbo-icon-128.png" alt="" />
          <div className="update-text">
            <strong>O Nimbo tem um app novo</strong>
            <span>
              Mais leve e com imagem melhor nas lives. Baixe e abra o instalador: ele troca este app pelo novo.
              Você entra com o Google mais uma vez.
            </span>
            <div className="update-actions">
              <button
                className="button primary small"
                onClick={() => window.open('https://golive-ad118.web.app/baixar/', '_blank')}
              >
                <Download size={15} />
                Baixar app novo
              </button>
              <button className="button ghost small" onClick={() => setOutdated('')}>
                Depois
              </button>
            </div>
          </div>
        </div>
      )}
      {stale && !isApp && !(progress > 0 && progress < 100) && (
        <div className="update-card" role="alert">
          <img src="/nimbo-icon-128.png" alt="" />
          <div className="update-text">
            <strong>Seu Nimbo está desatualizado</strong>
            <span>
              Você está na {APP_VERSION || 'versão antiga'} e a {SITE_VERSION} já saiu.
              {canFetch ? ' Atualize agora: o app baixa, instala e abre de novo sozinho.' : ' Feche o Nimbo e abra de novo para atualizar.'}
              {canFetch && live && ' Sua live cai por alguns segundos.'}
            </span>
            {problem && <span className="update-problem">{problem}</span>}
            <div className="update-actions">
              {canFetch && (
                <button className="button primary small" disabled={busy} onClick={update}>
                  <Download size={15} />
                  Atualizar agora
                </button>
              )}
              <button className="button ghost small" onClick={() => setStale(false)}>
                {canFetch ? 'Depois' : 'Entendi'}
              </button>
            </div>
          </div>
        </div>
      )}
      {(isApp || (site && !(progress > 0 && progress < 100))) && cardClosed && (
        <button
          className="update-pill"
          disabled={busy}
          title={live ? 'Sua live cai por alguns segundos ao atualizar' : undefined}
          onClick={update}
        >
          <Download size={14} />
          Atualização disponível
        </button>
      )}
      {(isApp || (site && !(progress > 0 && progress < 100))) && !cardClosed && (
        <div className="update-card" role="status">
          <img src="/nimbo-icon-128.png" alt="" />
          <div className="update-text">
            <strong>{isApp ? `Nimbo ${appVersion} está pronto` : 'Nova versão do Nimbo'}</strong>
            <span>
              {isApp ? 'O app instala e abre de novo sozinho.' : 'Atualize para ter as novidades.'}
              {live && ' Sua live cai por alguns segundos.'}
            </span>
            {problem && <span className="update-problem">{problem}</span>}
            <div className="update-actions">
              <button className="button primary small" disabled={busy} onClick={update}>
                {isApp ? <Download size={15} /> : <RotateCw size={15} />}
                {isApp ? 'Atualizar agora' : 'Atualizar'}
              </button>
              <button className="button ghost small" onClick={() => setCardClosed(true)}>
                Depois
              </button>
            </div>
          </div>
          <button className="icon-button tiny" aria-label="Fechar aviso" onClick={() => setCardClosed(true)}>
            <X size={14} />
          </button>
        </div>
      )}
      {notes.length > 0 && (
        <Modal
          title={`Novidades do Nimbo ${notes[0].version}`}
          kicker="ATUALIZADO"
          size="sm"
          onClose={closeNotes}
          footer={
            <button className="button primary" onClick={closeNotes}>
              Bora
            </button>
          }
        >
          <div className="release-notes">
            {notes.map((release) => (
              <section key={release.version}>
                {notes.length > 1 && (
                  <h3>
                    {release.version} <small className="muted">{release.date}</small>
                  </h3>
                )}
                <ul>
                  {release.items.map((item) => (
                    <li key={item}>
                      <Sparkles size={14} aria-hidden />
                      <span>{item}</span>
                    </li>
                  ))}
                </ul>
              </section>
            ))}
          </div>
        </Modal>
      )}
    </>
  );
}
