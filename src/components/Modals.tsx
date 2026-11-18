import { useState } from 'react';
import { desktop } from '../desktop';
import { playSound, setSoundsEnabled, soundsEnabled } from '../lib/sounds';
import { LoaderCircle } from 'lucide-react';
import { Modal } from '../ui/Modal';
import { nicknameError, NICKNAME_MAX } from '../community';
import { supportsOwnAudioRestriction, type CaptureSettings } from '../live/capture';

function SoundsOption() {
  const [on, setOn] = useState(soundsEnabled);
  const choose = (value: boolean) => {
    setSoundsEnabled(value);
    setOn(value);
    if (value) playSound('join');
  };
  return (
    <div className="option-group">
      <span className="option-label">Avisos sonoros</span>
      <div className="segmented wide">
        <button className={on ? 'on' : ''} onClick={() => choose(true)}>
          Ligados
        </button>
        <button className={on ? '' : 'on'} onClick={() => choose(false)}>
          Desligados
        </button>
      </div>
      <small className="muted">Quando alguém entra ou sai do canal e quando uma live começa.</small>
    </div>
  );
}

export function StreamSettingsModal({
  settings,
  live,
  onChange,
  onClose,
}: {
  settings: CaptureSettings;
  live: boolean;
  onChange: (settings: CaptureSettings) => void;
  onClose: () => void;
}) {
  const set = <K extends keyof CaptureSettings>(key: K, value: CaptureSettings[K]) =>
    onChange({ ...settings, [key]: value });
  return (
    <Modal
      title="Configurações da transmissão"
      kicker="LIVE"
      onClose={onClose}
      footer={
        <button className="button primary" onClick={onClose}>
          Concluído
        </button>
      }
    >
      {live && <p className="notice">As mudanças valem para a próxima transmissão.</p>}
      <div className="option-group">
        <span className="option-label">O que compartilhar</span>
        <div className="segmented wide">
          <button
            className={settings.surface === 'monitor' ? 'on' : ''}
            onClick={() => set('surface', 'monitor')}
          >
            Tela inteira
          </button>
          <button
            className={settings.surface === 'window' ? 'on' : ''}
            onClick={() => set('surface', 'window')}
          >
            Janela
          </button>
        </div>
        <small className="muted">
          Para jogos em tela cheia, prefira Tela inteira ou o modo janela sem bordas.
        </small>
      </div>
      <div className="option-row">
        <div className="option-group">
          <span className="option-label">Resolução</span>
          <div className="segmented wide">
            {([720, 1080] as const).map((q) => (
              <button
                key={q}
                className={settings.quality === q ? 'on' : ''}
                onClick={() => set('quality', q)}
              >
                {q}p
              </button>
            ))}
          </div>
        </div>
        <div className="option-group">
          <span className="option-label">Quadros por segundo</span>
          <div className="segmented wide">
            {([30, 60] as const).map((f) => (
              <button
                key={f}
                className={settings.frameRate === f ? 'on' : ''}
                onClick={() => set('frameRate', f)}
              >
                {f} FPS
              </button>
            ))}
          </div>
        </div>
      </div>
      <div className="option-group">
        <span className="option-label">Áudio</span>
        <div className="segmented wide">
          <button className={settings.audio === 'window' ? 'on' : ''} onClick={() => set('audio', 'window')}>
            {desktop ? 'Jogo ou janela' : 'Só a janela'}
          </button>
          <button className={settings.audio === 'system' ? 'on' : ''} onClick={() => set('audio', 'system')}>
            Computador
          </button>
          <button className={settings.audio === 'none' ? 'on' : ''} onClick={() => set('audio', 'none')}>
            Sem áudio
          </button>
        </div>
        <small className="muted">
          {settings.audio === 'system'
            ? supportsOwnAudioRestriction()
              ? 'Todo o som do computador vai para a live, inclusive as vozes de outros apps (ex.: Discord): quem está na chamada com você vai se ouvir. O som das lives que você assiste no Nimbo não é reenviado.'
              : 'Seu navegador pode reenviar o som das lives que você assiste. Use fones ou silencie as outras lives.'
            : settings.audio === 'window'
              ? desktop
                ? 'Só o som do jogo ou programa escolhido vai para a live; as vozes do Discord ficam de fora. Ao transmitir a tela inteira, vai o som do computador. Se o jogo ficar sem som, use "Computador".'
                : 'Envia apenas o som da janela escolhida, quando o navegador permite.'
              : 'Somente vídeo.'}
        </small>
      </div>
      <SoundsOption />
      <small className="muted">
        Cada pessoa assistindo usa parte do seu upload. Com muitos espectadores, 720p ou 30 FPS deixam a live
        mais estável.
      </small>
    </Modal>
  );
}

export function NicknameForm({
  initial,
  submitLabel,
  onSubmit,
}: {
  initial: string;
  submitLabel: string;
  onSubmit: (name: string) => Promise<void>;
}) {
  const [value, setValue] = useState(initial);
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  return (
    <form
      className="nickname-form"
      onSubmit={(e) => {
        e.preventDefault();
        const invalid = nicknameError(value);
        if (invalid) return setError(invalid);
        setSaving(true);
        void onSubmit(value.trim())
          .catch((err) => setError(err instanceof Error ? err.message : String(err)))
          .finally(() => setSaving(false));
      }}
    >
      <input
        autoFocus
        maxLength={NICKNAME_MAX}
        aria-label="Seu nickname"
        placeholder="Como seus amigos vão te chamar?"
        value={value}
        onChange={(e) => {
          setValue(e.target.value);
          setError('');
        }}
      />
      {error && <p className="field-error">{error}</p>}
      <button className="button primary" type="submit" disabled={saving}>
        {saving && <LoaderCircle size={16} className="spin" />}
        {submitLabel}
      </button>
    </form>
  );
}
