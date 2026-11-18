import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { Check } from 'lucide-react';

// Menu do botão direito, no estilo do Discord: abre onde o mouse está, fecha com clique fora, Esc,
// rolagem ou a janela perdendo o foco. Os itens são declarados por quem abre.
export type MenuItem =
  | {
      kind?: 'item';
      label: string;
      icon?: ReactNode;
      onSelect: () => void;
      danger?: boolean;
      disabled?: boolean;
      hint?: string;
      // Caixa de marcar (cargos): mostra o estado e o menu continua aberto depois do clique.
      checked?: boolean;
      // Cor da bolinha à esquerda (cor do cargo).
      color?: string;
      keepOpen?: boolean;
    }
  | { kind: 'separator' }
  | { kind: 'label'; label: string }
  | { kind: 'custom'; node: ReactNode };

// Onde o menu abre (posição do mouse). Quem abre guarda o alvo e monta os itens a cada desenho, com
// os dados atuais: marcar um cargo atualiza a caixa sem fechar o menu.
export type MenuAt = { x: number; y: number };
export const menuAt = (event: { clientX: number; clientY: number; preventDefault(): void }): MenuAt => {
  event.preventDefault();
  return { x: event.clientX, y: event.clientY };
};

export function ContextMenu({ x, y, items: list, onClose }: MenuAt & { items: MenuItem[]; onClose: () => void }) {
  const box = useRef<HTMLDivElement>(null);
  const [place, setPlace] = useState({ left: x, top: y, ready: false });

  // Cabe na tela: abre para a esquerda ou para cima quando não há espaço.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const left = x + width + 8 > innerWidth ? Math.max(8, x - width) : x;
    const top = y + height + 8 > innerHeight ? Math.max(8, innerHeight - height - 8) : y;
    setPlace({ left, top, ready: true });
  }, [x, y, list.length]);

  useEffect(() => {
    const away = (e: Event) => {
      if (!box.current?.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    const timer = setTimeout(() => {
      document.addEventListener('mousedown', away, true);
      document.addEventListener('contextmenu', away, true);
    });
    window.addEventListener('keydown', key);
    window.addEventListener('blur', onClose);
    window.addEventListener('resize', onClose);
    document.addEventListener('scroll', away, true);
    return () => {
      clearTimeout(timer);
      document.removeEventListener('mousedown', away, true);
      document.removeEventListener('contextmenu', away, true);
      window.removeEventListener('keydown', key);
      window.removeEventListener('blur', onClose);
      window.removeEventListener('resize', onClose);
      document.removeEventListener('scroll', away, true);
    };
  }, [onClose]);

  // Primeiro item clicável recebe o foco: dá para usar o teclado (setas, Enter).
  useEffect(() => {
    box.current?.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus({ preventScroll: true });
  }, []);
  const move = (e: React.KeyboardEvent) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    const buttons = [...(box.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') || [])];
    const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const next = e.key === 'ArrowDown' ? (at + 1) % buttons.length : (at - 1 + buttons.length) % buttons.length;
    buttons[next]?.focus();
  };

  return (
    <div
      ref={box}
      className="context-menu"
      role="menu"
      style={{ left: place.left, top: place.top, visibility: place.ready ? 'visible' : 'hidden' }}
      onContextMenu={(e) => e.preventDefault()}
      onKeyDown={move}
    >
      {list.map((item, i) => {
        if (item.kind === 'separator') return <div key={i} className="menu-separator" role="separator" />;
        if (item.kind === 'label')
          return (
            <div key={i} className="menu-label">
              {item.label}
            </div>
          );
        if (item.kind === 'custom') return <div key={i}>{item.node}</div>;
        const checkable = item.checked !== undefined;
        return (
          <button
            key={i}
            role={checkable ? 'menuitemcheckbox' : 'menuitem'}
            aria-checked={checkable ? item.checked : undefined}
            className={'menu-item' + (item.danger ? ' danger' : '')}
            disabled={item.disabled}
            title={item.hint}
            style={item.color ? ({ '--role': item.color } as React.CSSProperties) : undefined}
            onClick={() => {
              item.onSelect();
              if (!item.keepOpen && !checkable) onClose();
            }}
          >
            {item.color && <span className="role-dot" />}
            {item.icon && <span className="menu-icon">{item.icon}</span>}
            <span className="menu-text">{item.label}</span>
            {checkable && <span className={'menu-check' + (item.checked ? ' on' : '')}>{item.checked && <Check size={13} />}</span>}
          </button>
        );
      })}
    </div>
  );
}
