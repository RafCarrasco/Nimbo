import { useEffect, useState } from 'react';
import { Eye } from 'lucide-react';

// "12:34" ou "1:02:03".
export function formatElapsed(ms: number) {
  const total = Math.max(0, Math.floor(ms / 1000));
  const two = (n: number) => String(n).padStart(2, '0');
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  return hours ? `${hours}:${two(minutes)}:${two(total % 60)}` : `${minutes}:${two(total % 60)}`;
}

// Há quanto tempo a live está no ar. `startedAt` é na hora do servidor; `clockOffset` acerta o relógio
// deste aparelho com o do servidor.
export function Elapsed({ startedAt, clockOffset = 0 }: { startedAt?: number; clockOffset?: number }) {
  const [, tick] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(timer);
  }, []);
  if (!startedAt) return null;
  return (
    <span className="live-elapsed" title="Tempo no ar">
      {formatElapsed(Date.now() + clockOffset - startedAt)}
    </span>
  );
}

// Quantas pessoas estão assistindo, com os nomes ao passar o mouse.
export function Viewers({ names }: { names: string[] }) {
  if (!names.length) return null;
  return (
    <span className="live-viewers" title={'Assistindo: ' + names.join(', ')}>
      <Eye size={13} aria-hidden /> {names.length}
    </span>
  );
}
