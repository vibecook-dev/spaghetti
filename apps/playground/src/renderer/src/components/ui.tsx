/**
 * Shared playground UI primitives — monochrome, quiet, matches LoadingScreen.
 */

import type { ButtonHTMLAttributes, CSSProperties, ReactNode } from 'react';

export function Spinner({ className = '' }: { className?: string }) {
  return (
    <div
      className={`animate-spin rounded-full h-4 w-4 border-2 border-white/20 border-t-orange-400 ${className}`}
      aria-hidden
    />
  );
}

export function EmptyState({ title, detail, action }: { title: string; detail?: string; action?: ReactNode }) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 py-12 px-6 text-center">
      <p className="text-sm text-white/45">{title}</p>
      {detail ? <p className="text-[11px] text-white/30 max-w-xs leading-relaxed">{detail}</p> : null}
      {action ? <div className="mt-2">{action}</div> : null}
    </div>
  );
}

export function Dot() {
  return <span className="opacity-35"> · </span>;
}

type BtnVariant = 'ghost' | 'solid' | 'danger';

const BTN: Record<BtnVariant, string> = {
  ghost: 'bg-transparent border-white/14 text-white/75 hover:bg-white/5 hover:text-white/90 hover:border-white/22',
  solid: 'bg-white/10 border-white/16 text-white/90 hover:bg-white/14',
  danger: 'bg-red-500/10 border-red-500/25 text-red-200/90 hover:bg-red-500/15',
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
      className={`inline-flex items-center justify-center gap-1.5 text-[11px] px-2.5 py-1 rounded border cursor-pointer disabled:opacity-40 disabled:cursor-not-allowed transition-colors ${BTN[variant]} ${className}`}
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
      className={`inline-flex items-center text-[10px] px-2 py-0.5 rounded border font-mono tracking-wide transition-colors ${
        active
          ? 'bg-white/12 border-white/25 text-white/90'
          : 'bg-transparent border-white/10 text-white/45 hover:text-white/70 hover:border-white/18'
      } ${onClick ? 'cursor-pointer' : ''} ${className}`}
    >
      {children}
    </Tag>
  );
}

export function SectionLabel({ children, trailing }: { children: ReactNode; trailing?: ReactNode }) {
  return (
    <div className="px-3.5 py-2 text-[10px] tracking-[0.12em] uppercase text-white/35 border-b border-white/5 flex items-center gap-2 shrink-0">
      <span className="flex-1 min-w-0 truncate">{children}</span>
      {trailing}
    </div>
  );
}

export function LiveDot({ active, label = 'Live' }: { active: boolean; label?: string }) {
  return (
    <span
      className={`inline-flex items-center gap-1.5 text-[10px] font-mono tracking-wide ${
        active ? 'text-emerald-300/80' : 'text-white/30'
      }`}
      title={active ? 'Receiving live index updates' : 'Waiting for changes'}
    >
      <span
        className={`inline-block h-1.5 w-1.5 rounded-full ${active ? 'bg-emerald-400 animate-pulse' : 'bg-white/25'}`}
        aria-hidden
      />
      {label}
    </span>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="inline-flex items-center px-1 py-0.5 rounded border border-white/12 bg-white/[0.04] text-[9px] font-mono text-white/40 leading-none">
      {children}
    </kbd>
  );
}

/** Soft panel surface used by overlays / drawers */
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
    <div className={`bg-[#0c0c0c] border border-white/10 rounded-md shadow-2xl ${className}`} style={style}>
      {children}
    </div>
  );
}
