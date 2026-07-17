import { type ClassValue, clsx } from 'clsx';
import { twMerge } from 'tailwind-merge';

/**
 * Merge class names with Tailwind conflict resolution
 *
 * Combines clsx for conditional classes with tailwind-merge
 * to handle Tailwind class conflicts intelligently.
 *
 * @example
 * cn("px-4 py-2", isActive && "bg-primary", className)
 * cn("px-4", "px-8") // => "px-8" (conflict resolved)
 */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
