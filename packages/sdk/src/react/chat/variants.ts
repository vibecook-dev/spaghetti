/**
 * Chat CVA Variant Definitions
 *
 * Class Variance Authority definitions for consistent chat component styling.
 */

import { cva, type VariantProps } from 'class-variance-authority';

// =============================================================================
// CHAT CARD
// =============================================================================

export const chatCardVariants = cva('border border-border rounded-lg overflow-hidden transition-all duration-200', {
  variants: {
    variant: {
      default: 'bg-background chat-card-shadow',
      expanded: 'bg-background shadow-md',
      error: 'border-destructive/30 bg-destructive/5',
      agent: 'border-sky-500/20 bg-sky-500/5',
      tinted: '', // color set via inline style
    },
  },
  defaultVariants: { variant: 'default' },
});

export type ChatCardVariants = VariantProps<typeof chatCardVariants>;

// =============================================================================
// CHAT CARD HEADER
// =============================================================================

export const chatCardHeaderVariants = cva('flex items-center gap-2 px-3 py-2 border-b border-border', {
  variants: {
    variant: {
      default: 'bg-card',
      tinted: '', // color set via inline style
    },
  },
  defaultVariants: { variant: 'default' },
});

export type ChatCardHeaderVariants = VariantProps<typeof chatCardHeaderVariants>;

// =============================================================================
// BADGE
// =============================================================================

export const badgeVariants = cva('inline-flex items-center px-1.5 py-0.5 rounded text-[9px] font-mono', {
  variants: {
    variant: {
      default: 'bg-card border border-border text-muted-foreground',
      colored: '', // color via inline style
      success: 'bg-emerald-500/10 text-emerald-500 border border-emerald-500/30',
      error: 'bg-red-500/10 text-red-500 border border-red-500/30',
      warning: 'bg-amber-500/10 text-amber-500 border border-amber-500/30',
    },
  },
  defaultVariants: { variant: 'default' },
});

export type BadgeVariants = VariantProps<typeof badgeVariants>;

// =============================================================================
// TIMELINE HEADER
// =============================================================================

export const timelineHeaderVariants = cva('flex items-center gap-2 cursor-pointer select-none group/header', {
  variants: {
    spacing: {
      default: '',
      compact: 'py-0.5',
    },
  },
  defaultVariants: { spacing: 'default' },
});

export type TimelineHeaderVariants = VariantProps<typeof timelineHeaderVariants>;
