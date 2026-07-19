/**
 * Shared playground UI primitives — archive / paper design language.
 */

import type { ButtonHTMLAttributes, CSSProperties, ReactNode } from 'react';

export function Spinner({ className = '' }: { className?: string }) {
  return (
    <div
      className={`animate-spin rounded-full h-4 w-4 border border-ink/20 border-t-sanguine ${className}`}
      aria-hidden
    />
  );
}

export function EmptyState({ title, detail, action }: { title: string; detail?: string; action?: ReactNode }) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 py-12 px-6 text-center">
      {/* Design empty prose: serif ~14px; mono caption for detail */}
      <p className="font-serif text-[14px] leading-relaxed text-ink/50">{title}</p>
      {detail ? (
        <p className="font-mono text-[10px] tracking-[0.08em] uppercase text-ink/35 max-w-xs leading-relaxed">
          {detail}
        </p>
      ) : null}
      {action ? <div className="mt-2">{action}</div> : null}
    </div>
  );
}

export function Dot() {
  return <span className="opacity-40"> · </span>;
}

type BtnVariant = 'ghost' | 'solid' | 'danger';

const BTN: Record<BtnVariant, string> = {
  ghost:
    'bg-transparent border-[color:var(--archive-ink-line-mid)] text-ink/75 hover:border-[color:var(--archive-ink-line)] hover:text-ink',
  solid: 'bg-ink text-paper border-transparent hover:opacity-90 dark:bg-ink dark:text-[#11100f]',
  danger: 'bg-transparent border-sanguine/25 text-sanguine hover:bg-sanguine/10',
};

export function Btn({
  variant = 'ghost',
  className = '',
  children,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: BtnVariant }) {
  return (
    <button
      type="button"
      className={`inline-flex items-center justify-center gap-1.5 font-mono text-[9px] uppercase tracking-widest px-2.5 py-1 border cursor-pointer disabled:opacity-30 disabled:cursor-not-allowed transition-colors rounded-none ${BTN[variant]} ${className}`}
      {...rest}
    >
      {children}
    </button>
  );
}

export function Chip({
  children,
  active,
  onClick,
  title,
  className = '',
}: {
  children: ReactNode;
  active?: boolean;
  onClick?: () => void;
  title?: string;
  className?: string;
}) {
  const Tag = onClick ? 'button' : 'span';
  return (
    <Tag
      type={onClick ? 'button' : undefined}
      title={title}
      onClick={onClick}
      className={`inline-flex items-center font-mono text-[9px] px-1 py-0.5 tracking-[0.04em] uppercase transition-colors border-b ${
        active
          ? 'border-[color:var(--archive-ink-line-header)] text-ink opacity-100'
          : 'border-[color:var(--archive-ink-line-soft)] text-ink/45 hover:text-ink/70 hover:border-[color:var(--archive-ink-line-mid)]'
      } ${onClick ? 'cursor-pointer bg-transparent' : ''} ${className}`}
    >
      {children}
    </Tag>
  );
}

export function SectionLabel({ children, trailing }: { children: ReactNode; trailing?: ReactNode }) {
  return (
    <div className="h-10 px-4 border-b border-[color:var(--archive-ink-line-soft)] flex items-center gap-2 shrink-0">
      <span className="flex-1 min-w-0 truncate font-serif text-[10px] uppercase tracking-[0.15em] opacity-80">
        {children}
      </span>
      {trailing}
    </div>
  );
}

export function LiveDot({ active, label = 'Live' }: { active: boolean; label?: string }) {
  return (
    <span
      className={`inline-flex items-center gap-1.5 font-mono text-[9px] tracking-[0.1em] ${
        active ? 'text-[color:var(--archive-live)]' : 'text-ink/30'
      }`}
      title={active ? 'Receiving live index updates' : 'Waiting for changes'}
    >
      <span className={`inline-block h-1.5 w-1.5 ${active ? 'bg-current animate-pulse' : 'bg-ink/25'}`} aria-hidden />
      {label}
    </span>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="inline-flex items-center px-1 py-0.5 border border-[color:var(--archive-ink-line-soft)] font-mono text-[8px] text-ink/40 leading-none tracking-normal normal-case">
      {children}
    </kbd>
  );
}

export function PanelShell({
  children,
  className = '',
  style,
}: {
  children: ReactNode;
  className?: string;
  style?: CSSProperties;
}) {
  return (
    <div className={`border border-[color:var(--archive-ink-line)] bg-paper shadow-2xl ${className}`} style={style}>
      {children}
    </div>
  );
}
